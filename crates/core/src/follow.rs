//! Geometry, configuration and public diagnostics for destination-follows-mouse.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Left,
    #[default]
    Right,
    Top,
    Bottom,
}
impl Edge {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
    pub fn outward(self, delta: Point) -> f64 {
        match self {
            Self::Left => -delta.x,
            Self::Right => delta.x,
            Self::Top => -delta.y,
            Self::Bottom => delta.y,
        }
    }
    pub fn vertical(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Display {
    pub id: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    pub binding: String,
}
impl Display {
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.x + self.width && p.y < self.y + self.height
    }
    pub fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height, self.scale]
            .iter()
            .all(|v| v.is_finite())
            && (0.25..=8.0).contains(&self.scale)
            && (32.0..=65536.0).contains(&self.width)
            && (32.0..=65536.0).contains(&self.height)
            && self.width >= 16.0 * self.scale
            && self.height >= 16.0 * self.scale
            && self.x.abs() <= 1_000_000.0
            && self.y.abs() <= 1_000_000.0
            && !self.id.is_empty()
            && self.id.len() <= 4096
            && self.binding.len() <= 1024
    }
    pub fn inward(&self, edge: Edge, p: Point) -> f64 {
        match edge {
            Edge::Left => p.x - self.x,
            Edge::Right => self.x + self.width - p.x,
            Edge::Top => p.y - self.y,
            Edge::Bottom => self.y + self.height - p.y,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Segment {
    pub display: String,
    pub binding: String,
    pub edge: Edge,
    pub start: f64,
    pub end: f64,
}
impl Default for Segment {
    fn default() -> Self {
        Self {
            display: String::new(),
            binding: String::new(),
            edge: Edge::Right,
            start: 0.0,
            end: 100.0,
        }
    }
}
impl Segment {
    /// Automatically accept new geometry only for the same unambiguous display.
    pub fn rebind(&mut self, displays: &[Display]) -> bool {
        let mut matches = displays.iter().filter(|d| d.id == self.display);
        let Some(d) = matches.next() else {
            return false;
        };
        if matches.next().is_some() || !d.valid() || d.binding == self.binding {
            return false;
        }
        self.binding = d.binding.clone();
        true
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.display.is_empty()
                && !self.binding.is_empty()
                && self.display.len() <= 4096
                && self.binding.len() <= 1024,
            "Select and verify a display"
        );
        ensure!(
            self.start.is_finite()
                && self.end.is_finite()
                && 0.0 <= self.start
                && self.start < self.end
                && self.end <= 100.0,
            "Edge segments must satisfy 0 <= start < end <= 100"
        );
        Ok(())
    }
    pub fn position(&self, d: &Display, p: Point) -> f64 {
        let (v, origin, size) = if self.edge.vertical() {
            (p.y, d.y, d.height)
        } else {
            (p.x, d.x, d.width)
        };
        (v - origin) / size * 100.0
    }
    pub fn fraction(&self, d: &Display, p: Point) -> Option<f64> {
        let v = self.position(d, p);
        (v >= self.start && (v < self.end || self.end == 100.0 && v <= 100.0))
            .then_some(((v - self.start) / (self.end - self.start)).clamp(0.0, 1.0))
    }
    pub fn arrival(&self, d: &Display, t: f64) -> Result<Point> {
        self.validate()?;
        ensure!(
            d.valid() && d.id == self.display && d.binding == self.binding,
            "Destination display binding changed"
        );
        ensure!(
            t.is_finite() && (0.0..=1.0).contains(&t),
            "Invalid crossing position"
        );
        let u = (self.start + (self.end - self.start) * t) / 100.0;
        let inset = 8.0 * d.scale;
        Ok(match self.edge {
            Edge::Left => Point {
                x: d.x + inset,
                y: (d.y + d.height * u).clamp(d.y + 1.0, d.y + d.height - 1.0),
            },
            Edge::Right => Point {
                x: d.x + d.width - inset,
                y: (d.y + d.height * u).clamp(d.y + 1.0, d.y + d.height - 1.0),
            },
            Edge::Top => Point {
                x: (d.x + d.width * u).clamp(d.x + 1.0, d.x + d.width - 1.0),
                y: d.y + inset,
            },
            Edge::Bottom => Point {
                x: (d.x + d.width * u).clamp(d.x + 1.0, d.x + d.width - 1.0),
                y: d.y + d.height - inset,
            },
        })
    }
    pub fn hit(&self, d: &Display, p: Point, delta: Point) -> bool {
        let outward = match self.edge {
            Edge::Left => delta.x < 0.0,
            Edge::Right => delta.x > 0.0,
            Edge::Top => delta.y < 0.0,
            Edge::Bottom => delta.y > 0.0,
        };
        d.id == self.display
            && d.binding == self.binding
            && d.inward(self.edge, p) <= 1.1
            && d.inward(self.edge, p) >= -1.0
            && outward
            && self.fraction(d, p).is_some()
    }
    pub fn crossing(&self, d: &Display, from: Point, to: Point) -> Option<f64> {
        if d.id != self.display || d.binding != self.binding || !d.contains(from) {
            return None;
        }
        let before = d.inward(self.edge, from);
        let after = d.inward(self.edge, to);
        if before < 0.0 || after > 0.0 || before <= after {
            return None;
        }
        let t = before / (before - after);
        self.fraction(
            d,
            Point {
                x: from.x + (to.x - from.x) * t,
                y: from.y + (to.y - from.y) * t,
            },
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub enabled: bool,
    pub source: Segment,
    pub destination: Segment,
    pub peer: String,
    pub computer: String,
    pub group: String,
    pub mouse: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FollowConfig {
    pub enabled: bool,
    pub local_computer: String,
    pub port: u16,
    pub discovery: bool,
    pub rules: Vec<Rule>,
}
impl Default for FollowConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            local_computer: String::new(),
            port: 24873,
            discovery: true,
            rules: vec![],
        }
    }
}
impl FollowConfig {
    pub fn validate(&self, config: &crate::config::Config) -> Result<()> {
        ensure!(
            self.port >= 1024,
            "Choose a network port from 1024 to 65535"
        );
        ensure!(self.rules.len() <= 64, "Too many edge rules");
        if self.enabled {
            ensure!(
                config.computers.contains_key(&self.local_computer),
                "Select this computer's identity before enabling edge switching"
            );
        }
        let mut ids = BTreeSet::new();
        for (i, r) in self.rules.iter().enumerate() {
            ensure!(
                !r.id.is_empty() && r.id.len() <= 128 && ids.insert(&r.id),
                "Duplicate or invalid edge rule ID"
            );
            r.source.validate()?;
            r.destination.validate()?;
            ensure!(
                !r.peer.is_empty() && r.peer.len() <= 128,
                "Select a paired destination"
            );
            ensure!(
                config.computers.contains_key(&r.computer) && r.computer != self.local_computer,
                "Invalid edge destination computer"
            );
            let g = config
                .groups
                .iter()
                .find(|g| g.id == r.group)
                .ok_or_else(|| anyhow::anyhow!("Unknown edge rule group"))?;
            ensure!(
                g.devices
                    .iter()
                    .any(|m| m.id == r.mouse && m.channels.contains_key(&r.computer)),
                "Choose a mapped primary mouse in the selected group"
            );
            for other in &self.rules[..i] {
                if r.enabled
                    && other.enabled
                    && r.source.display == other.source.display
                    && r.source.edge == other.source.edge
                {
                    ensure!(
                        r.source.end <= other.source.start || other.source.end <= r.source.start,
                        "Overlapping source edge segments"
                    );
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DesktopState {
    #[serde(default)]
    pub cursor_hidden: bool,
    #[serde(default)]
    pub cursor_visibility: String,
    pub backend: String,
    pub displays: Vec<Display>,
    pub generation: u64,
    pub ready: bool,
    pub locked: bool,
    pub reason: String,
    pub pointer: Point,
    pub input_serial: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerStatus {
    pub id: String,
    pub name: String,
    pub address: String,
    pub computer: String,
    pub trusted: bool,
    pub connected: bool,
    pub ready: bool,
    pub reason: String,
    pub displays: Vec<Display>,
    pub generation: u64,
    pub config_generation: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pairing {
    pub id: String,
    pub name: String,
    pub code: String,
    pub confirmed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffResult {
    pub transaction: String,
    pub rule: String,
    pub outcome: String,
    pub edge_to_ready_ms: Option<f64>,
    pub ready_to_queue_ms: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FollowStatus {
    #[serde(default)]
    pub handoff: Option<HandoffResult>,
    #[serde(default)]
    pub proposed_rules: Vec<Rule>,
    pub identity: String,
    pub desktop: DesktopState,
    pub peers: Vec<PeerStatus>,
    pub pairing: Vec<Pairing>,
    pub pairing_open: bool,
    pub rules: Vec<(String, String)>,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Status,
    Authorize,
    OpenPairing,
    Connect { address: String },
    Confirm { peer: String, code: String },
    Revoke { peer: String },
    SetRule { id: String, enabled: bool },
    Reverse { id: String },
}

/// Outward pressure is the main guard. A short cooldown discards queued events
/// from the previous crossing/arrival without requiring an inward detour.
#[derive(Default)]
pub struct EdgeLatch {
    blocked_until: Option<Instant>,
    pressure: Option<(String, f64, Instant)>,
}
impl EdgeLatch {
    pub const BUFFER: f64 = 12.0;
    pub const COOLDOWN: Duration = Duration::from_millis(80);
    pub const PRESSURE_TIMEOUT: Duration = Duration::from_millis(250);

    pub fn block(&mut self, now: Instant) {
        self.reset();
        self.blocked_until = Some(now + Self::COOLDOWN);
    }
    pub fn reset(&mut self) {
        self.pressure = None;
    }
    pub fn eligible(&self, now: Instant) -> bool {
        self.blocked_until.is_none_or(|until| now >= until)
    }
    pub fn pending(&self, rule: &str, now: Instant) -> bool {
        self.pressure.as_ref().is_some_and(|(id, _, at)| {
            id == rule && now.saturating_duration_since(*at) < Self::PRESSURE_TIMEOUT
        })
    }
    pub fn push(&mut self, rule: &str, distance: f64, now: Instant) -> bool {
        if !self.eligible(now) || !distance.is_finite() || distance < 0.0 {
            self.reset();
            return false;
        }
        if !self.pending(rule, now) {
            self.pressure = Some((rule.into(), 0.0, now));
        }
        let (_, total, at) = self.pressure.as_mut().unwrap();
        *total += distance;
        *at = now;
        if *total >= Self::BUFFER {
            self.block(now);
            true
        } else {
            false
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_rebind_preserves_monitor_and_segment_and_rejects_ambiguity() {
        let mut d = display();
        let mut s = Segment {
            display: d.id.clone(),
            binding: d.binding.clone(),
            start: 33.3,
            ..Default::default()
        };
        d.x = 0.0;
        d.scale = 2.0;
        d.binding = "changed-layout".into();
        assert!(s.rebind(&[d.clone()]));
        assert_eq!(s.binding, "changed-layout");
        assert_eq!(s.start, 33.3);
        assert!(!s.rebind(&[d.clone()]));
        d.binding = "newer".into();
        assert!(!s.rebind(&[d.clone(), d.clone()]));
        d.id = "replacement-monitor".into();
        assert!(!s.rebind(&[d]));
        assert_eq!(s.binding, "changed-layout");
    }
    #[test]
    fn pressure_does_not_carry_between_rules_or_across_idle_gaps() {
        let at = Instant::now();
        let mut latch = EdgeLatch::default();
        assert!(!latch.push("one", 8.0, at));
        assert!(!latch.push("two", 4.0, at));
        assert!(!latch.push("two", 8.0, at + EdgeLatch::PRESSURE_TIMEOUT));
        latch.reset();
        assert!(!latch.push("two", 4.0, at + EdgeLatch::PRESSURE_TIMEOUT));
        assert!(latch.push("two", 8.0, at + EdgeLatch::PRESSURE_TIMEOUT));
    }
    #[test]
    fn fresh_pressure_can_return_after_eighty_ms_without_inward_motion() {
        let at = Instant::now();
        let mut latch = EdgeLatch::default();
        assert!(latch.push("out", EdgeLatch::BUFFER, at));
        assert!(!latch.push("back", 100.0, at + Duration::from_millis(79)));
        assert!(!latch.push("back", 6.0, at + Duration::from_millis(80)));
        assert!(latch.push("back", 6.0, at + Duration::from_millis(81)));
    }
    fn display() -> Display {
        Display {
            id: "a".into(),
            name: "a".into(),
            x: -1920.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
            scale: 1.5,
            binding: "layout".into(),
        }
    }
    #[test]
    fn crossing_internal_seam_and_proportional_full_edges() {
        let d = display();
        for edge in Edge::ALL {
            let s = Segment {
                display: d.id.clone(),
                binding: d.binding.clone(),
                edge,
                ..Default::default()
            };
            for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
                assert!(d.contains(s.arrival(&d, fraction).unwrap()));
            }
        }
        let s = Segment {
            display: d.id.clone(),
            binding: d.binding.clone(),
            ..Default::default()
        };
        assert_eq!(
            s.crossing(
                &d,
                Point { x: -10.0, y: 540.0 },
                Point { x: 10.0, y: 540.0 }
            ),
            Some(0.5)
        );
        assert_eq!(
            s.crossing(
                &d,
                Point { x: 10.0, y: 540.0 },
                Point { x: -10.0, y: 540.0 }
            ),
            None
        );
    }
    #[test]
    fn overlapping_source_rules_are_rejected_but_adjacent_segments_are_valid() {
        use crate::config::{Config, Group, Member, SourceMode};
        let mut c = Config::default();
        c.follow.local_computer = "computer-1".into();
        c.groups.push(Group {
            id: "desk".into(),
            devices: vec![Member {
                id: "mouse".into(),
                channels: std::collections::BTreeMap::from([("computer-2".into(), 2)]),
                mode: SourceMode::Auto,
            }],
        });
        let s = Segment {
            display: "display".into(),
            binding: "layout".into(),
            ..Default::default()
        };
        let r = Rule {
            id: "one".into(),
            enabled: true,
            source: s.clone(),
            destination: s,
            peer: "peer".into(),
            computer: "computer-2".into(),
            group: "desk".into(),
            mouse: "mouse".into(),
        };
        let mut r2 = r.clone();
        r2.id = "two".into();
        c.follow.rules = vec![r, r2];
        assert!(c.validate().is_err());
        c.follow.rules[0].source.end = 50.0;
        c.follow.rules[1].source.start = 50.0;
        assert!(c.validate().is_ok());
    }
    #[test]
    fn partial_segments_map_negative_scaled_coordinates() {
        let d = display();
        let mut s = Segment {
            display: d.id.clone(),
            binding: d.binding.clone(),
            edge: Edge::Left,
            start: 25.0,
            end: 75.0,
        };
        assert_eq!(
            s.fraction(
                &d,
                Point {
                    x: -1920.0,
                    y: 540.0
                }
            ),
            Some(0.5)
        );
        assert!(
            s.fraction(
                &d,
                Point {
                    x: -1920.0,
                    y: 50.0
                }
            )
            .is_none()
        );
        assert_eq!(
            s.arrival(&d, 0.5).unwrap(),
            Point {
                x: -1908.0,
                y: 540.0
            }
        );
        s.start = f64::NAN;
        assert!(s.validate().is_err());
    }
    #[test]
    fn outward_only_and_bounce_guard() {
        let d = display();
        let s = Segment {
            display: d.id.clone(),
            binding: d.binding.clone(),
            ..Default::default()
        };
        assert!(s.hit(&d, Point { x: -1.0, y: 100.0 }, Point { x: 5.0, y: 0.0 }));
        assert!(!s.hit(&d, Point { x: -1.0, y: 100.0 }, Point { x: -5.0, y: 0.0 }));
        let now = Instant::now();
        let mut latch = EdgeLatch::default();
        latch.block(now);
        assert!(!latch.push("edge", 50.0, now));
        let ready = now + EdgeLatch::COOLDOWN;
        assert!(!latch.push("edge", 6.0, ready));
        assert!(latch.push("edge", 6.0, ready));
    }
}
