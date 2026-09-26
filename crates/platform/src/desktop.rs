//! Event-driven desktop integration. Never polls the cursor while idle.
use anyhow::{Result, ensure};
use easy_switch_core::follow::{DesktopState, EdgeLatch, Point, Rule, Segment};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

#[cfg(target_os = "linux")]
#[path = "desktop_gnome.rs"]
mod gnome;
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
    #[test]
    fn recovery_input_guards_cancel_on_relevant_input_and_new_handoffs() {
        let activity = Arc::new(InputActivity::default());
        assert!(activity.guard("keyboard").is_none());
        activity.available(true);
        let keyboard = activity.guard("keyboard").unwrap();
        let mouse = activity.guard("mouse").unwrap();
        activity.input(false, true);
        assert!(
            keyboard.valid(),
            "Mouse movement must not cancel a keyboard retry"
        );
        assert!(!mouse.valid());
        activity.input(true, false);
        assert!(!keyboard.valid());
        let guard = activity.guard("keyboard").unwrap();
        activity.cancel();
        assert!(
            !guard.valid(),
            "Arrival/configuration/new handoff cancels old retries"
        );
        let guard = activity.guard("other").unwrap();
        activity.input(true, true);
        assert!(!guard.valid());
    }
    #[test]
    fn departure_requires_current_crossing_and_success_before_new_input() {
        let at = Instant::now();
        let written = at + Duration::from_millis(30);
        let mut cursor = CursorVisibility::default();
        cursor.depart(at, written, written);
        assert!(!cursor.hidden, "Manual switches have no edge crossing");
        cursor.crossing = Some(at);
        cursor.depart(at, written, written);
        assert!(cursor.hidden);
        cursor.input(written + Duration::from_millis(1));
        assert!(
            !cursor.hidden,
            "Unknown, unlinked and returning inputs restore"
        );
        cursor.depart(at, written, written + Duration::from_millis(2));
        assert!(!cursor.hidden, "Duplicate completion cannot re-hide");
        cursor.crossing = Some(at);
        cursor.depart(at, written, written + Duration::from_millis(2));
        assert!(
            !cursor.hidden,
            "Input already processed after the write wins"
        );
    }
    #[test]
    fn departure_is_invalidated_by_arrival_configuration_or_expiration() {
        let at = Instant::now();
        for case in 0..3 {
            let mut cursor = CursorVisibility {
                crossing: Some(at),
                ..Default::default()
            };
            if case == 0 {
                cursor.restore();
            }
            if case == 1 {
                cursor.crossing = Some(at + Duration::from_millis(1));
            }
            let now = at + Duration::from_secs(if case == 2 { 2 } else { 0 });
            cursor.depart(at, at, now);
            assert!(!cursor.hidden);
        }
    }
    #[test]
    fn synthetic_motion_cannot_restore_departed_cursor_but_arrival_does() {
        let (mut m, _, segment) = fixture();
        m.cursor.hidden = true;
        let p = m.state.pointer;
        m.movement(p, Point::default(), false, true);
        assert!(m.cursor.hidden);
        m.placement(&segment, 0.5, m.state.generation).unwrap();
        assert!(!m.cursor.hidden);
        m.cursor.hidden = true;
        m.input();
        assert!(!m.cursor.hidden);
    }
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
    fn buffer_scales_on_all_four_edges_and_fast_contact_does_not_trigger() {
        for edge in Edge::ALL {
            let (mut m, rx, _) = fixture();
            m.state.displays[0].scale = 2.0;
            m.rules[0].source.edge = edge;
            let (p, delta) = match edge {
                Edge::Left => (
                    Point {
                        x: -1000.0,
                        y: 400.0,
                    },
                    Point { x: -12.0, y: 0.0 },
                ),
                Edge::Right => (Point { x: -1.0, y: 400.0 }, Point { x: 12.0, y: 0.0 }),
                Edge::Top => (Point { x: -500.0, y: 0.0 }, Point { x: 0.0, y: -12.0 }),
                Edge::Bottom => (
                    Point {
                        x: -500.0,
                        y: 799.0,
                    },
                    Point { x: 0.0, y: 12.0 },
                ),
            };
            m.movement(
                p,
                Point {
                    x: delta.x * 100.0,
                    y: delta.y * 100.0,
                },
                false,
                false,
            );
            assert!(rx.is_empty(), "Fast approach is not pressure: {edge:?}");
            m.movement(p, delta, false, false);
            assert!(rx.is_empty(), "Half a buffer: {edge:?}");
            m.movement(p, delta, false, false);
            assert!(matches!(rx.try_recv(), Ok(Event::Cross { .. })), "{edge:?}");
        }
    }
    #[test]
    fn inward_motion_buttons_and_synthetic_motion_discard_partial_pressure() {
        for mode in 0..3 {
            let (mut m, rx, _) = fixture();
            let p = Point { x: -1.0, y: 400.0 };
            m.state.pointer = p;
            m.movement(p, Point { x: 8.0, y: 0.0 }, false, false);
            m.movement(
                p,
                Point {
                    x: if mode == 0 { -1.0 } else { 1.0 },
                    y: 0.0,
                },
                mode == 1,
                mode == 2,
            );
            m.movement(p, Point { x: 4.0, y: 0.0 }, false, false);
            assert!(rx.is_empty());
            m.movement(p, Point { x: 8.0, y: 0.0 }, false, false);
            assert!(matches!(rx.try_recv(), Ok(Event::Cross { .. })));
        }
    }
    #[test]
    fn pressure_can_continue_across_an_internal_display_seam() {
        let (mut m, rx, _) = fixture();
        m.state.pointer = Point { x: -10.0, y: 400.0 };
        for (x, dx) in [(5.0, 15.0), (10.0, 5.0)] {
            m.movement(Point { x, y: 400.0 }, Point { x: dx, y: 0.0 }, false, false);
            assert!(rx.is_empty());
        }
        m.movement(
            Point { x: 12.0, y: 400.0 },
            Point { x: 2.0, y: 0.0 },
            false,
            false,
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(Event::Cross { fraction: 0.5, .. })
        ));
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
        assert!(rx.is_empty(), "Merely reaching the edge must not switch");
        m.movement(
            Point { x: -1.0, y: 400.0 },
            Point { x: 7.0, y: 0.0 },
            false,
            false,
        );
        assert!(rx.is_empty());
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
    Restore(oneshot::Sender<()>),
    /// Sent only after the primary mouse's edge-switch HID write succeeds.
    Depart {
        crossing: Instant,
        written: Instant,
    },
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
    activity: Arc<InputActivity>,
    tx: mpsc::Sender<Command>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
impl Handle {
    pub fn retry_guard(&self, kind: &str) -> Option<RetryGuard> {
        self.activity.guard(kind)
    }
    pub fn cancel_retries(&self) {
        self.activity.cancel();
    }
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
    pub fn send(&self, c: Command) -> Result<()> {
        if !matches!(
            c,
            Command::Depart { .. } | Command::Complete(_) | Command::Authorize
        ) {
            self.activity.cancel();
        }
        self.tx
            .try_send(c)
            .map_err(|_| anyhow::anyhow!("Desktop command queue unavailable"))?;
        (self.wake)();
        Ok(())
    }
    pub async fn restore(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send(Command::Restore(tx))?;
        rx.await?;
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
    let activity = Arc::new(InputActivity::default());
    #[cfg(windows)]
    let wake = native::start(rx, events, _private_dir, activity.clone())?;
    #[cfg(target_os = "linux")]
    let use_gnome = {
        let p = crate::system::policy()?;
        p.as_ref()
            .is_some_and(|p| p.login_handoffs && unsafe { libc::geteuid() } == p.peer_uid)
            || _private_dir.join("gnome-companion.enabled").exists()
    };
    #[cfg(target_os = "linux")]
    let wake = if use_gnome {
        gnome::start(rx, events, _private_dir, activity.clone(), tx.clone())?
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        wayland::start(rx, events, _private_dir, activity.clone())?
    } else {
        x11::start(rx, events, _private_dir, activity.clone())?
    };
    let handle = Handle { tx, wake, activity };
    #[cfg(target_os = "linux")]
    if !use_gnome {
        session_watch::start(handle.clone());
    }
    Ok(handle)
}
struct Reservation {
    id: u64,
    previous: Point,
    serial: u64,
    until: Instant,
}
pub(super) struct Model {
    activity: Arc<InputActivity>,
    cursor: CursorVisibility,
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
        #[allow(unused_mut)]
        let mut model = Self {
            activity: Arc::default(),
            cursor: CursorVisibility::default(),
            bindings: None,
            state: DesktopState {
                backend: backend.into(),
                cursor_visibility: "Cursor hiding is unavailable on this desktop backend".into(),
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
        };
        #[cfg(target_os = "linux")]
        if let Ok(Some(p)) = crate::system::policy()
            && let Ok(s) = crate::system::active_session(&p)
        {
            model.state.session = s.id;
            model.state.session_kind = if s.greeter { "greeter" } else { "user" }.into();
        }
        model
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
        self.activity.cancel();
        self.cursor.restore();
        self.latch.reset();
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
            self.latch.reset();
            return;
        }
        self.input();
        let now = Instant::now();
        if !self.enabled
            || !self.state.ready
            || !self.state.available()
            || buttons
            || !self.latch.eligible(now)
        {
            self.latch.reset();
            return;
        }
        let hits: Vec<_> = self
            .rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                self.state.displays.iter().find_map(|d| {
                    let outward = r.source.edge.outward(delta);
                    if outward <= 0.0 || !outward.is_finite() {
                        return None;
                    }
                    let crossing = r.source.crossing(d, previous, p);
                    let continuing = self.latch.pending(&r.id, now)
                        && d.id == r.source.display
                        && d.binding == r.source.binding
                        && d.inward(r.source.edge, p) <= 1.1;
                    if !r.source.hit(d, p, delta) && crossing.is_none() && !continuing {
                        return None;
                    }
                    let fraction = r.source.fraction(d, p).or(crossing)?;
                    let before = d.inward(r.source.edge, previous);
                    let distance = if before <= 1.1 && (before >= -1.0 || continuing) {
                        outward
                    } else {
                        // A fast approach to a clamped outer edge isn't pressure.
                        // At an internal seam, only the actual overshoot counts.
                        (-d.inward(r.source.edge, p)).max(0.0)
                    } / d.scale;
                    Some((r, fraction, distance))
                })
            })
            .collect();
        if let [(rule, fraction, distance)] = hits.as_slice() {
            if self.latch.push(&rule.id, *distance, now) {
                self.cursor.crossing = Some(now);
                let _ = self.events.try_send(Event::Cross {
                    rule: rule.id.clone(),
                    fraction: *fraction,
                    generation: self.state.generation,
                    at: now,
                });
            }
        } else {
            self.latch.reset();
        }
    }
    fn placement(&mut self, s: &Segment, f: f64, generation: u64) -> Result<(u64, Point)> {
        ensure!(
            self.enabled && self.state.available(),
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
        self.cursor.restore();
        self.sequence = self.sequence.wrapping_add(1);
        self.reservation = Some(Reservation {
            id: self.sequence,
            previous: self.state.pointer,
            serial: self.state.input_serial,
            until: Instant::now() + Duration::from_secs(5),
        });
        self.latch.block(Instant::now());
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
    fn input(&mut self) {
        self.state.input_serial = self.state.input_serial.wrapping_add(1);
        self.cursor.input(Instant::now());
    }
}

/// Restoring on *all* physical input includes unknown/unlinked devices and a
/// linked device returning manually. Receiver collections do not always expose
/// enough identity to distinguish their paired devices; never guess or swallow input.
#[derive(Default)]
struct CursorVisibility {
    crossing: Option<Instant>,
    last_input: Option<Instant>,
    hidden: bool,
}
impl CursorVisibility {
    fn depart(&mut self, crossing: Instant, written: Instant, now: Instant) {
        if self.crossing == Some(crossing)
            && written >= crossing
            && written <= now
            && now.saturating_duration_since(crossing) < Duration::from_secs(1)
            && self.last_input.is_none_or(|input| input <= written)
        {
            self.hidden = true;
        }
        // A duplicate completion must never hide a cursor restored by local input.
        self.crossing = None;
    }
    fn input(&mut self, at: Instant) {
        self.last_input = Some(at);
        self.hidden = false;
    }
    fn restore(&mut self) {
        self.crossing = None;
        self.hidden = false;
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

/// Raw receiver collections cannot always identify a paired unit. Activity from
/// *any* keyboard cancels keyboard retries; any pointer cancels pointer retries.
/// Unknown devices cancel both. Wayland cannot observe uncaptured keyboard input,
/// so no guard is issued there. Counters contain no key/button content.
#[derive(Default)]
pub(super) struct InputActivity {
    available: std::sync::atomic::AtomicBool,
    generation: std::sync::atomic::AtomicU64,
    keyboard: std::sync::atomic::AtomicU64,
    pointer: std::sync::atomic::AtomicU64,
}
#[derive(Clone)]
pub struct RetryGuard {
    activity: Arc<InputActivity>,
    generation: u64,
    keyboard: Option<u64>,
    pointer: Option<u64>,
}
impl InputActivity {
    fn cancel(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn input(&self, keyboard: bool, pointer: bool) {
        if keyboard {
            self.keyboard
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        if pointer {
            self.pointer
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn available(&self, value: bool) {
        if self
            .available
            .swap(value, std::sync::atomic::Ordering::SeqCst)
            != value
        {
            self.cancel();
        }
    }
    fn guard(self: &Arc<Self>, kind: &str) -> Option<RetryGuard> {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.available.load(SeqCst) {
            return None;
        }
        Some(RetryGuard {
            activity: self.clone(),
            generation: self.generation.load(SeqCst),
            keyboard: (kind != "mouse").then(|| self.keyboard.load(SeqCst)),
            pointer: (kind != "keyboard").then(|| self.pointer.load(SeqCst)),
        })
    }
}
impl RetryGuard {
    pub fn valid(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.activity.available.load(SeqCst)
            && self.activity.generation.load(SeqCst) == self.generation
            && self
                .keyboard
                .is_none_or(|v| self.activity.keyboard.load(SeqCst) == v)
            && self
                .pointer
                .is_none_or(|v| self.activity.pointer.load(SeqCst) == v)
    }
}
