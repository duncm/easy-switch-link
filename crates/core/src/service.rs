//! Local system-service contracts. Never exposed on the LAN protocol.
use crate::{Request, Response, Status, config::Config, follow::FollowStatus};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const LIMIT: usize = 256 * 1024;
pub const POLICY: &str = "/etc/easy-switch-link/policy.json";
pub const HARDWARE_SOCKET: &str = "/run/easy-switch-link/hardware/control.sock";
pub const PEER_SOCKET: &str = "/run/easy-switch-link/peer/control.sock";
pub const DESKTOP_SOCKET: &str = "/run/easy-switch-link/peer/desktop.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    pub owner_uid: u32,
    pub receiver_uid: u32,
    pub peer_uid: u32,
    pub greeter_uid: u32,
    pub seat: String,
    pub login_handoffs: bool,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "Unsupported service policy version"
        );
        ensure!(
            self.owner_uid != 0 && self.receiver_uid != 0 && self.peer_uid != 0,
            "Service identities must not be root"
        );
        ensure!(
            self.owner_uid != self.receiver_uid
                && self.owner_uid != self.peer_uid
                && self.peer_uid != self.receiver_uid,
            "Service identities must be distinct"
        );
        ensure!(
            !self.seat.is_empty()
                && self.seat.len() <= 64
                && self
                    .seat
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid seat"
        );
        ensure!(
            !self.login_handoffs
                || self.greeter_uid != 0
                    && ![self.owner_uid, self.receiver_uid, self.peer_uid]
                        .contains(&self.greeter_uid),
            "Invalid greeter identity"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub mode: String,
    pub owner_uid: Option<u32>,
    pub seat: String,
    pub login_handoffs: bool,
    pub generation: u64,
    pub instance: String,
    pub observation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Watch,
    User {
        request: Request,
    },
    Coordinator {
        state: Box<FollowStatus>,
    },
    Layout {
        generation: u64,
        config: Config,
    },
    Edge {
        instance: String,
        generation: u64,
        transaction: String,
        rule: String,
        session: String,
        desktop_generation: u64,
        deadline_ns: u64,
    },
    Cancel,
    Supersede,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Packet {
    pub version: u32,
    pub control: Control,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Notice {
    RestoreCursor,
    Snapshot {
        status: Status,
    },
    Departure {
        transaction: String,
        written_ns: u64,
    },
    Reply {
        response: Response,
    },
}

/// A coordinator can refresh bindings, never change rules, enrollment or policy.
pub fn validate_layout_update(old: &Config, proposed: &Config) -> Result<()> {
    proposed.validate()?;
    let mut permitted = old.clone();
    ensure!(
        permitted.follow.rules.len() == proposed.follow.rules.len(),
        "Layout update changed rules"
    );
    for (a, b) in permitted
        .follow
        .rules
        .iter_mut()
        .zip(&proposed.follow.rules)
    {
        a.source.binding = b.source.binding.clone();
        a.destination.binding = b.destination.binding.clone();
    }
    ensure!(
        &permitted == proposed,
        "Layout update changed more than display bindings"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_non_root_service_identities() {
        let mut p = Policy {
            version: 1,
            owner_uid: 1000,
            receiver_uid: 901,
            peer_uid: 902,
            greeter_uid: 120,
            seat: "seat0".into(),
            login_handoffs: true,
        };
        p.validate().unwrap();
        p.peer_uid = p.receiver_uid;
        assert!(p.validate().is_err());
        p.peer_uid = 0;
        assert!(p.validate().is_err());
    }
    #[test]
    fn coordinator_cannot_change_hardware_configuration() {
        let c = Config::default();
        let mut changed = c.clone();
        changed.start_at_login = !changed.start_at_login;
        assert!(validate_layout_update(&c, &changed).is_err());
        validate_layout_update(&c, &c).unwrap();
    }
}
