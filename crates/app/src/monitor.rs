use crate::{
    ipc,
    receiver::{self, Command, Event, Worker},
    storage::Paths,
};
use anyhow::{Context, Result, ensure};
use easy_switch_core::{
    Request, Response, Status,
    config::{Config, SourceMode},
    engine::Engine,
    protocol,
};
use fs2::FileExt;
use std::{
    collections::{BTreeMap, VecDeque},
    fs::OpenOptions,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

pub fn run(paths: Paths, headless: bool, diagnostics: bool) -> Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(paths.root.join("monitor.lock"))?;
    lock.try_lock_exclusive()
        .context("Easy-Switch Link is already running")?;
    let log_dir = paths.root.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("easy-switch-link")
        .max_log_files(5)
        .build(log_dir)?;
    let (writer, _guard) = tracing_appender::non_blocking(appender);
    let filter = if diagnostics {
        tracing_subscriber::EnvFilter::new("easy_switch_link=trace,easy_switch_platform=debug")
    } else {
        std::env::var("RUST_LOG")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(tracing_subscriber::EnvFilter::new)
            .unwrap_or_else(|| {
                if diagnostics {
                    "easy_switch_link=trace,easy_switch_platform=debug"
                } else {
                    "easy_switch_link=info,easy_switch_platform=warn"
                }
                .into()
            })
    };
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(writer)
        .try_init();
    let config = paths.load()?;
    let journal = receiver::Journal::open(paths.root.join("recovery.json"))?;
    let first_run = !paths.config().exists();
    let (control_tx, control_rx) = crossbeam_channel::bounded(32);
    ipc::serve(paths.clone(), control_tx.clone())?;
    let (events_tx, events_rx) = crossbeam_channel::bounded(256);
    let (changes_tx, changes_rx) = crossbeam_channel::bounded(1);
    easy_switch_platform::watch_changes(changes_tx.clone())?;
    let stop_tx = control_tx.clone();
    ctrlc::set_handler(move || {
        let (reply, _) = tokio::sync::oneshot::channel();
        let _ = stop_tx.try_send((Request::Quit, reply));
    })?;
    if !headless {
        crate::tray::spawn(paths.clone())?;
        if first_run && std::env::var_os("EASY_SWITCH_SETTINGS_OPENING").is_none() {
            crate::cli::spawn_settings(&paths)?;
        }
    }
    let mut engine = Engine::new(config);
    let cached: Vec<easy_switch_core::Device> =
        crate::storage::read_json(&paths.root.join("device-cache.json"))?;
    for mut device in cached {
        device.online = false;
        device.generation = 0;
        engine.devices.insert(device.id.clone(), device);
    }
    let (follow_tx, follow_rx) = crossbeam_channel::bounded(16);
    let mut state = Monitor {
        follow_tx,
        engine,
        journal,
        workers: BTreeMap::new(),
        receivers: BTreeMap::new(),
        discovering: 0,
        messages: VecDeque::new(),
        paths,
        generation: 1,
        follow: None,
        follow_generation: 1,
        ambiguous: std::collections::BTreeSet::new(),
        events: events_tx,
        retry: None,
        retry_count: 0,
    };
    state.rescan()?;
    state.update_follow();
    loop {
        let retry_timer = state
            .retry
            .map(|t| crossbeam_channel::after(t.saturating_duration_since(Instant::now())))
            .unwrap_or_else(crossbeam_channel::never);
        crossbeam_channel::select! {
            recv(control_rx) -> envelope => {
                let (request, reply) = envelope?;
                let quit = matches!(request, Request::Quit);
                let response = match state.handle(request) {
                    Ok(message) => Response { ok: true, message, status: Some(state.status()) },
                    Err(error) => Response { ok: false, message: format!("{error:#}"), status: Some(state.status()) },
                };
                let _ = reply.send(response);
                if quit { break; }
            }
            recv(events_rx) -> event => state.event(event?),
            recv(follow_rx) -> dispatch => { if let Ok(dispatch) = dispatch { let result = state.edge_dispatch(&dispatch); let _ = dispatch.reply.send(result.map_err(|e|e.to_string())); } },
            recv(changes_rx) -> _ => {
                state.retry_count = 0;
                // Coalesce OS event bursts; suspend/resume also invalidates queued work.
                state.invalidate();
                state.retry = Some(Instant::now() + Duration::from_millis(250));
            }
            recv(retry_timer) -> _ => { state.retry = None; if let Err(e) = state.rescan() { state.message(format!("Receiver discovery: {e:#}")); } }
        }
    }
    // Do not block the coordinator while bounded worker event queues are draining.
    if let Some(follow) = &state.follow {
        follow.shutdown();
    }
    state.engine.paused = true;
    state.invalidate();
    for worker in state.workers.values() {
        let _ = worker.send(Command::Stop);
    }
    let until = Instant::now() + Duration::from_secs(8);
    while state
        .workers
        .values()
        .any(|w| w.join.as_ref().is_some_and(|j| !j.is_finished()))
        && Instant::now() < until
    {
        let _ = events_rx.recv_timeout(Duration::from_millis(20));
    }
    for (_, mut worker) in state.workers {
        if worker.join.as_ref().is_some_and(|j| j.is_finished()) {
            let _ = worker.join.take().unwrap().join();
        }
    }
    Ok(())
}

struct Monitor {
    follow_tx: crossbeam_channel::Sender<crate::follow::Dispatch>,
    follow: Option<crate::follow::Manager>,
    follow_generation: u64,
    ambiguous: std::collections::BTreeSet<String>,
    journal: std::sync::Arc<receiver::Journal>,
    engine: Engine,
    workers: BTreeMap<String, Worker>,
    receivers: BTreeMap<String, String>,
    discovering: usize,
    messages: VecDeque<String>,
    paths: Paths,
    generation: u64,
    events: crossbeam_channel::Sender<Event>,
    retry: Option<Instant>,
    retry_count: u32,
}
impl Monitor {
    fn ensure_follow(&mut self) -> Result<()> {
        if self.follow.is_none() {
            let manager =
                crate::follow::Manager::start(self.paths.clone(), self.follow_tx.clone())?;
            manager.send(crate::follow::Command::Update(crate::follow::Local {
                config: self.engine.config.clone(),
                devices: self.engine.devices.values().cloned().collect(),
                paused: self.engine.paused,
                generation: self.follow_generation,
            }))?;
            self.follow = Some(manager);
        }
        Ok(())
    }
    fn update_follow(&mut self) {
        if !self.engine.config.follow.enabled {
            if let Some(follow) = self.follow.take() {
                follow.shutdown();
            }
            return;
        }
        if let Err(e) = self.ensure_follow() {
            self.message(format!("Edge coordinator unavailable: {e:#}"));
            return;
        }
        if let Some(follow) = &self.follow {
            let _ = follow.send(crate::follow::Command::Update(crate::follow::Local {
                config: self.engine.config.clone(),
                devices: self.engine.devices.values().cloned().collect(),
                paused: self.engine.paused,
                generation: self.follow_generation,
            }));
        }
    }
    fn edge_dispatch(&mut self, request: &crate::follow::Dispatch) -> Result<()> {
        ensure!(
            Instant::now() < request.deadline,
            "Handoff authorization expired"
        );
        ensure!(
            !self.engine.paused && self.engine.config.follow.enabled,
            "Monitoring paused or edge switching disabled"
        );
        ensure!(
            self.follow_generation == request.generation,
            "Configuration changed during handoff"
        );
        ensure!(
            self.engine
                .config
                .follow
                .rules
                .iter()
                .any(|r| r == &request.rule && r.enabled),
            "Edge rule changed"
        );
        ensure!(
            self.follow.as_ref().is_some_and(|f| {
                let s = f.status();
                s.desktop.ready
                    && !s.desktop.locked
                    && s.peers
                        .iter()
                        .any(|p| p.id == request.rule.peer && p.trusted && p.ready)
            }),
            "Desktop or destination became unavailable"
        );
        let mouse = self
            .engine
            .devices
            .get(&request.rule.mouse)
            .filter(|d| d.online && d.kind == "mouse")
            .context("Primary mouse disconnected")?;
        let member = self
            .engine
            .config
            .member(&mouse.id)
            .context("Mouse mapping missing")?;
        ensure!(
            member.channels.contains_key(&request.rule.computer),
            "Mouse destination unmapped"
        );
        let (mut commands, errors) = self.engine.plan(
            &request.rule.group,
            &request.rule.computer,
            None,
            Instant::now(),
        );
        commands.sort_by_key(|c| c.id == request.rule.mouse);
        ensure!(
            commands.iter().any(|c| c.id == request.rule.mouse),
            "Mouse is already at the destination or unavailable"
        );
        for error in errors {
            self.message(error);
        }
        for mut command in commands {
            command.expires = Some(request.deadline);
            command.transaction = Some(request.transaction.clone());
            let primary = command.id == request.rule.mouse;
            let Some(worker) = self.workers.get(&command.receiver) else {
                if primary {
                    anyhow::bail!("Primary mouse receiver disappeared");
                }
                self.message("Follower receiver disappeared".into());
                continue;
            };
            let epoch = worker.epoch.load(Ordering::Acquire);
            if let Err(error) = worker.send(Command::Switch {
                command,
                epoch,
                received_at: request.at,
                departure: request.desktop.clone().filter(|_| primary).map(|desktop| {
                    let crossing = request.at;
                    Box::new(move |written| {
                        if let Err(error) =
                            desktop.send(easy_switch_platform::desktop::Command::Depart {
                                crossing,
                                written,
                            })
                        {
                            tracing::warn!(%error, "Could not hide departed cursor");
                        }
                    }) as Box<dyn FnOnce(Instant) + Send + Sync>
                }),
            }) {
                if primary {
                    return Err(error);
                }
                self.message(format!("Follower command unavailable: {error:#}"));
            }
        }
        Ok(())
    }
    fn current_session(&self, receiver: &str, session: u64) -> bool {
        self.workers
            .get(receiver)
            .is_some_and(|worker| worker.session == session)
    }
    fn message(&mut self, message: String) {
        tracing::info!(%message);
        if self.messages.len() == 32 {
            self.messages.pop_front();
        }
        self.messages.push_back(message);
    }
    fn status(&self) -> Status {
        Status {
            follow: self.follow.as_ref().map(|f| f.status()).unwrap_or_default(),
            paused: self.engine.paused,
            configured: self
                .engine
                .config
                .groups
                .iter()
                .any(|g| !g.devices.is_empty()),
            discovering: self.discovering,
            devices: self.engine.devices.values().cloned().collect(),
            receivers: self.receivers.clone(),
            messages: self.messages.iter().cloned().collect(),
            config: self.engine.config.clone(),
        }
    }
    fn invalidate(&mut self) {
        for worker in self.workers.values() {
            worker.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }
    fn configure(&mut self) -> Result<()> {
        self.follow_generation += 1;
        self.update_follow();
        for worker in self.workers.values() {
            let epoch = worker.epoch.fetch_add(1, Ordering::AcqRel) + 1;
            worker.send(Command::Configure {
                config: self.engine.config.clone(),
                paused: self.engine.paused,
                epoch,
            })?;
        }
        Ok(())
    }
    fn rescan(&mut self) -> Result<()> {
        self.ambiguous.clear();
        let found = easy_switch_platform::discover()?;
        let gone: Vec<_> = self
            .workers
            .keys()
            .filter(|id| !found.iter().any(|r| &r.id == *id))
            .cloned()
            .collect();
        for id in gone {
            if let Some(worker) = self.workers.remove(&id) {
                let _ = worker.send(Command::Stop);
            }
            self.engine.disconnect(&id);
            self.receivers.remove(&id);
        }
        for info in found {
            if let Some(worker) = self.workers.get(&info.id) {
                worker.send(Command::Refresh)?;
                continue;
            }
            self.generation += 1;
            let id = info.id.clone();
            match receiver::spawn(
                info,
                self.events.clone(),
                self.journal.clone(),
                self.engine.config.clone(),
                self.engine.paused,
                self.generation << 32,
            ) {
                Ok(worker) => {
                    self.receivers
                        .insert(id.clone(), "Discovering devices".into());
                    self.workers.insert(id, worker);
                }
                Err(e) => {
                    self.receivers.insert(id, format!("Access error: {e:#}"));
                    self.message(format!("Receiver access: {e:#}"));
                    self.schedule_retry();
                }
            }
        }
        self.discovering = self
            .receivers
            .values()
            .filter(|v| v.as_str() == "Discovering devices")
            .count();
        // Bring surviving workers onto the current generation after hotplug/resume.
        self.configure()?;
        Ok(())
    }
    fn schedule_retry(&mut self) {
        if self.retry_count < 4 {
            self.retry = Some(Instant::now() + Duration::from_secs(1 << self.retry_count));
            self.retry_count += 1;
        }
    }
    fn event(&mut self, event: Event) {
        match event {
            Event::Message(message) => self.message(message),
            Event::Conflict(id, session, name) => {
                if !self.current_session(&id, session) {
                    return;
                }
                self.engine.paused = true;
                self.message(format!("Paused: {name} reporting was changed by another application. Disable its Easy-Switch rules before resuming."));
                if let Err(e) = self.configure() {
                    self.message(format!("Could not restore all receiver settings: {e:#}"));
                }
            }
            Event::Ready(id, session) => {
                if !self.current_session(&id, session) {
                    return;
                }
                self.receivers.insert(id, "Listening".into());
                self.discovering = self
                    .receivers
                    .values()
                    .filter(|v| v.as_str() == "Discovering devices")
                    .count();
            }
            Event::Lost(id, session, error) => {
                if !self.current_session(&id, session) {
                    return;
                }
                self.workers.remove(&id);
                self.engine.disconnect(&id);
                self.receivers.insert(id, format!("Disconnected: {error}"));
                self.message(error);
                self.schedule_retry();
            }
            Event::Device(mut device) => {
                if !self.current_session(&device.receiver, device.generation >> 32) {
                    return;
                }
                if self
                    .engine
                    .devices
                    .get(&device.id)
                    .is_some_and(|existing| existing.online && existing.receiver != device.receiver)
                {
                    device.online = false;
                    self.ambiguous.insert(device.id.clone());
                    self.message(format!(
                        "Ambiguous device identity {}; forwarding disabled",
                        device.id
                    ));
                }
                if self.ambiguous.contains(&device.id) {
                    device.online = false;
                }
                self.engine.devices.insert(device.id.clone(), device);
                self.update_follow();
                let devices: Vec<_> = self.engine.devices.values().collect();
                if let Err(error) =
                    crate::storage::write_json(&self.paths.root.join("device-cache.json"), &devices)
                {
                    self.message(format!("Device cache could not be saved: {error:#}"));
                }
            }
            Event::Report {
                receiver,
                report,
                generation,
                epoch,
                received_at,
            } => {
                if !self
                    .workers
                    .get(&receiver)
                    .is_some_and(|w| w.epoch.load(Ordering::Acquire) == epoch)
                {
                    return;
                }
                let device = self
                    .engine
                    .devices
                    .values()
                    .find(|d| {
                        d.receiver == receiver
                            && d.slot == report.device
                            && d.online
                            && Some(d.generation) == generation
                    })
                    .cloned();
                if let Some(d) = device
                    && let Some(target) = protocol::switch_event(&report, &d, d.intercepted)
                {
                    let (commands, errors) = self.engine.from_event(&d.id, target, Instant::now());
                    for error in errors {
                        self.message(error);
                    }
                    self.dispatch(commands, received_at);
                }
            }
        }
    }
    fn dispatch(
        &mut self,
        commands: Vec<easy_switch_core::engine::SwitchCommand>,
        received_at: Instant,
    ) {
        for command in commands {
            if let Some(worker) = self.workers.get(&command.receiver) {
                let epoch = worker.epoch.load(Ordering::Acquire);
                if let Err(e) = worker.send(Command::Switch {
                    command,
                    epoch,
                    received_at,
                    departure: None,
                }) {
                    self.message(format!("Switch not queued: {e:#}"));
                }
            }
        }
    }
    fn validate_devices(&self, config: &Config) -> Result<()> {
        config.validate()?;
        for member in config.groups.iter().flat_map(|g| &g.devices) {
            let device = self.engine.devices.get(&member.id).context(format!("Device {} has not been identified locally. Connect it before importing/enrolling it.", member.id))?;
            ensure!(
                member.channels.values().all(|ch| *ch <= device.host_count),
                "{} has fewer channels than configured",
                device.name
            );
            if member.mode == SourceMode::Intercept {
                ensure!(
                    device.controls.iter().any(|c| c.divertable),
                    "{} does not expose divertable Easy-Switch keys",
                    device.name
                );
            }
        }
        Ok(())
    }
    fn handle(&mut self, request: Request) -> Result<String> {
        match request {
            Request::Status => Ok("Ready".into()),
            Request::Follow { action } => {
                self.ensure_follow()?;
                match action {
                    easy_switch_core::follow::Action::SetRule { id, enabled } => {
                        let mut config = self.engine.config.clone();
                        config
                            .follow
                            .rules
                            .iter_mut()
                            .find(|r| r.id == id)
                            .context("Unknown edge rule")?
                            .enabled = enabled;
                        self.validate_devices(&config)?;
                        self.paths.save(&config)?;
                        self.engine.replace_config(config);
                        self.configure()?;
                    }
                    action => self
                        .follow
                        .as_ref()
                        .context("Edge coordinator unavailable")?
                        .send(crate::follow::Command::Action(action))?,
                }
                Ok("Edge action queued".into())
            }
            Request::Pause => {
                self.engine.paused = true;
                self.configure()?;
                Ok("Paused; restoring reporting on reachable devices".into())
            }
            Request::Resume => {
                self.engine.paused = false;
                self.configure()?;
                Ok("Monitoring resumed".into())
            }
            Request::Recover => {
                self.engine.paused = true;
                self.invalidate();
                for worker in self.workers.values() {
                    worker.send(Command::Recover)?;
                }
                Ok("Recovery requested; unreachable devices remain in the recovery journal".into())
            }
            Request::Rescan => {
                self.rescan()?;
                Ok("Discovery requested".into())
            }
            Request::Save { config } => {
                let config = config.migrate()?;
                self.validate_devices(&config)?;
                let previous_startup = self.engine.config.start_at_login;
                let startup_changed = config.start_at_login != previous_startup;
                let executable = std::env::current_exe()?;
                if startup_changed {
                    easy_switch_platform::set_autostart(config.start_at_login, &executable)?;
                }
                if let Err(error) = self.paths.save(&config) {
                    if startup_changed {
                        let _ = easy_switch_platform::set_autostart(previous_startup, &executable);
                    }
                    return Err(error);
                }
                self.engine.replace_config(config);
                self.configure()?;
                Ok("Configuration saved".into())
            }
            Request::Switch { group, computer } => {
                ensure!(!self.engine.paused, "Monitoring is paused");
                let computer = self.engine.config.resolve_computer(&computer)?.to_owned();
                ensure!(
                    self.engine.config.groups.iter().any(|g| g.id == group),
                    "Unknown group"
                );
                let (commands, errors) = self.engine.plan(&group, &computer, None, Instant::now());
                let count = commands.len();
                for error in errors {
                    self.message(error);
                }
                self.dispatch(commands, Instant::now());
                Ok(format!(
                    "Queued {count} switch commands; destination arrival cannot be confirmed locally"
                ))
            }
            Request::Quit => Ok("Stopping and restoring reporting".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_receiver_events_cannot_remove_or_pause_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf())).unwrap();
        let (events, _) = crossbeam_channel::bounded(8);
        let (tx, _rx) = crossbeam_channel::bounded(8);
        let worker = Worker {
            session: 2,
            tx,
            switches: crossbeam_channel::bounded(1).0,
            wake: std::sync::Arc::new(|| {}),
            join: None,
            epoch: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
        };
        let mut monitor = Monitor {
            follow_tx: crossbeam_channel::bounded(1).0,
            journal: receiver::Journal::open(paths.root.join("recovery.json")).unwrap(),
            paths,
            engine: Engine::default(),
            workers: [("bolt".into(), worker)].into(),
            receivers: [("bolt".into(), "Discovering devices".into())].into(),
            discovering: 1,
            messages: VecDeque::new(),
            generation: 2,
            events,
            retry: None,
            retry_count: 0,
            follow: None,
            follow_generation: 1,
            ambiguous: Default::default(),
        };
        monitor.event(Event::Lost("bolt".into(), 1, "stale disconnect".into()));
        monitor.event(Event::Ready("bolt".into(), 1));
        monitor.event(Event::Conflict("bolt".into(), 1, "old device".into()));
        assert!(monitor.workers.contains_key("bolt"));
        assert!(!monitor.engine.paused);
        assert_eq!(monitor.receivers["bolt"], "Discovering devices");
        monitor.event(Event::Ready("bolt".into(), 2));
        assert_eq!(monitor.receivers["bolt"], "Listening");
        monitor.event(Event::Lost("bolt".into(), 2, "current disconnect".into()));
        assert!(!monitor.workers.contains_key("bolt"));
    }
}
