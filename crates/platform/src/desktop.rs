//! Event-driven desktop integration. Never polls the cursor while idle.
use anyhow::{Result, ensure};
use easy_switch_core::follow::{DesktopState, EdgeLatch, Point, Rule, Segment};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

#[cfg(windows)]
#[path = "desktop_windows.rs"]
mod native;
#[cfg(target_os = "linux")]
#[path = "desktop_wayland.rs"]
mod wayland;
#[cfg(target_os = "linux")]
#[path = "desktop_x11.rs"]
mod x11;

#[derive(Debug)]
pub enum Event {
    State(DesktopState),
    Cross {
        rule: String,
        fraction: f64,
        generation: u64,
        at: Instant,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use easy_switch_core::follow::{Display, Edge};
    fn fixture() -> (Model, crossbeam_channel::Receiver<Event>, Segment) {
        let (tx, rx) = crossbeam_channel::bounded(16);
        let mut model = Model::new("simulation", tx);
        model.enabled = true;
        model.state.ready = true;
        model.state.displays = vec![Display {
            id: "screen".into(),
            name: "screen".into(),
            x: -1000.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
            scale: 1.0,
            binding: "layout1".into(),
        }];
        model.state.pointer = Point {
            x: -500.0,
            y: 400.0,
        };
        let s = Segment {
            display: "screen".into(),
            binding: "layout1".into(),
            ..Default::default()
        };
        model.rules = vec![Rule {
            id: "edge".into(),
            enabled: true,
            source: s.clone(),
            destination: s.clone(),
            peer: "peer".into(),
            computer: "other".into(),
            group: "desk".into(),
            mouse: "mouse".into(),
        }];
        (model, rx, s)
    }
    #[test]
    fn synthetic_buttons_paused_and_locked_do_not_cross() {
        for mode in 0..4 {
            let (mut m, rx, _) = fixture();
            if mode == 2 {
                m.enabled = false;
            }
            if mode == 3 {
                m.state.locked = true;
            }
            m.movement(
                Point { x: -1.0, y: 400.0 },
                Point { x: 5.0, y: 0.0 },
                mode == 1,
                mode == 0,
            );
            assert!(rx.is_empty());
        }
    }
    #[test]
    fn single_crossing_then_bounce_is_suppressed() {
        let (mut m, rx, _) = fixture();
        m.movement(
            Point { x: -1.0, y: 400.0 },
            Point { x: 5.0, y: 0.0 },
            false,
            false,
        );
        assert!(matches!(rx.try_recv(),Ok(Event::Cross{fraction,..}) if fraction==0.5));
        m.movement(
            Point { x: -1.0, y: 401.0 },
            Point { x: 5.0, y: 1.0 },
            false,
            false,
        );
        assert!(rx.is_empty());
    }
    #[test]
    fn corner_with_two_destinations_never_guesses() {
        let (mut m, rx, _) = fixture();
        let mut second = m.rules[0].clone();
        second.id = "corner".into();
        second.source.edge = Edge::Top;
        m.rules.push(second);
        m.movement(
            Point { x: -1.0, y: 0.0 },
            Point { x: 5.0, y: -5.0 },
            false,
            false,
        );
        assert!(rx.is_empty());
    }
    #[test]
    fn cancellation_restores_only_without_intervening_input() {
        let (mut m, _, s) = fixture();
        let previous = m.state.pointer;
        let (id, p) = m.placement(&s, 0.5, 0).unwrap();
        m.state.pointer = p;
        assert!(m.placement(&s, 0.5, 0).is_err());
        assert_eq!(m.cancel(id), Some(previous));
        assert_eq!(m.cancel(id), None);
        let (id, p) = m.placement(&s, 0.5, 0).unwrap();
        m.movement(p, Point::default(), false, false);
        assert_eq!(m.cancel(id), None);
    }
    #[test]
    fn stale_topology_permission_and_fraction_fail_closed() {
        let (mut m, _, mut s) = fixture();
        assert!(m.placement(&s, 0.5, 1).is_err());
        assert!(m.placement(&s, f64::NAN, 0).is_err());
        s.binding = "removed".into();
        assert!(m.placement(&s, 0.5, 0).is_err());
        m.state.ready = false;
        assert!(m.placement(&s, 0.5, 0).is_err());
    }
    #[test]
    fn unplugged_display_requires_reverification_even_after_restart() {
        let (m, _, _) = fixture();
        let original = m.state.displays;
        let root = std::env::temp_dir();
        let path = root.join(format!(
            "esl-bindings-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut b = Bindings {
            path: path.clone(),
            ..Default::default()
        };
        let mut displays = original.clone();
        b.verify(&mut displays).unwrap();
        assert_eq!(displays, original);
        b.verify(&mut []).unwrap();
        let mut restored: Bindings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        restored.path = path.clone();
        restored.verify(&mut displays).unwrap();
        assert_ne!(displays[0].binding, original[0].binding);
        let verified = displays.clone();
        let mut restarted: Bindings =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        restarted.path = path.clone();
        let mut fresh = original;
        restarted.verify(&mut fresh).unwrap();
        assert_eq!(fresh, verified);
        assert!(path.starts_with(root));
        std::fs::remove_file(path).unwrap();
    }
}
#[cfg(target_os = "linux")]
#[path = "desktop_session.rs"]
mod session_watch;

pub enum Command {
    Configure {
        enabled: bool,
        rules: Vec<Rule>,
    },
    Authorize,
    #[cfg(target_os = "linux")]
    SessionLocked(bool),
    Place {
        deadline: Instant,
        segment: Segment,
        fraction: f64,
        generation: u64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Cancel(u64),
    Complete(u64),
    Stop,
}
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<Command>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl Handle {
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
    pub fn send(&self, c: Command) -> Result<()> {
        self.tx
            .try_send(c)
            .map_err(|_| anyhow::anyhow!("Desktop command queue unavailable"))?;
        (self.wake)();
        Ok(())
    }
    pub async fn place(&self, segment: Segment, fraction: f64, generation: u64) -> Result<u64> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Place {
            deadline: Instant::now() + Duration::from_millis(350),
            segment,
            fraction,
            generation,
            reply: tx,
        })?;
        rx.await?.map_err(anyhow::Error::msg)
    }
}
pub fn start(
    events: crossbeam_channel::Sender<Event>,
    _private_dir: std::path::PathBuf,
) -> Result<Handle> {
    let (tx, rx) = mpsc::channel(32);
    #[cfg(windows)]
    let wake = native::start(rx, events, _private_dir)?;
    #[cfg(target_os = "linux")]
    let wake = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        wayland::start(rx, events, _private_dir)?
    } else {
        x11::start(rx, events, _private_dir)?
    };
    let handle = Handle { tx, wake };
    #[cfg(target_os = "linux")]
    session_watch::start(handle.clone());
    Ok(handle)
}
struct Reservation {
    id: u64,
    previous: Point,
    serial: u64,
    until: Instant,
}
pub(super) struct Model {
    bindings: Option<Bindings>,
    pub state: DesktopState,
    pub enabled: bool,
    pub rules: Vec<Rule>,
    pub events: crossbeam_channel::Sender<Event>,
    latch: EdgeLatch,
    reservation: Option<Reservation>,
    sequence: u64,
    #[cfg(target_os = "linux")]
    session_locked: bool,
}
impl Model {
    fn new(backend: &str, events: crossbeam_channel::Sender<Event>) -> Self {
        Self {
            bindings: None,
            state: DesktopState {
                backend: backend.into(),
                ..Default::default()
            },
            enabled: false,
            rules: vec![],
            events,
            latch: EdgeLatch::default(),
            reservation: None,
            sequence: 0,
            #[cfg(target_os = "linux")]
            session_locked: true,
        }
    }
    fn publish(&self) {
        let _ = self.events.try_send(Event::State(self.state.clone()));
    }
    fn load_bindings(&mut self, directory: &std::path::Path) -> Result<()> {
        let path = directory.join("display-bindings.json");
        let mut bindings: Bindings = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Bindings::default(),
            Err(e) => return Err(e.into()),
        };
        bindings.path = path;
        self.bindings = Some(bindings);
        Ok(())
    }
    fn topology(&mut self, mut displays: Vec<easy_switch_core::follow::Display>) {
        displays.sort_by(|a, b| a.id.cmp(&b.id));
        if !displays.iter().all(|d| d.valid()) || displays.windows(2).any(|d| d[0].id == d[1].id) {
            self.state.ready = false;
            self.state.reason = "Ambiguous or invalid display identity/geometry".into();
            self.reservation = None;
            self.publish();
            return;
        }
        if let Some(bindings) = &mut self.bindings
            && let Err(e) = bindings.verify(&mut displays)
        {
            self.state.ready = false;
            self.state.reason = format!("Cannot retain display verification: {e:#}");
            self.reservation = None;
            self.publish();
            return;
        }
        self.state.ready = !displays.is_empty();
        self.state.reason = if displays.is_empty() {
            "No displays available".into()
        } else {
            String::new()
        };
        if self.state.displays != displays {
            self.state.generation = self.state.generation.wrapping_add(1);
            self.state.displays = displays;
            self.reservation = None;
            self.publish();
        }
    }
    #[cfg(target_os = "linux")]
    fn unavailable(&mut self, why: impl Into<String>) {
        self.state.ready = false;
        self.state.reason = why.into();
        self.reservation = None;
        self.publish();
    }
    fn movement(&mut self, p: Point, delta: Point, buttons: bool, synthetic: bool) {
        let previous = self.state.pointer;
        self.state.pointer = p;
        if synthetic {
            return;
        }
        self.state.input_serial = self.state.input_serial.wrapping_add(1);
        if !self.enabled
            || !self.state.ready
            || self.state.locked
            || buttons
            || !self.latch.eligible(p, Instant::now())
        {
            return;
        }
        let hits: Vec<_> = self
            .rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                self.state.displays.iter().find_map(|d| {
                    let fraction = if r.source.hit(d, p, delta) {
                        r.source.fraction(d, p)
                    } else {
                        r.source.crossing(d, previous, p)
                    };
                    fraction.map(|f| (r, d, f))
                })
            })
            .collect();
        if let [(rule, d, fraction)] = hits.as_slice() {
            self.latch
                .block((*d).clone(), rule.source.edge, Instant::now());
            let _ = self.events.try_send(Event::Cross {
                rule: rule.id.clone(),
                fraction: *fraction,
                generation: self.state.generation,
                at: Instant::now(),
            });
        }
    }
    fn placement(&mut self, s: &Segment, f: f64, generation: u64) -> Result<(u64, Point)> {
        ensure!(
            self.enabled && self.state.ready && !self.state.locked,
            "Desktop is unavailable, paused or locked"
        );
        ensure!(
            self.state.generation == generation,
            "Desktop layout changed"
        );
        ensure!(
            self.reservation
                .as_ref()
                .is_none_or(|r| r.until <= Instant::now()),
            "Another handoff is in progress"
        );
        let d = self
            .state
            .displays
            .iter()
            .find(|d| d.id == s.display)
            .ok_or_else(|| anyhow::anyhow!("Destination display missing"))?;
        let p = s.arrival(d, f)?;
        self.sequence = self.sequence.wrapping_add(1);
        self.reservation = Some(Reservation {
            id: self.sequence,
            previous: self.state.pointer,
            serial: self.state.input_serial,
            until: Instant::now() + Duration::from_secs(5),
        });
        self.latch.block(d.clone(), s.edge, Instant::now());
        Ok((self.sequence, p))
    }
    fn cancel(&mut self, id: u64) -> Option<Point> {
        if self.reservation.as_ref().is_some_and(|r| r.id == id) {
            let r = self.reservation.take().unwrap();
            if r.serial == self.state.input_serial {
                return Some(r.previous);
            }
        }
        None
    }
    fn complete(&mut self, id: u64) {
        if self.reservation.as_ref().is_some_and(|r| r.id == id) {
            self.reservation = None;
        }
    }
}

/// A topology change invalidates affected bindings even when a monitor later
/// returns to the same position. Persist epochs so restarting cannot bypass review.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Bindings {
    #[serde(skip)]
    path: std::path::PathBuf,
    observed: Vec<easy_switch_core::follow::Display>,
    epochs: std::collections::BTreeMap<String, u64>,
}
impl Bindings {
    fn verify(&mut self, displays: &mut [easy_switch_core::follow::Display]) -> Result<()> {
        if self.observed != displays {
            for previous in &self.observed {
                if !displays.iter().any(|d| d == previous) {
                    *self.epochs.entry(previous.id.clone()).or_default() += 1;
                }
            }
            self.observed = displays.to_vec();
            use std::io::Write;
            let temporary = self.path.with_extension("tmp");
            let mut options = std::fs::OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec(self)?)?;
            file.sync_all()?;
            std::fs::rename(temporary, &self.path)?;
        }
        for display in displays {
            if let Some(epoch) = self.epochs.get(&display.id) {
                display
                    .binding
                    .push_str(&format!(":verified-layout-{epoch}"));
            }
        }
        Ok(())
    }
}
