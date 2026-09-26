use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(windows)]
pub use windows::*;

pub const RECEIVER_PIDS: &[u16] = &[0xc548, 0xc52b, 0xc532];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub path: String,
    pub input_len: usize,
    pub output_len: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiverInfo {
    pub id: String,
    pub product_id: u16,
    pub endpoints: Vec<Endpoint>,
}

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// One owner serializes writes and receives replies/notifications on all collections.
pub trait Transport: Send {
    fn write(&mut self, bytes: &[u8]) -> Result<()>;
    /// None indicates a control wake or timeout, not a device disconnect.
    fn read(&mut self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>>;
    fn waker(&self) -> Wake;
}

pub mod desktop;
#[cfg(target_os = "linux")]
pub mod system;
