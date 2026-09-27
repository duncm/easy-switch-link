use anyhow::{Context, Result, bail, ensure};
use crossbeam_channel::{Receiver, Sender};
use easy_switch_core::{
    Control, Device,
    config::{Config, SourceMode},
    engine::SwitchCommand,
    protocol::{self, Report},
};
use easy_switch_platform::{ReceiverInfo, Transport, Wake};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// Waking Bolt devices can take over a second to answer. Switches use a separate
// bounded queue serviced even while a discovery/settings transaction is waiting.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum Event {
    #[cfg(target_os = "linux")]
    SystemControl(Box<crate::service::Envelope>),
    #[cfg(target_os = "linux")]
    CoordinatorStatus(Box<easy_switch_core::follow::FollowStatus>),
    #[cfg(target_os = "linux")]
    SystemDisconnected(String),
    #[cfg(target_os = "linux")]
    SystemDeparture {
        transaction: String,
        written_ns: u64,
    },
    #[cfg(target_os = "linux")]
    SessionChanged,
    #[cfg(target_os = "linux")]
    Supersede(tokio::sync::oneshot::Sender<Result<(), String>>),
    LayoutConfig {
        generation: u64,
        config: Config,
    },
    Device(Device),
    Report {
        receiver: String,
        report: Report,
        generation: Option<u64>,
        epoch: u64,
        received_at: Instant,
    },
    Conflict(String, u64, String),
    Ready(String, u64),
    Lost(String, u64, String),
    Message(String),
}
pub enum Command {
    Configure {
        config: Config,
        paused: bool,
        epoch: u64,
    },
    Switch {
        command: SwitchCommand,
        epoch: u64,
        received_at: Instant,
        departure: Option<Box<dyn FnOnce(Instant) + Send + Sync>>,
        recovery: Option<RecoveryPermission>,
    },
    Recover,
    Refresh,
    Stop,
}
pub struct Worker {
    pub session: u64,
    pub tx: Sender<Command>,
    pub switches: Sender<PendingSwitch>,
    pub wake: Wake,
    pub join: Option<std::thread::JoinHandle<()>>,
    pub epoch: Arc<AtomicU64>,
    pub switch_epoch: Arc<AtomicU64>,
}
pub struct PendingSwitch {
    command: SwitchCommand,
    epoch: u64,
    switch_epoch: u64,
    received_at: Instant,
    departure: Option<Box<dyn FnOnce(Instant) + Send + Sync>>,
    recovery: Option<RecoveryPermission>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let (replacement, _) = crossbeam_channel::bounded(0);
        let sender = std::mem::replace(&mut self.tx, replacement);
        let _ = sender.try_send(Command::Stop);
        drop(sender);
        (self.wake)();
    }
}
impl Worker {
    /// Cancel queued switches without invalidating the applied configuration or
    /// suppressing subsequent device notifications.
    pub fn cancel_switches(&self) {
        self.switch_epoch.fetch_add(1, Ordering::AcqRel);
        (self.wake)();
    }
    pub fn send(&self, command: Command) -> Result<()> {
        if let Command::Switch {
            command,
            epoch,
            received_at,
            departure,
            recovery,
        } = command
        {
            self.switches
                .try_send(PendingSwitch {
                    command,
                    epoch,
                    switch_epoch: self.switch_epoch.load(Ordering::Acquire),
                    received_at,
                    departure,
                    recovery,
                })
                .context("Receiver switch queue is full or disconnected")?;
        } else {
            self.tx
                .try_send(command)
                .context("Receiver command queue is full or disconnected")?;
        }
        (self.wake)();
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Restoration {
    id: String,
    cid: u16,
    analytics: bool,
    previous: bool,
}

/// Device identities, not USB paths: recovery also works after moving a receiver.
pub struct Journal {
    path: PathBuf,
    entries: Mutex<Vec<Restoration>>,
}
impl Journal {
    pub fn open(path: PathBuf) -> Result<Arc<Self>> {
        let entries =
            crate::storage::read_json(&path).context("Cannot read reporting recovery journal")?;
        Ok(Arc::new(Self {
            path,
            entries: Mutex::new(entries),
        }))
    }
    fn entries(&self) -> Result<Vec<Restoration>> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("Recovery lock poisoned"))?
            .clone())
    }
    fn remember(&self, entry: Restoration) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("Recovery lock poisoned"))?;
        let mut next = entries.clone();
        if !next.iter().any(|e| e.id == entry.id && e.cid == entry.cid) {
            next.push(entry);
        }
        crate::storage::write_json(&self.path, &next)?;
        *entries = next;
        Ok(())
    }
    fn forget(&self, id: &str, cid: u16) -> Result<()> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("Recovery lock poisoned"))?;
        let next: Vec<_> = entries
            .iter()
            .filter(|e| !(e.id == id && e.cid == cid))
            .cloned()
            .collect();
        crate::storage::write_json(&self.path, &next)?;
        *entries = next;
        Ok(())
    }
}

pub fn spawn(
    info: ReceiverInfo,
    events: Sender<Event>,
    journal: Arc<Journal>,
    config: Config,
    paused: bool,
    generation: u64,
) -> Result<Worker> {
    let transport = easy_switch_platform::open(&info)?;
    let wake = transport.waker();
    let (tx, rx) = crossbeam_channel::bounded(64);
    let (switch_tx, switch_rx) = crossbeam_channel::bounded(64);
    let epoch = Arc::new(AtomicU64::new(1));
    let active_epoch = epoch.clone();
    let switch_epoch = Arc::new(AtomicU64::new(1));
    let active_switch_epoch = switch_epoch.clone();
    let join = std::thread::Builder::new()
        .name("hid-receiver".into())
        .spawn(move || {
            let id = info.id.clone();
            let mut session = Session {
                info,
                transport,
                events: events.clone(),
                devices: BTreeMap::new(),
                reprobe: BTreeSet::new(),
                journal,
                config,
                paused,
                epoch: 1,
                active_epoch,
                active_switch_epoch,
                generation,
                armed: BTreeMap::new(),
                reporting_checks: BTreeSet::new(),
                next_software_id: 8,
                switches: switch_rx,
                retries: BTreeMap::new(),
                device_input: BTreeMap::new(),
            };
            if let Err(error) = session.run(rx) {
                let _ = events.send(Event::Lost(id, generation >> 32, format!("{error:#}")));
            }
        })?;
    Ok(Worker {
        session: generation >> 32,
        tx,
        switches: switch_tx,
        wake,
        join: Some(join),
        epoch,
        switch_epoch,
    })
}

struct Session {
    info: ReceiverInfo,
    transport: Box<dyn Transport>,
    events: Sender<Event>,
    devices: BTreeMap<u8, Device>,
    reprobe: BTreeSet<u8>,
    journal: Arc<Journal>,
    config: Config,
    paused: bool,
    epoch: u64,
    active_epoch: Arc<AtomicU64>,
    active_switch_epoch: Arc<AtomicU64>,
    generation: u64,
    // Value is true for passive analytics, false for temporary diversion.
    armed: BTreeMap<(u8, u16), bool>,
    reporting_checks: BTreeSet<(u8, u16)>,
    next_software_id: u8,
    switches: Receiver<PendingSwitch>,
    retries: BTreeMap<u8, FollowerRetry>,
    device_input: BTreeMap<u8, u64>,
}

impl Session {
    fn message(&self, text: impl Into<String>) {
        let _ = self.events.send(Event::Message(text.into()));
    }
    fn run(&mut self, commands: Receiver<Command>) -> Result<()> {
        // Read-modify-write the receiver's HID++ 1.0 notification flags.
        if let Ok(flags) = self.register(0x81, 0x00, &[])
            && flags.len() >= 3
        {
            let _ = self.register(0x80, 0x00, &[flags[0], flags[1] | 0x09, flags[2]]);
        }
        // Ask the receiver to announce paired devices; active slots are also probed once.
        let _ = self.register(0x80, 0x02, &[0x02]);
        for slot in 1..=6 {
            if let Err(error) = self.probe(slot) {
                tracing::debug!(slot, "Probe: {error:#}");
                if self.reprobe.contains(&slot) {
                    self.message(format!(
                        "Connected receiver slot {slot} could not be identified: {error:#}"
                    ));
                }
            } else {
                self.reprobe.remove(&slot);
            }
        }
        self.restore()?;
        self.configure()?;
        self.events
            .send(Event::Ready(self.info.id.clone(), self.generation >> 32))?;
        loop {
            self.dispatch_pending_switches()?;
            self.service_retry()?;
            loop {
                let command = match commands.try_recv() {
                    Ok(command) => command,
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        self.restore()?;
                        return Ok(());
                    }
                };
                match command {
                    Command::Stop => {
                        self.restore()?;
                        return Ok(());
                    }
                    Command::Recover => {
                        self.paused = true;
                        self.restore()?;
                        self.message("Reporting recovery completed for reachable devices; monitoring remains paused");
                    }
                    Command::Refresh => {
                        self.restore()?;
                        for d in self.devices.values_mut() {
                            d.online = false;
                            let _ = self.events.send(Event::Device(d.clone()));
                        }
                        for slot in 1..=6 {
                            let _ = self.probe(slot);
                        }
                        self.reprobe.clear();
                        self.restore()?;
                        self.configure()?;
                        self.events
                            .send(Event::Ready(self.info.id.clone(), self.generation >> 32))?;
                    }
                    Command::Configure {
                        config,
                        paused,
                        epoch,
                    } => {
                        self.restore()?;
                        self.config = config;
                        self.paused = paused;
                        self.epoch = epoch;
                        self.configure()?;
                    }
                    Command::Switch {
                        command,
                        epoch,
                        received_at,
                        departure,
                        recovery,
                    } => {
                        self.dispatch_switch(PendingSwitch {
                            command,
                            epoch,
                            switch_epoch: self.active_switch_epoch.load(Ordering::Acquire),
                            received_at,
                            departure,
                            recovery,
                        })?;
                    }
                }
            }
            if let Some(slot) = self.reprobe.pop_first() {
                if let Err(e) = self.probe(slot) {
                    tracing::debug!(slot, "Reconnect probe: {e:#}");
                }
                self.reprobe.remove(&slot);
                self.restore()?;
                self.configure()?;
                continue;
            }
            if let Some((slot, cid)) = self.reporting_checks.pop_first() {
                self.check_reporting(slot, cid)?;
                continue;
            }
            let timeout = self.retry_wait(None);
            if let Some(bytes) = self.transport.read(timeout)? {
                self.receive(&bytes);
            }
        }
    }
    fn receive(&mut self, bytes: &[u8]) {
        let received_at = Instant::now();
        let Ok(report) = Report::parse(bytes) else {
            return;
        };
        tracing::trace!(receiver = %self.info.id, slot = report.device, feature = report.feature, address = report.address, "HID++ report header (input payload omitted)");
        if report.report_id == 0x10 && report.feature == 0x41 && report.payload.len() >= 3 {
            self.reporting_checks
                .retain(|(slot, _)| *slot != report.device);
            let connected = report.payload[0] & 0x40 == 0;
            if connected {
                self.reprobe.insert(report.device);
            } else if let Some(d) = self.devices.get_mut(&report.device) {
                d.online = false;
                let _ = self.events.send(Event::Device(d.clone()));
            }
        }
        if let Some(d) = self.devices.get(&report.device) {
            // Easy-Switch/diverted controls may bypass ordinary OS keyboard input.
            // Count their notifications even while cached availability is stale.
            if report.notification() && Some(report.feature) == d.reprog {
                let serial = self.device_input.entry(report.device).or_default();
                *serial = serial.wrapping_add(1);
            }
            if report.notification() && Some(report.feature) == d.wireless_status {
                self.reprobe.insert(report.device);
                self.reporting_checks
                    .retain(|(slot, _)| *slot != report.device);
            }
            // A setCidReporting reply only echoes a masked update, not the resulting
            // state. Other applications routinely write unrelated bits on reconnect.
            // Software IDs are not ownership tokens either. Verify the actual state
            // outside this receive/transaction path before declaring a conflict.
            if Some(report.feature) == d.reprog
                && report.function() == 3
                && report.address & 0xf != 0
                && report.payload.len() >= 5
                && d.online
                && !self.paused
                && !self.reprobe.contains(&report.device)
            {
                let cid = u16::from_be_bytes([report.payload[0], report.payload[1]]);
                if self.armed.contains_key(&(report.device, cid)) {
                    self.reporting_checks.insert((report.device, cid));
                }
            }
        }
        let generation = self.devices.get(&report.device).map(|d| d.generation);
        let _ = self.events.send(Event::Report {
            receiver: self.info.id.clone(),
            report,
            generation,
            epoch: self.epoch,
            received_at,
        });
    }
    fn dispatch_pending_switches(&mut self) -> Result<()> {
        // Bounded work; the same thread remains the sole HID transport owner.
        for _ in 0..64 {
            let Ok(pending) = self.switches.try_recv() else {
                break;
            };
            self.dispatch_switch(pending)?;
        }
        Ok(())
    }
    fn dispatch_switch(&mut self, pending: PendingSwitch) -> Result<()> {
        let PendingSwitch {
            command,
            epoch,
            switch_epoch,
            received_at,
            departure,
            recovery,
        } = pending;
        if switch_epoch != self.active_switch_epoch.load(Ordering::Acquire) {
            return Ok(());
        }
        self.retries.remove(&command.slot);
        if let Some(permission) = recovery {
            if permission.epoch.load(Ordering::SeqCst) != permission.version {
                return Ok(());
            }
            self.retries.insert(
                command.slot,
                FollowerRetry {
                    command: command.clone(),
                    epoch,
                    switch_epoch,
                    permission,
                    checks: 0,
                    input_serial: self.device_input.get(&command.slot).copied().unwrap_or(0),
                    due: Instant::now() + Duration::from_millis(250),
                    until: Instant::now() + Duration::from_secs(3),
                },
            );
        }
        if epoch != self.active_epoch.load(Ordering::Acquire)
            || epoch != self.epoch
            || self.paused
            || command.expires.is_some_and(|t| Instant::now() >= t)
        {
            return Ok(());
        }
        let Some(d) = self.devices.get(&command.slot).filter(|d| {
            d.online
                && d.id == command.id
                && d.generation == command.generation
                && d.change_host == command.feature
                && command.target < d.host_count
        }) else {
            return Ok(());
        };
        let name = d.name.clone();
        if switch_epoch != self.active_switch_epoch.load(Ordering::Acquire) {
            return Ok(());
        }
        let write_started = Instant::now();
        let result = self.transport.write(&protocol::request(
            command.slot,
            command.feature,
            1,
            &[command.target],
        )?);
        let completed = Instant::now();
        match result {
            Ok(()) => {
                if let Some(notify_departure) = departure {
                    notify_departure(completed);
                }
                let dispatch_us =
                    completed.saturating_duration_since(received_at).as_micros() as u64;
                tracing::info!(transaction=command.transaction.as_deref().unwrap_or("physical-key"), device = %name, target_channel = command.target + 1,
                    event_to_write_us = dispatch_us,
                    hid_write_us = completed.duration_since(write_started).as_micros() as u64,
                    "Switch command dispatched");
                self.message(format!(
                    "Sent {name} to channel {} ({:.1} ms dispatch; arrival unverified)",
                    command.target + 1,
                    dispatch_us as f64 / 1000.0
                ));
            }
            Err(e) => self.message(format!(
                "Could not switch {name} (transaction {}): {e:#}",
                command.transaction.as_deref().unwrap_or("physical-key")
            )),
        }
        Ok(())
    }
    fn check_reporting(&mut self, slot: u8, cid: u16) -> Result<()> {
        let Some(&analytics) = self.armed.get(&(slot, cid)) else {
            return Ok(());
        };
        let Some(d) = self.devices.get(&slot).filter(|d| d.online).cloned() else {
            return Ok(());
        };
        let Some(feature) = d.reprog else {
            return Ok(());
        };
        if self.paused || self.reprobe.contains(&slot) {
            return Ok(());
        }
        let Ok(state) = self.call(slot, feature, 2, &cid.to_be_bytes()) else {
            // A timeout or disconnect is not evidence of a configuration conflict.
            return Ok(());
        };
        if self.paused
            || self.reprobe.contains(&slot)
            || !self
                .devices
                .get(&slot)
                .is_some_and(|now| now.online && now.generation == d.generation)
            || protocol::reporting_compatible(&state, cid, analytics) != Some(false)
        {
            return Ok(());
        }
        tracing::warn!(device = %d.name, cid, analytics, flags = state[2], extra_flags = state.get(5), "Confirmed incompatible Easy-Switch reporting state");
        self.paused = true;
        self.reporting_checks.clear();
        self.events.send(Event::Conflict(
            self.info.id.clone(),
            self.generation >> 32,
            d.name,
        ))?;
        Ok(())
    }
    fn transact(&mut self, packet: impl AsRef<[u8]>) -> Result<Vec<u8>> {
        // Verification runs between transactions, never recursively while another
        // reply is outstanding. Urgent switch writes still interrupt either wait.
        self.dispatch_pending_switches()?;
        self.service_retry()?;
        self.transact_until(packet, Instant::now() + REQUEST_TIMEOUT)
    }
    fn transact_until(&mut self, packet: impl AsRef<[u8]>, deadline: Instant) -> Result<Vec<u8>> {
        self.dispatch_pending_switches()?;
        ensure!(Instant::now() < deadline, "Request expired before write");
        let mut packet = packet.as_ref().to_vec();
        if packet[1] != 0xff {
            packet[3] = (packet[3] & 0xf0) | self.next_software_id;
            self.next_software_id = 8 + (self.next_software_id + 1) % 8;
        }
        self.transport.write(&packet)?;
        while Instant::now() < deadline {
            // The transport wake interrupts read() when a switch arrives. Do not
            // resume waiting for an unrelated reply before servicing that queue.
            self.dispatch_pending_switches()?;
            let wait = Some(deadline.saturating_duration_since(Instant::now()));
            let Some(bytes) = self.transport.read(wait)? else {
                continue;
            };
            if let Ok(report) = Report::parse(&bytes) {
                if let Some(code) = report.error_for(packet[1], packet[2], packet[3]) {
                    bail!("HID++ error {code:#04x}");
                }
                if report.matches(packet[1], packet[2], packet[3]) {
                    return Ok(report.payload);
                }
            }
            // Notifications must not be lost while a request is in flight.
            self.receive(&bytes);
        }
        bail!("HID++ request timed out")
    }
    fn register(&mut self, operation: u8, register: u8, params: &[u8]) -> Result<Vec<u8>> {
        let mut packet = [0; 7];
        packet[..4].copy_from_slice(&[0x10, 0xff, operation, register]);
        packet[4..4 + params.len()].copy_from_slice(params);
        self.transact(packet)
    }
    fn call(&mut self, slot: u8, feature: u8, function: u8, params: &[u8]) -> Result<Vec<u8>> {
        self.transact(protocol::request(slot, feature, function, params)?)
    }
    fn feature(&mut self, slot: u8, id: u16) -> Result<u8> {
        let bytes = id.to_be_bytes();
        let reply = self.call(slot, 0, 0, &bytes)?;
        let index = *reply.first().context("Empty feature reply")?;
        ensure!(index != 0, "Feature {id:#06x} unavailable");
        Ok(index)
    }
    fn probe(&mut self, slot: u8) -> Result<()> {
        let change_host = self.feature(slot, protocol::CHANGE_HOST)?;
        let host = self.call(slot, change_host, 0, &[])?;
        ensure!(
            host.len() >= 2 && (1..=3).contains(&host[0]) && host[1] < host[0],
            "Invalid host capabilities"
        );
        let ids_feature = self.feature(slot, 3)?;
        let ids = self.call(slot, ids_feature, 0, &[])?;
        ensure!(
            ids.len() >= 13 && ids[1..5].iter().any(|b| *b != 0),
            "No stable device unit ID; not safe to enroll this device"
        );
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let id = format!("unit:{}:{}", hex(&ids[1..5]), hex(&ids[7..13]));
        let name_index = self.feature(slot, 5)?;
        let length = self
            .call(slot, name_index, 0, &[])?
            .first()
            .copied()
            .unwrap_or(0) as usize;
        ensure!(length <= 128, "Device name too long");
        let mut name = Vec::new();
        while name.len() < length {
            let part = self.call(slot, name_index, 1, &[name.len() as u8])?;
            ensure!(!part.is_empty(), "Empty name response");
            name.extend_from_slice(&part[..part.len().min(length - name.len())]);
        }
        let reprog = self.feature(slot, protocol::REPROG).ok();
        let wireless_status = self.feature(slot, 0x1d4b).ok();
        let mut controls = Vec::new();
        if let Some(feature) = reprog
            && let Ok(reply) = self.call(slot, feature, 0, &[])
        {
            for index in 0..reply.first().copied().unwrap_or(0).min(128) {
                if let Ok(info) = self.call(slot, feature, 1, &[index]) {
                    if info.len() < 5 {
                        continue;
                    }
                    let cid = u16::from_be_bytes([info[0], info[1]]);
                    if protocol::cid_host(cid).is_some() {
                        controls.push(Control {
                            cid,
                            divertable: info[4] & 0x20 != 0,
                            analytics: protocol::analytics_capable(&info),
                        });
                    }
                }
            }
        }
        self.generation += 1;
        let capability = if controls.iter().any(|c| c.analytics) {
            "Passive key reporting; host announcements listened for"
        } else if controls.iter().any(|c| c.divertable) {
            "Host announcements; temporary interception available"
        } else {
            "Can follow; host announcement support requires verification"
        }
        .into();
        let kind = self
            .call(slot, name_index, 2, &[])
            .ok()
            .and_then(|v| v.first().copied())
            .map(|k| match k {
                0 => "keyboard",
                3 => "mouse",
                _ => "other",
            })
            .unwrap_or("unknown")
            .to_owned();
        let device = Device {
            kind,
            id,
            receiver: self.info.id.clone(),
            slot,
            name: String::from_utf8_lossy(&name).into(),
            host_count: host[0],
            current_host: host[1],
            change_host,
            reprog,
            wireless_status,
            controls,
            intercepted: false,
            online: true,
            generation: self.generation,
            capability,
        };
        self.devices.insert(slot, device.clone());
        self.events.send(Event::Device(device))?;
        Ok(())
    }
    fn reporting(
        &mut self,
        slot: u8,
        feature: u8,
        cid: u16,
        analytics: bool,
        enabled: bool,
    ) -> Result<()> {
        let [hi, lo] = cid.to_be_bytes();
        let params = if analytics {
            vec![hi, lo, 0, 0, 0, if enabled { 3 } else { 2 }]
        } else {
            vec![hi, lo, if enabled { 3 } else { 2 }, 0, 0]
        };
        self.call(slot, feature, 3, &params)?;
        Ok(())
    }
    fn restore(&mut self) -> Result<()> {
        self.reporting_checks.clear();
        let saved = self.journal.entries()?;
        for entry in saved {
            let target = self
                .devices
                .values()
                .find(|d| d.online && d.id == entry.id)
                .cloned();
            if let Some(d) = target
                && let Some(feature) = d.reprog
                && self
                    .reporting(d.slot, feature, entry.cid, entry.analytics, entry.previous)
                    .is_ok()
            {
                self.armed.remove(&(d.slot, entry.cid));
                self.journal.forget(&entry.id, entry.cid)?;
                continue;
            }
        }
        for device in self.devices.values_mut() {
            if device.intercepted && !self.armed.keys().any(|(slot, _)| *slot == device.slot) {
                device.intercepted = false;
                let _ = self.events.send(Event::Device(device.clone()));
            }
        }
        Ok(())
    }
    fn configure(&mut self) -> Result<()> {
        if self.paused {
            return Ok(());
        }
        let devices: Vec<_> = self
            .devices
            .values()
            .filter(|d| d.online)
            .cloned()
            .collect();
        for d in devices {
            let Some(member) = self.config.member(&d.id) else {
                continue;
            };
            let mode = member.mode;
            if mode == SourceMode::FollowOnly {
                continue;
            }
            let Some(feature) = d.reprog else {
                continue;
            };
            for control in &d.controls {
                let analytics = mode == SourceMode::Auto && d.controls.iter().any(|c| c.analytics);
                if (analytics && !control.analytics) || (!analytics && !control.divertable) {
                    continue;
                }
                if self
                    .journal
                    .entries()?
                    .iter()
                    .any(|r| r.id == d.id && r.cid == control.cid)
                {
                    continue;
                }
                let Ok(existing) = self.call(d.slot, feature, 2, &control.cid.to_be_bytes()) else {
                    continue;
                };
                if existing.len() < 5 || existing[..2] != control.cid.to_be_bytes() {
                    continue;
                }
                if existing[2] & 5 != 0 {
                    self.message(format!(
                        "{} Easy-Switch key is already diverted by another application",
                        d.name
                    ));
                    continue;
                }
                let previous = if analytics {
                    existing.get(5).is_some_and(|b| b & 1 != 0)
                } else {
                    existing[2] & 1 != 0
                };
                if previous {
                    continue;
                }
                // Persist before touching device state, so termination between the write and ACK is recoverable.
                self.journal.remember(Restoration {
                    id: d.id.clone(),
                    cid: control.cid,
                    analytics,
                    previous,
                })?;
                match self.reporting(d.slot, feature, control.cid, analytics, true) {
                    Ok(()) => {
                        self.armed.insert((d.slot, control.cid), analytics);
                        if !analytics && let Some(device) = self.devices.get_mut(&d.slot) {
                            device.intercepted = true;
                            let _ = self.events.send(Event::Device(device.clone()));
                        }
                    }
                    Err(e) => self.message(format!(
                        "{} reporting setup failed: {e:#}; host announcements remain available",
                        d.name
                    )),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeTransport {
        frames: std::collections::VecDeque<Vec<u8>>,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        fail: bool,
    }
    impl Transport for FakeTransport {
        fn write(&mut self, bytes: &[u8]) -> Result<()> {
            self.writes.lock().unwrap().push(bytes.to_vec());
            if self.fail {
                bail!("Transport disappeared");
            }
            Ok(())
        }
        fn read(&mut self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>> {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(Some(frame));
            }
            if let Some(duration) = timeout {
                std::thread::sleep(duration);
            }
            Ok(None)
        }
        fn waker(&self) -> Wake {
            Arc::new(|| {})
        }
    }
    fn session(
        dir: &std::path::Path,
        frames: Vec<Vec<u8>>,
        fail: bool,
    ) -> (Session, Receiver<Event>) {
        let (tx, rx) = crossbeam_channel::bounded(32);
        (
            Session {
                info: ReceiverInfo {
                    id: "test-bolt".into(),
                    product_id: 0xc548,
                    endpoints: vec![],
                },
                transport: Box::new(FakeTransport {
                    frames: frames.into(),
                    writes: Arc::default(),
                    fail,
                }),
                events: tx,
                devices: BTreeMap::new(),
                reprobe: BTreeSet::new(),
                journal: Journal::open(dir.join("recovery.json")).unwrap(),
                config: Config::default(),
                paused: false,
                epoch: 1,
                active_epoch: Arc::new(AtomicU64::new(1)),
                active_switch_epoch: Arc::new(AtomicU64::new(1)),
                generation: 1,
                armed: BTreeMap::new(),
                reporting_checks: BTreeSet::new(),
                next_software_id: 8,
                switches: crossbeam_channel::never(),
                retries: BTreeMap::new(),
                device_input: BTreeMap::new(),
            },
            rx,
        )
    }
    fn device() -> Device {
        Device {
            kind: String::new(),
            id: "unit:abc:model".into(),
            receiver: "test-bolt".into(),
            slot: 2,
            name: "test keyboard".into(),
            host_count: 3,
            current_host: 0,
            change_host: 10,
            reprog: Some(9),
            wireless_status: None,
            controls: vec![],
            intercepted: false,
            online: true,
            generation: 1,
            capability: String::new(),
        }
    }
    fn reporting_packet(function: u8, software_id: u8, state: &[u8]) -> Vec<u8> {
        let mut packet = protocol::request(2, 9, function, state).unwrap();
        packet[3] = function << 4 | software_id;
        packet.to_vec()
    }
    fn pending_switch() -> PendingSwitch {
        PendingSwitch {
            command: SwitchCommand {
                expires: None,
                transaction: None,
                id: device().id,
                receiver: device().receiver,
                slot: 2,
                feature: 10,
                target: 1,
                generation: 1,
            },
            epoch: 1,
            switch_epoch: 1,
            received_at: Instant::now(),
            departure: None,
            recovery: None,
        }
    }
    #[test]
    fn follower_recovery_requires_confirmed_source_identity_idle_input_and_current_handoff() {
        use std::{collections::VecDeque, sync::atomic::AtomicBool};
        // success, already there, wrong identity, input, newer handoff, offline
        // cache, expired initial command, wrong source, input during verification.
        for case in 0..11 {
            let dir = tempfile::tempdir().unwrap();
            let (mut s, _events) = session(dir.path(), vec![], false);
            let mut d = device();
            d.id = "unit:01020304:05060708090a".into();
            d.online = case != 5;
            s.devices.insert(2, d.clone());
            s.config.follow.local_computer = "computer-1".into();
            s.config.groups.push(easy_switch_core::config::Group {
                id: "desk".into(),
                devices: vec![easy_switch_core::config::Member {
                    id: d.id.clone(),
                    channels: [("computer-1".into(), 1), ("computer-2".into(), 2)].into(),
                    mode: easy_switch_core::config::SourceMode::Auto,
                }],
            });
            let idle = Arc::new(AtomicBool::new(true));
            let observed = idle.clone();
            let epoch = Arc::new(AtomicU64::new(1));
            let writes = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
            let mut ids = vec![0, 1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 9, 10];
            if case == 2 {
                ids[1] = 9;
            }
            let mut frames = VecDeque::new();
            for (feature, software, payload) in [
                (0, 8, vec![7]),
                (7, 9, ids),
                (0, 10, vec![11]),
                (
                    11,
                    11,
                    vec![
                        3,
                        if case == 1 {
                            1
                        } else if case == 7 {
                            2
                        } else {
                            0
                        },
                    ],
                ),
            ] {
                let mut packet = protocol::request(2, feature, 0, &payload).unwrap().to_vec();
                packet[3] = software;
                frames.push_back(packet);
            }
            if case == 10 {
                let mut input = protocol::request(2, 9, 2, &[0, 0xd1, 1]).unwrap().to_vec();
                input[3] = 0x20;
                frames.push_front(input);
            }
            struct VerifyTransport {
                frames: VecDeque<Vec<u8>>,
                writes: Arc<Mutex<Vec<Vec<u8>>>>,
                input_on_host_reply: bool,
                idle: Arc<AtomicBool>,
            }
            impl Transport for VerifyTransport {
                fn write(&mut self, b: &[u8]) -> Result<()> {
                    self.writes.lock().unwrap().push(b.to_vec());
                    Ok(())
                }
                fn read(&mut self, _: Option<Duration>) -> Result<Option<Vec<u8>>> {
                    let next = self.frames.pop_front();
                    if self.input_on_host_reply && self.frames.is_empty() {
                        self.idle.store(false, Ordering::SeqCst);
                    }
                    Ok(next)
                }
                fn waker(&self) -> Wake {
                    Arc::new(|| {})
                }
            }
            s.transport = Box::new(VerifyTransport {
                frames,
                writes: writes.clone(),
                input_on_host_reply: case == 8,
                idle: idle.clone(),
            });
            let mut pending = pending_switch();
            pending.command.id = d.id;
            if case == 6 {
                pending.command.expires = Some(Instant::now());
            }
            pending.recovery = Some(RecoveryPermission {
                activity: Arc::new(move || observed.load(Ordering::SeqCst)),
                epoch: epoch.clone(),
                version: 1,
            });
            s.dispatch_switch(pending).unwrap();
            writes.lock().unwrap().clear();
            if case == 3 {
                idle.store(false, Ordering::SeqCst);
            }
            if case == 4 {
                epoch.store(2, Ordering::SeqCst);
            }
            if case == 9 {
                let mut input = protocol::request(2, 9, 2, &[0, 0xd1, 1]).unwrap();
                input[3] = 0x20;
                s.receive(&input);
            }
            s.retries.get_mut(&2).unwrap().due = Instant::now();
            s.service_retry().unwrap();
            let writes = writes.lock().unwrap();
            let switches: Vec<_> = writes.iter().filter(|b| b[3] >> 4 == 1).collect();
            assert_eq!(
                switches.len(),
                usize::from(matches!(case, 0 | 5 | 6)),
                "case {case}"
            );
            if let Some(command) = switches.first() {
                assert_eq!(command[2], 11, "Resolve fresh feature index");
            }
        }
    }
    #[test]
    fn unreachable_follower_is_not_retried_and_verification_stops_after_two_checks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _events) = session(dir.path(), vec![], false);
        let writes = Arc::new(Mutex::new(Vec::new()));
        s.transport = Box::new(FakeTransport {
            frames: Default::default(),
            writes: writes.clone(),
            fail: false,
        });
        s.devices.insert(2, device());
        let mut pending = pending_switch();
        pending.recovery = Some(RecoveryPermission {
            activity: Arc::new(|| true),
            epoch: Arc::new(AtomicU64::new(1)),
            version: 1,
        });
        s.dispatch_switch(pending).unwrap();
        for _ in 0..2 {
            s.retries.get_mut(&2).unwrap().due = Instant::now();
            s.service_retry().unwrap();
        }
        assert!(s.retries.is_empty());
        assert!(s.retry_wait(None).is_none(), "Return to event-driven idle");
        let count = writes
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b[3] >> 4 == 1)
            .count();
        assert_eq!(count, 1, "Missing replies must never cause blind switching");
    }
    #[test]
    fn cursor_departure_is_notified_only_after_successful_current_hid_write() {
        for case in 0..7 {
            let dir = tempfile::tempdir().unwrap();
            let (mut s, _events) = session(dir.path(), vec![], case == 1);
            s.devices.insert(2, device());
            let mut pending = pending_switch();
            let calls = Arc::new(AtomicU64::new(0));
            let completed = calls.clone();
            if case != 6 {
                // Manual switches carry no cursor notification.
                pending.departure = Some(Box::new(move |_| {
                    completed.fetch_add(1, Ordering::Relaxed);
                }));
            }
            match case {
                2 => s.paused = true,
                3 => pending.epoch += 1,
                4 => pending.command.expires = Some(Instant::now()),
                5 => s.devices.get_mut(&2).unwrap().online = false,
                _ => {}
            }
            s.dispatch_switch(pending).unwrap();
            assert_eq!(
                calls.load(Ordering::Relaxed),
                u64::from(case == 0),
                "case {case}"
            );
        }
    }
    #[test]
    fn switch_interrupts_an_unrelated_transaction_without_losing_its_reply() {
        struct WokenTransport {
            writes: Arc<Mutex<Vec<Vec<u8>>>>,
            queue: Sender<PendingSwitch>,
            reads: usize,
        }
        impl Transport for WokenTransport {
            fn write(&mut self, bytes: &[u8]) -> Result<()> {
                self.writes.lock().unwrap().push(bytes.to_vec());
                Ok(())
            }
            fn read(&mut self, _: Option<Duration>) -> Result<Option<Vec<u8>>> {
                self.reads += 1;
                if self.reads == 1 {
                    // Equivalent to a coordinator command waking the HID wait
                    // while the receiver is waiting for a slow discovery reply.
                    self.queue.send(pending_switch()).unwrap();
                    return Ok(None);
                }
                let writes = self.writes.lock().unwrap();
                assert_eq!(writes.len(), 2, "Switch must precede the pending reply");
                assert_eq!(&writes[1][1..5], &[2, 10, 0x1a, 1]);
                let mut reply = writes[0].clone();
                reply[4] = 12;
                Ok(Some(reply))
            }
            fn waker(&self) -> Wake {
                Arc::new(|| {})
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _events) = session(dir.path(), vec![], false);
        let (tx, rx) = crossbeam_channel::bounded(64);
        let writes = Arc::default();
        s.transport = Box::new(WokenTransport {
            writes,
            queue: tx,
            reads: 0,
        });
        s.switches = rx;
        s.devices.insert(2, device());
        let started = Instant::now();
        assert_eq!(s.feature(2, 3).unwrap(), 12);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
    #[test]
    fn priority_queue_preserves_pause_epoch_disconnect_and_generation_guards() {
        for case in 0..5 {
            let dir = tempfile::tempdir().unwrap();
            let (mut s, _events) = session(dir.path(), vec![], false);
            let writes: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
            s.transport = Box::new(FakeTransport {
                frames: Default::default(),
                writes: writes.clone(),
                fail: false,
            });
            s.devices.insert(2, device());
            match case {
                0 => s.paused = true,
                1 => {
                    s.active_epoch.store(2, Ordering::Release);
                }
                2 => s.devices.get_mut(&2).unwrap().online = false,
                3 => s.devices.get_mut(&2).unwrap().generation += 1,
                4 => s.devices.get_mut(&2).unwrap().id = "replacement device".into(),
                _ => unreachable!(),
            }
            let (tx, rx) = crossbeam_channel::bounded(64);
            s.switches = rx;
            tx.send(pending_switch()).unwrap();
            s.dispatch_pending_switches().unwrap();
            assert!(writes.lock().unwrap().is_empty(), "Guard {case} failed");
        }
    }
    #[test]
    fn repeated_handoff_cancellation_keeps_manual_switches_and_notifications_working() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, events) = session(dir.path(), vec![], false);
        let writes = Arc::new(Mutex::new(Vec::new()));
        s.transport = Box::new(FakeTransport {
            frames: Default::default(),
            writes: writes.clone(),
            fail: false,
        });
        s.devices.insert(2, device());
        let (switches, urgent) = crossbeam_channel::bounded(8);
        s.switches = urgent;
        let worker = Worker {
            session: 1,
            tx: crossbeam_channel::bounded(1).0,
            switches,
            wake: Arc::new(|| {}),
            join: None,
            epoch: s.active_epoch.clone(),
            switch_epoch: s.active_switch_epoch.clone(),
        };
        let send = |edge| {
            let mut p = pending_switch();
            if edge {
                p.command.expires = Some(Instant::now() + Duration::from_secs(1));
            }
            worker
                .send(Command::Switch {
                    command: p.command,
                    epoch: worker.epoch.load(Ordering::Acquire),
                    received_at: p.received_at,
                    departure: None,
                    recovery: None,
                })
                .unwrap();
        };
        for round in 0..3 {
            send(true);
            worker.cancel_switches();
            s.dispatch_pending_switches().unwrap();
            assert_eq!(
                writes.lock().unwrap().len(),
                round * 2,
                "Old queued handoff was not cancelled"
            );
            send(false);
            send(true);
            s.dispatch_pending_switches().unwrap();
            assert_eq!(
                writes.lock().unwrap().len(),
                (round + 1) * 2,
                "Fresh manual and edge commands must survive cancellation"
            );
            s.receive(&protocol::request(2, 9, 0, &[]).unwrap());
            assert!(events.try_iter().any(|e| matches!(e, Event::Report { epoch, .. } if epoch == worker.epoch.load(Ordering::Acquire))));
            assert_eq!(s.epoch, worker.epoch.load(Ordering::Acquire));
        }
    }
    #[test]
    fn worker_routes_switches_around_a_full_management_queue() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let (switches, urgent) = crossbeam_channel::bounded(1);
        let wakes = Arc::new(AtomicU64::new(0));
        let signal = wakes.clone();
        let worker = Worker {
            session: 1,
            tx,
            switches,
            wake: Arc::new(move || {
                signal.fetch_add(1, Ordering::Relaxed);
            }),
            join: None,
            epoch: Arc::new(AtomicU64::new(1)),
            switch_epoch: Arc::new(AtomicU64::new(1)),
        };
        worker.send(Command::Refresh).unwrap();
        let PendingSwitch {
            command,
            epoch,
            received_at,
            departure,
            recovery,
            ..
        } = pending_switch();
        worker
            .send(Command::Switch {
                command,
                epoch,
                received_at,
                departure,
                recovery,
            })
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(Command::Refresh)));
        assert_eq!(urgent.try_recv().unwrap().command.target, 1);
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn harmless_reporting_writes_never_pause_repeated_switch_sessions() {
        let dir = tempfile::tempdir().unwrap();
        // The other application's reply echoes a diversion-only update. Its
        // zero analytics byte does NOT mean analytics reporting was disabled.
        let replies = (8..16)
            .map(|swid| reporting_packet(2, swid, &[0, 0xd1, 0, 0, 0, 1]))
            .collect();
        let (mut s, events) = session(dir.path(), replies, false);
        s.devices.insert(2, device());
        s.armed.insert((2, 0xd1), true);
        for swid in 1..9 {
            s.receive(&reporting_packet(3, swid, &[0, 0xd1, 2, 0, 0, 0]));
            assert!(!s.paused, "A write echo alone must never pause");
            let (slot, cid) = s.reporting_checks.pop_first().unwrap();
            s.check_reporting(slot, cid).unwrap();
            assert!(!s.paused);
        }
        assert!(!events.try_iter().any(|e| matches!(e, Event::Conflict(..))));
    }
    #[test]
    fn confirmed_reporting_conflict_pauses_once_without_overriding_writer() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, events) = session(
            dir.path(),
            vec![reporting_packet(2, 8, &[0, 0xd1, 0, 0, 0, 0])],
            false,
        );
        s.devices.insert(2, device());
        s.armed.insert((2, 0xd1), true);
        // Software IDs in our range are also usable by other applications.
        let write = reporting_packet(3, 15, &[0, 0xd1, 0, 0, 0, 2]);
        s.receive(&write);
        s.receive(&write); // coalesced, rather than one query per echoed write
        assert_eq!(s.reporting_checks.len(), 1);
        let (slot, cid) = s.reporting_checks.pop_first().unwrap();
        s.check_reporting(slot, cid).unwrap();
        assert!(s.paused);
        s.receive(&write);
        assert!(s.reporting_checks.is_empty());
        assert_eq!(
            events
                .try_iter()
                .filter(|e| matches!(e, Event::Conflict(..)))
                .count(),
            1
        );
    }
    #[test]
    fn reconnect_clears_pending_reporting_checks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, events) = session(dir.path(), vec![], false);
        s.devices.insert(2, device());
        s.armed.insert((2, 0xd1), true);
        let write = reporting_packet(3, 1, &[0, 0xd1, 2, 0, 0]);
        s.receive(&write);
        assert_eq!(s.reporting_checks.len(), 1);
        s.receive(&[0x10, 2, 0x41, 0, 0x40, 0, 0]);
        s.receive(&write);
        assert!(s.reporting_checks.is_empty());
        s.receive(&[0x10, 2, 0x41, 0, 0, 0, 0]);
        s.receive(&write);
        assert!(s.reporting_checks.is_empty());
        assert!(s.reprobe.contains(&2));
        assert!(!s.paused);
        assert!(!events.try_iter().any(|e| matches!(e, Event::Conflict(..))));
    }
    #[test]
    fn disconnect_during_reporting_verification_is_not_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, events) = session(
            dir.path(),
            vec![
                vec![0x10, 2, 0x41, 0, 0x40, 0, 0],
                reporting_packet(2, 8, &[0, 0xd1, 0, 0, 0, 0]),
            ],
            false,
        );
        s.devices.insert(2, device());
        s.armed.insert((2, 0xd1), true);
        s.check_reporting(2, 0xd1).unwrap();
        assert!(!s.paused);
        assert!(!events.try_iter().any(|e| matches!(e, Event::Conflict(..))));
    }
    #[test]
    fn failed_reporting_verification_is_not_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, events) = session(dir.path(), vec![], true);
        s.devices.insert(2, device());
        s.armed.insert((2, 0xd1), true);
        s.check_reporting(2, 0xd1).unwrap();
        assert!(!s.paused);
        assert!(!events.try_iter().any(|e| matches!(e, Event::Conflict(..))));
    }
    #[test]
    fn notification_survives_request_and_stale_response_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut notify = protocol::request(2, 10, 0, &[0, 1]).unwrap();
        notify[3] = 0;
        let stale = protocol::request(2, 0, 0, &[99]).unwrap(); // software ID A, not this request's 8
        let mut reply = protocol::request(2, 0, 0, &[10]).unwrap();
        reply[3] = 8;
        let (mut s, events) = session(
            dir.path(),
            vec![notify.to_vec(), stale.to_vec(), reply.to_vec()],
            false,
        );
        s.devices.insert(2, device());
        assert_eq!(s.feature(2, protocol::CHANGE_HOST).unwrap(), 10);
        assert!(events.try_iter().any(|e| matches!(e, Event::Report { report, .. } if report.feature == 10 && report.notification())));
    }
    #[test]
    fn failed_restore_keeps_record_and_success_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = session(dir.path(), vec![], true);
        s.devices.insert(2, device());
        s.journal
            .remember(Restoration {
                id: device().id,
                cid: 0xd1,
                analytics: false,
                previous: false,
            })
            .unwrap();
        s.restore().unwrap();
        assert_eq!(s.journal.entries().unwrap().len(), 1);
        // Reopening the journal simulates restarting with a different receiver path.
        let mut ack = protocol::request(2, 9, 3, &[0, 0xd1, 2, 0, 0]).unwrap();
        ack[3] = 0x38;
        let (mut restarted, _) = session(dir.path(), vec![ack.to_vec()], false);
        restarted.devices.insert(2, device());
        restarted.restore().unwrap();
        assert!(restarted.journal.entries().unwrap().is_empty());
        let disk: Vec<Restoration> =
            crate::storage::read_json(&dir.path().join("recovery.json")).unwrap();
        assert!(disk.is_empty());
    }
    #[test]
    fn absent_device_recovery_is_retained() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = session(dir.path(), vec![], false);
        s.journal
            .remember(Restoration {
                id: device().id,
                cid: 0xd2,
                analytics: true,
                previous: false,
            })
            .unwrap();
        s.restore().unwrap();
        assert_eq!(s.journal.entries().unwrap().len(), 1);
    }
    #[test]
    fn request_timeout_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = session(dir.path(), vec![], false);
        let start = Instant::now();
        assert!(s.feature(2, protocol::CHANGE_HOST).is_err());
        assert!(start.elapsed() < REQUEST_TIMEOUT + Duration::from_millis(250));
    }
}

#[derive(Clone)]
pub struct RecoveryPermission {
    pub activity: Arc<dyn Fn() -> bool + Send + Sync>,
    pub epoch: Arc<AtomicU64>,
    pub version: u64,
}
impl RecoveryPermission {
    fn valid(&self) -> bool {
        self.epoch.load(Ordering::SeqCst) == self.version && (self.activity)()
    }
}
struct FollowerRetry {
    command: SwitchCommand,
    epoch: u64,
    switch_epoch: u64,
    permission: RecoveryPermission,
    checks: u8,
    input_serial: u64,
    due: Instant,
    until: Instant,
}
impl Session {
    fn retry_wait(&self, current: Option<Duration>) -> Option<Duration> {
        let next = self
            .retries
            .values()
            .map(|r| r.due)
            .min()
            .map(|t| t.saturating_duration_since(Instant::now()));
        match (current, next) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    fn retry_valid(&self, r: &FollowerRetry) -> bool {
        !self.paused
            && r.epoch == self.epoch
            && r.epoch == self.active_epoch.load(Ordering::Acquire)
            && r.switch_epoch == self.active_switch_epoch.load(Ordering::Acquire)
            && r.permission.valid()
            && self.device_input.get(&r.command.slot).copied().unwrap_or(0) == r.input_serial
            && Instant::now() < r.until
    }
    fn service_retry(&mut self) -> Result<()> {
        let Some(slot) = self
            .retries
            .iter()
            .find(|(_, r)| r.due <= Instant::now())
            .map(|(slot, _)| *slot)
        else {
            return Ok(());
        };
        let mut retry = self.retries.remove(&slot).unwrap();
        if !self.retry_valid(&retry) {
            return Ok(());
        }
        let verified = self.verify_follower(&retry);
        if !self.retry_valid(&retry) {
            return Ok(());
        }
        retry.checks += 1;
        match verified {
            Ok(None) => return Ok(()),
            Ok(Some(feature)) => {
                // Fresh identity/host replies prove it is still on this source.
                // Recheck activity and handoff generation immediately before writing.
                if !self.retry_valid(&retry) {
                    return Ok(());
                }
                let result = self.transport.write(&protocol::request(
                    slot,
                    feature,
                    1,
                    &[retry.command.target],
                )?);
                self.message(format!(
                    "Follower recovery {} for {} (transaction {}): {}",
                    retry.checks,
                    retry.command.id,
                    retry.command.transaction.as_deref().unwrap_or("unknown"),
                    match result {
                        Ok(()) => "switch resent after confirming source host and no input".into(),
                        Err(error) => format!("{error:#}"),
                    }
                ));
            }
            Err(e) => {
                tracing::debug!(slot, error=%e, "Follower location unknown; no recovery write")
            }
        }
        if retry.checks < 2 {
            retry.due = Instant::now() + Duration::from_millis(750);
            // An urgent newer switch may have installed a replacement during the query.
            self.retries.entry(slot).or_insert(retry);
        }
        Ok(())
    }
    fn verify_follower(&mut self, r: &FollowerRetry) -> Result<Option<u8>> {
        let slot = r.command.slot;
        let deadline = Instant::now() + Duration::from_millis(200);
        let mut call = |feature, function, params: &[u8]| -> Result<Vec<u8>> {
            self.transact_until(
                protocol::request(slot, feature, function, params)?,
                deadline,
            )
        };
        let ids_index = call(0, 0, &3u16.to_be_bytes())?;
        let ids_index = *ids_index
            .first()
            .filter(|i| **i != 0)
            .context("Identity feature unavailable")?;
        let ids = call(ids_index, 0, &[])?;
        ensure!(ids.len() >= 13, "Truncated device identity");
        let hex = |b: &[u8]| b.iter().map(|b| format!("{b:02x}")).collect::<String>();
        ensure!(
            format!("unit:{}:{}", hex(&ids[1..5]), hex(&ids[7..13])) == r.command.id,
            "Receiver slot identity changed"
        );
        let feature = call(0, 0, &protocol::CHANGE_HOST.to_be_bytes())?;
        let feature = *feature
            .first()
            .filter(|i| **i != 0)
            .context("ChangeHost unavailable")?;
        let host = call(feature, 0, &[])?;
        ensure!(
            host.len() >= 2
                && (1..=3).contains(&host[0])
                && host[1] < host[0]
                && r.command.target < host[0],
            "Invalid host response"
        );
        if host[1] == r.command.target {
            return Ok(None);
        }
        let source = self
            .config
            .member(&r.command.id)
            .and_then(|m| m.channels.get(&self.config.follow.local_computer))
            .context("Source host unmapped")?;
        ensure!(
            host[1] + 1 == *source,
            "Device not confirmed on source host"
        );
        Ok(Some(feature))
    }
}
