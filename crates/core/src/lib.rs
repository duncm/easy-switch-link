pub mod config;
pub mod engine;
pub mod follow;
pub mod protocol;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    #[serde(default)]
    pub kind: String,
    pub id: String,
    pub receiver: String,
    pub slot: u8,
    pub name: String,
    pub host_count: u8,
    /// Zero-based wire index; UI/config use 1-based channels.
    pub current_host: u8,
    pub change_host: u8,
    pub reprog: Option<u8>,
    pub wireless_status: Option<u8>,
    pub controls: Vec<Control>,
    #[serde(default)]
    pub intercepted: bool,
    pub online: bool,
    pub generation: u64,
    pub capability: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Control {
    pub cid: u16,
    pub divertable: bool,
    pub analytics: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub follow: follow::FollowStatus,
    pub paused: bool,
    pub configured: bool,
    pub discovering: usize,
    pub devices: Vec<Device>,
    pub receivers: BTreeMap<String, String>,
    pub messages: Vec<String>,
    pub config: config::Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Follow { action: follow::Action },
    Status,
    Pause,
    Resume,
    Switch { group: String, computer: String },
    Save { config: config::Config },
    Recover,
    Rescan,
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    pub status: Option<Status>,
}
