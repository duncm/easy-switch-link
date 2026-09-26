use crate::{
    Device,
    config::{Config, SourceMode},
};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchCommand {
    pub transaction: Option<String>,
    pub expires: Option<Instant>,
    pub id: String,
    pub receiver: String,
    pub slot: u8,
    pub feature: u8,
    pub target: u8,
    pub generation: u64,
}

#[derive(Default)]
pub struct Engine {
    pub config: Config,
    pub devices: BTreeMap<String, Device>,
    pub paused: bool,
    echoes: HashMap<String, (u8, Instant, u64)>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }
    pub fn replace_config(&mut self, config: Config) {
        self.config = config;
        self.echoes.clear();
    }
    pub fn disconnect(&mut self, receiver: &str) {
        for d in self.devices.values_mut().filter(|d| d.receiver == receiver) {
            d.online = false;
        }
        self.echoes
            .retain(|id, _| self.devices.get(id).is_some_and(|d| d.online));
    }
    pub fn from_event(
        &mut self,
        id: &str,
        target: u8,
        now: Instant,
    ) -> (Vec<SwitchCommand>, Vec<String>) {
        if self.paused {
            return Default::default();
        }
        let Some(source) = self.devices.get(id).filter(|d| d.online).cloned() else {
            return Default::default();
        };
        if target >= source.host_count || target == source.current_host {
            return Default::default();
        }
        if self.echoes.get(id).is_some_and(|(h, t, g)| {
            *h == target
                && *g == source.generation
                && now.saturating_duration_since(*t) < Duration::from_secs(2)
        }) {
            return Default::default();
        }
        let Some(group) = self
            .config
            .groups
            .iter()
            .find(|g| g.devices.iter().any(|m| m.id == id))
            .cloned()
        else {
            return Default::default();
        };
        let member = group.devices.iter().find(|m| m.id == id).unwrap();
        if member.mode == SourceMode::FollowOnly {
            return Default::default();
        }
        let destination = member
            .channels
            .iter()
            .find(|(_, ch)| **ch == target + 1)
            .map(|(pc, _)| pc.clone());
        let mut result = match destination {
            Some(pc) => self.plan(&group.id, &pc, Some(id), now),
            None => (
                Vec::new(),
                vec![format!(
                    "{} channel {} is not mapped",
                    source.name,
                    target + 1
                )],
            ),
        };
        // A diverted key must still perform its original function, even with missing mappings/followers.
        if source.intercepted {
            result.0.push(command(&source, target));
        }
        self.echoes
            .insert(id.into(), (target, now, source.generation));
        result
    }
    pub fn plan(
        &mut self,
        group_id: &str,
        computer: &str,
        skip: Option<&str>,
        now: Instant,
    ) -> (Vec<SwitchCommand>, Vec<String>) {
        self.plan_inner(group_id, computer, skip, now, false)
    }
    /// Edge handoffs include cached/reconnecting followers; the transport verifies
    /// identity and reachability before any deferred recovery write.
    pub fn plan_edge(
        &mut self,
        group_id: &str,
        computer: &str,
        now: Instant,
    ) -> (Vec<SwitchCommand>, Vec<String>) {
        self.plan_inner(group_id, computer, None, now, true)
    }
    fn plan_inner(
        &mut self,
        group_id: &str,
        computer: &str,
        skip: Option<&str>,
        now: Instant,
        edge: bool,
    ) -> (Vec<SwitchCommand>, Vec<String>) {
        let mut commands = Vec::new();
        let mut messages = Vec::new();
        let Some(group) = self.config.groups.iter().find(|g| g.id == group_id) else {
            return (commands, vec!["Unknown link group".into()]);
        };
        for member in &group.devices {
            if Some(member.id.as_str()) == skip {
                continue;
            }
            let Some(&ch) = member.channels.get(computer) else {
                messages.push(format!("{} has no mapping for {computer}", member.id));
                continue;
            };
            let Some(d) = self.devices.get(&member.id).filter(|d| edge || d.online) else {
                messages.push(format!("{} is unavailable on this computer", member.id));
                continue;
            };
            if ch > d.host_count {
                messages.push(format!("{} does not support channel {ch}", d.name));
                continue;
            }
            if !edge && d.current_host == ch - 1 {
                continue;
            }
            commands.push(command(d, ch - 1));
            self.echoes
                .insert(d.id.clone(), (ch - 1, now, d.generation));
        }
        (commands, messages)
    }
}

fn command(d: &Device, target: u8) -> SwitchCommand {
    SwitchCommand {
        expires: None,
        transaction: None,
        id: d.id.clone(),
        receiver: d.receiver.clone(),
        slot: d.slot,
        feature: d.change_host,
        target,
        generation: d.generation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Group, Member};
    pub fn device(id: &str, slot: u8) -> Device {
        Device {
            kind: String::new(),
            id: id.into(),
            receiver: "bolt".into(),
            slot,
            name: id.into(),
            host_count: 3,
            current_host: 0,
            change_host: 9 + slot,
            reprog: None,
            wireless_status: None,
            controls: vec![],
            intercepted: false,
            online: true,
            generation: 1,
            capability: String::new(),
        }
    }
    fn engine() -> Engine {
        let mut e = Engine::default();
        e.devices.insert("keyboard".into(), device("keyboard", 1));
        e.devices.insert("mouse".into(), device("mouse", 2));
        e.config.groups.push(Group {
            id: "desk".into(),
            devices: vec![
                Member {
                    id: "keyboard".into(),
                    channels: [("computer-1".into(), 1), ("computer-2".into(), 2)].into(),
                    mode: SourceMode::Auto,
                },
                Member {
                    id: "mouse".into(),
                    channels: [("computer-1".into(), 1), ("computer-2".into(), 3)].into(),
                    mode: SourceMode::Auto,
                },
            ],
        });
        e
    }
    #[test]
    fn translate_channels_suppress_echoes_and_duplicates() {
        let mut e = engine();
        let now = Instant::now();
        let (commands, errors) = e.from_event("keyboard", 1, now);
        assert!(errors.is_empty());
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].target, 2);
        assert!(e.from_event("keyboard", 1, now).0.is_empty());
        assert!(e.from_event("mouse", 2, now).0.is_empty());
        // A new destination is not swallowed by the duplicate guard.
        assert!(!e.from_event("keyboard", 2, now).1.is_empty());
    }
    #[test]
    fn rapid_edge_handoffs_include_reconnecting_and_stale_host_followers() {
        let mut e = engine();
        e.devices.get_mut("keyboard").unwrap().online = false;
        let now = Instant::now();
        for destination in ["computer-2", "computer-1", "computer-2"] {
            let (commands, _) = e.plan_edge("desk", destination, now);
            assert_eq!(commands.len(), 2);
            assert!(commands.iter().any(|c| c.id == "keyboard"));
        }
        assert_eq!(e.plan("desk", "computer-2", None, now).0.len(), 1);
    }
    #[test]
    fn diverted_source_survives_partial_failure_and_unmapped_channel() {
        let mut e = engine();
        e.config.groups[0].devices[0].mode = SourceMode::Intercept;
        e.devices.get_mut("keyboard").unwrap().intercepted = true;
        e.devices.get_mut("mouse").unwrap().online = false;
        let (cmds, errors) = e.from_event("keyboard", 1, Instant::now());
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, "keyboard");
        assert_eq!(errors.len(), 1);
        let (cmds, _) = e.from_event("keyboard", 2, Instant::now());
        assert_eq!(cmds[0].target, 2);
    }
    #[test]
    fn pause_disconnect_and_reconnect_generation() {
        let mut e = engine();
        e.paused = true;
        assert!(e.from_event("keyboard", 1, Instant::now()).0.is_empty());
        e.paused = false;
        e.disconnect("bolt");
        assert!(e.from_event("keyboard", 1, Instant::now()).0.is_empty());
        for d in e.devices.values_mut() {
            d.online = true;
            d.generation += 1;
        }
        assert_eq!(e.from_event("keyboard", 1, Instant::now()).0.len(), 1);
    }
    #[test]
    fn host_announcement_requires_correct_feature_and_departing_host() {
        let d = device("keyboard", 1);
        let mut wire = crate::protocol::request(1, d.change_host, 0, &[0, 1]).unwrap();
        wire[3] = 0;
        assert_eq!(
            crate::protocol::switch_event(
                &crate::protocol::Report::parse(&wire).unwrap(),
                &d,
                false
            ),
            Some(1)
        );
        wire[2] += 1;
        assert_eq!(
            crate::protocol::switch_event(
                &crate::protocol::Report::parse(&wire).unwrap(),
                &d,
                false
            ),
            None
        );
    }
}
