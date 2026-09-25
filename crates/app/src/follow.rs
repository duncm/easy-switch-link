//! Authenticated, bounded LAN control plane; no keyboard, mouse-button, clipboard
//! or screen data is transmitted. Local IPC remains a separate endpoint.
use crate::{
    peer_tls::{Gate, Identity, fingerprint, pairing_code},
    storage::Paths,
};
use anyhow::{Context, Result, bail, ensure};
use easy_switch_core::{Device, config::Config, follow::*};
use easy_switch_platform::desktop;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};
const MAX_FRAME: usize = 64 * 1024;
#[derive(Clone)]
pub struct Local {
    pub config: Config,
    pub devices: Vec<Device>,
    pub paused: bool,
    pub generation: u64,
}
pub struct Dispatch {
    pub transaction: String,
    pub deadline: Instant,
    pub rule: Rule,
    pub generation: u64,
    pub at: Instant,
    pub reply: oneshot::Sender<Result<(), String>>,
}
pub enum Command {
    Update(Local),
    Desktop(desktop::Event),
    Action(Action),
    Stop,
}
#[derive(Clone)]
pub struct Manager {
    tx: mpsc::Sender<Command>,
    status: Arc<Mutex<FollowStatus>>,
}
impl Manager {
    pub fn start(paths: Paths, dispatch: crossbeam_channel::Sender<Dispatch>) -> Result<Self> {
        let (tx, rx) = mpsc::channel(64);
        let status = Arc::new(Mutex::new(FollowStatus::default()));
        let thread_status = status.clone();
        let thread_tx = tx.clone();
        std::thread::Builder::new()
            .name("edge-coordinator".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    rt.block_on(run(paths, rx, thread_tx, thread_status.clone(), dispatch))
                })();
                if let Err(e) = result {
                    thread_status.lock().unwrap().message = format!("Edge coordinator: {e:#}");
                }
            })?;
        Ok(Self { tx, status })
    }
    pub fn send(&self, c: Command) -> Result<()> {
        self.tx
            .try_send(c)
            .map_err(|_| anyhow::anyhow!("Edge coordinator queue full or stopped"))
    }
    pub fn status(&self) -> FollowStatus {
        self.status.lock().unwrap().clone()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Trust {
    id: String,
    name: String,
    address: String,
    computer: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot {
    port: u16,
    computer: String,
    name: String,
    desktop: DesktopState,
    config_generation: u64,
    ready: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Wire {
    OfferRule {
        rule: Rule,
    },
    Hello {
        version: u32,
        nonce: String,
        state: Snapshot,
    },
    Confirm,
    State {
        state: Snapshot,
    },
    Prepare {
        id: String,
        destination: Segment,
        fraction: f64,
        layout: u64,
        config_generation: u64,
        computer: String,
        group: String,
        mouse: String,
    },
    Prepared {
        id: String,
        result: Result<u64, String>,
    },
    Commit {
        id: String,
    },
    Cancel {
        id: String,
    },
    Bye,
}
struct Peer {
    tx: mpsc::Sender<Wire>,
    status: PeerStatus,
    session: u64,
    code: String,
    ours: bool,
    theirs: bool,
    expires: Instant,
}
struct Shared {
    paths: Paths,
    id: String,
    gate: Arc<Mutex<Gate>>,
    local: Local,
    desktop: Option<desktop::Handle>,
    desktop_state: DesktopState,
    peers: BTreeMap<String, Peer>,
    trust: BTreeMap<String, Trust>,
    pending: BTreeMap<String, oneshot::Sender<Result<u64, String>>>,
    status: Arc<Mutex<FollowStatus>>,
    dispatch: crossbeam_channel::Sender<Dispatch>,
    outbound: bool,
    connecting: BTreeSet<String>,
}
type State = Arc<Mutex<Shared>>;
impl Shared {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            port: self.local.config.follow.port,
            computer: self.local.config.follow.local_computer.clone(),
            name: self
                .local
                .config
                .computers
                .get(&self.local.config.follow.local_computer)
                .cloned()
                .unwrap_or_else(|| "Unconfigured computer".into()),
            desktop: DesktopState {
                reason: if self.local.paused {
                    "Monitoring paused".into()
                } else if self.desktop_state.locked {
                    "User session locked or inactive".into()
                } else {
                    self.desktop_state.reason.clone()
                },
                pointer: Point::default(),
                input_serial: 0,
                ..self.desktop_state.clone()
            },
            config_generation: self.local.generation,
            ready: self.local.config.follow.enabled
                && !self.local.paused
                && self.desktop_state.ready
                && !self.desktop_state.displays.is_empty()
                && !self.desktop_state.locked,
        }
    }
    fn publish(&self) {
        let mut s = self.status.lock().unwrap();
        s.identity = self.id.clone();
        s.desktop = self.desktop_state.clone();
        s.pairing_open = self
            .gate
            .lock()
            .unwrap()
            .pairing_until
            .is_some_and(|t| t > Instant::now());
        s.pairing = self
            .peers
            .iter()
            .filter(|(id, _)| !self.trust.contains_key(*id))
            .map(|(id, p)| Pairing {
                id: id.clone(),
                name: p.status.name.clone(),
                code: p.code.clone(),
                confirmed: p.ours,
            })
            .collect();
        let discovered: Vec<_> = s
            .peers
            .iter()
            .filter(|p| !p.trusted && !p.connected && !self.peers.contains_key(&p.id))
            .cloned()
            .collect();
        s.peers = self.peers.values().map(|p| p.status.clone()).collect();
        for t in self.trust.values() {
            if !self.peers.contains_key(&t.id) {
                s.peers.push(PeerStatus {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    address: t.address.clone(),
                    computer: t.computer.clone(),
                    trusted: true,
                    reason: "Disconnected".into(),
                    ..Default::default()
                });
            }
        }
        s.peers.extend(discovered);
        s.rules = self
            .local
            .config
            .follow
            .rules
            .iter()
            .map(|r| {
                let reason = if !r.enabled {
                    "Disabled"
                } else if !self.local.config.follow.enabled {
                    "Feature disabled"
                } else if self.local.paused {
                    "Monitoring paused"
                } else if !self.desktop_state.ready {
                    "Desktop permission or capability unavailable"
                } else if !self
                    .desktop_state
                    .displays
                    .iter()
                    .any(|d| d.id == r.source.display && d.binding == r.source.binding)
                {
                    "Source display binding changed"
                } else if !self
                    .peers
                    .get(&r.peer)
                    .is_some_and(|p| p.status.trusted && p.status.ready)
                {
                    "Destination unavailable"
                } else {
                    "Ready"
                };
                (r.id.clone(), reason.into())
            })
            .collect();
    }
    fn message(&self, text: impl Into<String>) {
        let text = text.into();
        tracing::info!(message=%text,"Destination follows mouse");
        self.status.lock().unwrap().message = text;
    }
    fn broadcast(&self) {
        let msg = Wire::State {
            state: self.snapshot(),
        };
        for p in self.peers.values().filter(|p| p.status.trusted) {
            let _ = p.tx.try_send(msg.clone());
        }
    }
    fn save_trust(&mut self) -> Result<()> {
        crate::storage::write_json(&self.paths.root.join("peer-trust.json"), &self.trust)?;
        self.gate.lock().unwrap().pins = self.trust.keys().cloned().collect();
        Ok(())
    }
}
async fn read_wire<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Result<Wire> {
    let first = r.read_u8().await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut rest = [0; 3];
        r.read_exact(&mut rest).await?;
        let n = u32::from_be_bytes([first, rest[0], rest[1], rest[2]]) as usize;
        ensure!(n > 0 && n <= MAX_FRAME, "Peer message too large");
        let mut bytes = vec![0; n];
        r.read_exact(&mut bytes).await?;
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await?
}
async fn write_wire<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, msg: &Wire) -> Result<()> {
    let b = serde_json::to_vec(msg)?;
    ensure!(b.len() <= MAX_FRAME, "Peer message too large");
    tokio::time::timeout(Duration::from_secs(2), async {
        w.write_u32(b.len() as u32).await?;
        w.write_all(&b).await?;
        w.flush().await
    })
    .await??;
    Ok(())
}
fn random_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}
async fn run(
    paths: Paths,
    mut commands: mpsc::Receiver<Command>,
    command_tx: mpsc::Sender<Command>,
    status: Arc<Mutex<FollowStatus>>,
    dispatch: crossbeam_channel::Sender<Dispatch>,
) -> Result<()> {
    let identity = Identity::load(&paths.root)?;
    let trust: BTreeMap<String, Trust> =
        crate::storage::read_json(&paths.root.join("peer-trust.json"))?;
    let gate = Arc::new(Mutex::new(Gate {
        pins: trust.keys().cloned().collect(),
        ..Default::default()
    }));
    let (client, server) = identity.configs(gate.clone())?;
    let shared = Arc::new(Mutex::new(Shared {
        paths,
        id: fingerprint(&identity.certificate),
        gate,
        local: Local {
            config: Config::default(),
            devices: vec![],
            paused: false,
            generation: 0,
        },
        desktop: None,
        desktop_state: Default::default(),
        peers: Default::default(),
        trust,
        pending: Default::default(),
        status,
        dispatch,
        outbound: false,
        connecting: BTreeSet::new(),
    }));
    let mut listener: Option<tokio::task::JoinHandle<()>> = None;
    let mut discovery: Option<mdns_sd::ServiceDaemon> = None;
    let mut port = 0;
    while let Some(command) = commands.recv().await {
        match command {
            Command::Stop => break,
            Command::Update(local) => {
                let enabled = local.config.follow.enabled;
                let new_port = local.config.follow.port;
                let wants_discovery = local.config.follow.discovery;
                {
                    let mut s = shared.lock().unwrap();
                    let desktop_changed = s.local.generation != local.generation
                        || s.local.paused != local.paused
                        || s.local.config.follow != local.config.follow;
                    if desktop_changed {
                        s.pending.clear();
                    }
                    s.local = local;
                    s.gate.lock().unwrap().enabled = enabled;
                    if desktop_changed && let Some(d) = &s.desktop {
                        let _ = d.send(desktop::Command::Configure {
                            enabled: enabled && !s.local.paused,
                            rules: s.local.config.follow.rules.clone(),
                        });
                    }
                    s.publish();
                    s.broadcast();
                }
                if !enabled || port != new_port {
                    if let Some(task) = listener.take() {
                        task.abort();
                    }
                    if let Some(d) = discovery.take() {
                        let _ = d.shutdown();
                    }
                    let s = shared.lock().unwrap();
                    for p in s.peers.values() {
                        let _ = p.tx.try_send(Wire::Bye);
                    }
                    port = new_port;
                }
                if !wants_discovery && let Some(d) = discovery.take() {
                    let _ = d.shutdown();
                }
                if wants_discovery && enabled && listener.is_some() && discovery.is_none() {
                    match start_discovery(shared.clone(), client.clone(), port) {
                        Ok(d) => discovery = Some(d),
                        Err(e) => shared
                            .lock()
                            .unwrap()
                            .message(format!("Discovery unavailable: {e:#}")),
                    }
                }
                if enabled && listener.is_none() {
                    ensure_desktop(&shared, &command_tx);
                    match TcpListener::bind(("0.0.0.0", port)).await {
                        Ok(socket) => {
                            let state = shared.clone();
                            let server = server.clone();
                            listener = Some(tokio::spawn(async move {
                                let limits = Arc::new(tokio::sync::Semaphore::new(8));
                                while let Ok((stream, address)) = socket.accept().await {
                                    let Ok(permit) = limits.clone().try_acquire_owned() else {
                                        continue;
                                    };
                                    let _ = stream.set_nodelay(true);
                                    let state = state.clone();
                                    let server = server.clone();
                                    tokio::spawn(async move {
                                        let _permit = permit;
                                        let result = async {
                                            let tls = tokio::time::timeout(
                                                Duration::from_secs(3),
                                                tokio_rustls::TlsAcceptor::from(server)
                                                    .accept(stream),
                                            )
                                            .await??;
                                            let cert = tls
                                                .get_ref()
                                                .1
                                                .peer_certificates()
                                                .and_then(|v| v.first())
                                                .context("Missing peer certificate")?;
                                            let id = fingerprint(cert);
                                            connection(
                                                state.clone(),
                                                tokio_rustls::TlsStream::Server(tls),
                                                id,
                                                address.to_string(),
                                            )
                                            .await
                                        }
                                        .await;
                                        if let Err(e) = result {
                                            tracing::debug!("Peer connection: {e:#}");
                                        }
                                    });
                                }
                            }));
                        }
                        Err(e) => shared
                            .lock()
                            .unwrap()
                            .message(format!("Cannot listen on port {port}: {e}")),
                    }
                    if shared.lock().unwrap().local.config.follow.discovery {
                        match start_discovery(shared.clone(), client.clone(), port) {
                            Ok(d) => discovery = Some(d),
                            Err(e) => shared.lock().unwrap().message(format!(
                                "Discovery unavailable: {e:#}; use a manual address"
                            )),
                        }
                    }
                    let peers: Vec<_> = shared.lock().unwrap().trust.values().cloned().collect();
                    for peer in peers {
                        spawn_connect(shared.clone(), client.clone(), peer.address, Some(peer.id));
                    }
                }
            }
            Command::Desktop(event) => match event {
                desktop::Event::State(snapshot) => {
                    let mut s = shared.lock().unwrap();
                    if snapshot.locked
                        || !snapshot.ready
                        || snapshot.generation != s.desktop_state.generation
                    {
                        s.pending.clear();
                    }
                    s.desktop_state = snapshot;
                    s.publish();
                    s.broadcast();
                }
                desktop::Event::Cross {
                    rule,
                    fraction,
                    generation,
                    at,
                } => {
                    let state = shared.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handoff(state.clone(), rule, fraction, generation, at).await
                        {
                            state.lock().unwrap().message(format!("Stayed here: {e:#}"));
                        }
                    });
                }
            },
            Command::Action(action) => {
                let result = (|| -> Result<()> {
                    match action {
                        Action::Status => {}
                        Action::Authorize => {
                            ensure_desktop(&shared, &command_tx);
                            if let Some(d) = &shared.lock().unwrap().desktop {
                                d.send(desktop::Command::Authorize)?;
                            }
                        }
                        Action::OpenPairing => {
                            let s = shared.lock().unwrap();
                            ensure!(
                                s.local.config.follow.enabled,
                                "Enable the feature and select this computer first"
                            );
                            s.gate.lock().unwrap().pairing_until =
                                Some(Instant::now() + Duration::from_secs(120));
                            s.message("Pairing open for two minutes; open pairing on the other computer too");
                        }
                        Action::Connect { address } => {
                            ensure!(
                                address.len() <= 255 && address.contains(':'),
                                "Use hostname:port (default port 24873)"
                            );
                            {
                                let s = shared.lock().unwrap();
                                ensure!(s.local.config.follow.enabled, "Enable the feature first");
                                s.gate.lock().unwrap().pairing_until =
                                    Some(Instant::now() + Duration::from_secs(120));
                            }
                            spawn_connect(shared.clone(), client.clone(), address, None);
                        }
                        Action::Confirm { peer, code } => {
                            let mut s = shared.lock().unwrap();
                            let p = s
                                .peers
                                .get_mut(&peer)
                                .context("Pairing connection expired")?;
                            ensure!(
                                p.expires > Instant::now() && p.code == code,
                                "Pairing code mismatched or expired"
                            );
                            p.ours = true;
                            p.tx.try_send(Wire::Confirm)?;
                            finish_pair(&mut s, &peer)?;
                        }
                        Action::Revoke { peer } => {
                            let mut s = shared.lock().unwrap();
                            s.trust.remove(&peer);
                            s.save_trust()?;
                            if let Some(p) = s.peers.get_mut(&peer) {
                                p.expires = Instant::now();
                                p.ours = false;
                                p.theirs = false;
                                p.status.trusted = false;
                                let _ = p.tx.try_send(Wire::Bye);
                            }
                            s.message("Peer revoked");
                        }
                        Action::Reverse { id } => {
                            let s = shared.lock().unwrap();
                            let original = s
                                .local
                                .config
                                .follow
                                .rules
                                .iter()
                                .find(|r| r.id == id)
                                .context("Unknown edge rule")?;
                            let p = s
                                .peers
                                .get(&original.peer)
                                .filter(|p| p.status.trusted)
                                .context("Destination must be paired and connected")?;
                            let rule = Rule {
                                id: format!("reverse-{}", random_id()),
                                enabled: false,
                                source: original.destination.clone(),
                                destination: original.source.clone(),
                                peer: s.id.clone(),
                                computer: s.local.config.follow.local_computer.clone(),
                                group: original.group.clone(),
                                mouse: original.mouse.clone(),
                            };
                            p.tx.try_send(Wire::OfferRule { rule })?;
                            s.message("Reverse rule proposed. Review and save it in the destination's settings.");
                        }
                        Action::SetRule { .. } => {
                            bail!("Rule actions must be handled by the local monitor")
                        }
                    }
                    Ok(())
                })();
                let s = shared.lock().unwrap();
                if let Err(e) = result {
                    s.message(e.to_string());
                }
                s.publish();
            }
        }
    }
    if let Some(task) = listener {
        task.abort();
    }
    if let Some(d) = discovery {
        let _ = d.shutdown();
    }
    let s = shared.lock().unwrap();
    if let Some(d) = &s.desktop {
        let _ = d.send(desktop::Command::Stop);
    }
    for p in s.peers.values() {
        let _ = p.tx.try_send(Wire::Bye);
    }
    Ok(())
}
fn ensure_desktop(shared: &State, tx: &mpsc::Sender<Command>) {
    let mut s = shared.lock().unwrap();
    if s.desktop.as_ref().is_some_and(|d| !d.is_closed()) {
        return;
    }
    s.desktop = None;
    let (events, rx) = crossbeam_channel::bounded(128);
    match desktop::start(events, s.paths.root.clone()) {
        Ok(handle) => {
            let _ = handle.send(desktop::Command::Configure {
                enabled: s.local.config.follow.enabled && !s.local.paused,
                rules: s.local.config.follow.rules.clone(),
            });
            s.desktop = Some(handle);
            let tx = tx.clone();
            std::thread::spawn(move || {
                while let Ok(e) = rx.recv() {
                    if tx.blocking_send(Command::Desktop(e)).is_err() {
                        break;
                    }
                }
            });
        }
        Err(e) => {
            s.desktop_state.reason = format!("Desktop unavailable: {e:#}");
            s.message(s.desktop_state.reason.clone());
        }
    }
    s.publish();
}
fn finish_pair(s: &mut Shared, id: &str) -> Result<()> {
    let p = s.peers.get_mut(id).context("Peer gone")?;
    if p.ours && p.theirs && p.expires > Instant::now() {
        let mut updated = s.trust.clone();
        updated.insert(
            id.into(),
            Trust {
                id: id.into(),
                name: p.status.name.clone(),
                address: p.status.address.clone(),
                computer: p.status.computer.clone(),
            },
        );
        crate::storage::write_json(&s.paths.root.join("peer-trust.json"), &updated)?;
        s.trust = updated;
        s.gate.lock().unwrap().pins = s.trust.keys().cloned().collect();
        p.status.trusted = true;
        s.broadcast();
        s.message("Computers paired");
    }
    Ok(())
}
fn spawn_connect(
    shared: State,
    client: Arc<rustls::ClientConfig>,
    address: String,
    mut expected: Option<String>,
) {
    let key = expected.clone().unwrap_or_else(|| address.clone());
    {
        let mut s = shared.lock().unwrap();
        if s.connecting.len() >= 8 || !s.connecting.insert(key.clone()) {
            return;
        }
    }
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            let (enabled, connected) = {
                let s = shared.lock().unwrap();
                (
                    s.local.config.follow.enabled
                        && expected.as_ref().is_none_or(|id| s.trust.contains_key(id)),
                    expected.as_ref().is_some_and(|id| s.peers.contains_key(id)),
                )
            };
            if !enabled {
                break;
            }
            if connected {
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
            let result = async {
                let stream =
                    tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&address))
                        .await??;
                stream.set_nodelay(true)?;
                let tls = tokio::time::timeout(
                    Duration::from_secs(3),
                    tokio_rustls::TlsConnector::from(client.clone()).connect(
                        rustls::pki_types::ServerName::try_from("easy-switch-link.local")?,
                        stream,
                    ),
                )
                .await??;
                let cert = tls
                    .get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|v| v.first())
                    .context("Missing peer certificate")?;
                let id = fingerprint(cert);
                ensure!(
                    expected.as_ref().is_none_or(|v| v == &id),
                    "Peer identity changed; pairing required"
                );
                expected = Some(id.clone());
                connection(
                    shared.clone(),
                    tokio_rustls::TlsStream::Client(tls),
                    id,
                    address.clone(),
                )
                .await
            }
            .await;
            if backoff == 1
                && let Err(e) = result
            {
                shared
                    .lock()
                    .unwrap()
                    .message(format!("Peer {address}: {e:#}"));
            }
            if expected.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(
                backoff * 1000 + rand::random_range(0..250),
            ))
            .await;
            backoff = (backoff * 2).min(60);
        }
        shared.lock().unwrap().connecting.remove(&key);
    });
}
async fn connection(
    shared: State,
    mut stream: tokio_rustls::TlsStream<TcpStream>,
    id: String,
    address: String,
) -> Result<()> {
    let nonce = random_id();
    let hello = { shared.lock().unwrap().snapshot() };
    write_wire(
        &mut stream,
        &Wire::Hello {
            version: 1,
            nonce: nonce.clone(),
            state: hello,
        },
    )
    .await?;
    let Wire::Hello {
        version,
        nonce: remote_nonce,
        state: remote,
    } = tokio::time::timeout(Duration::from_secs(3), read_wire(&mut stream)).await??
    else {
        bail!("Expected protocol greeting")
    };
    ensure!(
        version == 1
            && remote_nonce.len() == 32
            && remote.name.len() <= 128
            && remote.desktop.displays.len() <= 32
            && remote.desktop.displays.iter().all(|d| d.valid()),
        "Unsupported peer greeting"
    );
    let (tx, mut output) = mpsc::channel(32);
    let address = address
        .parse::<std::net::SocketAddr>()
        .map(|a| std::net::SocketAddr::new(a.ip(), remote.port).to_string())
        .unwrap_or(address);
    let session = rand::random();
    let trusted;
    {
        let mut s = shared.lock().unwrap();
        ensure!(id != s.id, "Cannot pair this computer with itself");
        ensure!(!s.peers.contains_key(&id), "Peer is already connected");
        trusted = s.trust.contains_key(&id);
        ensure!(
            trusted
                || s.gate
                    .lock()
                    .unwrap()
                    .pairing_until
                    .is_some_and(|t| t > Instant::now()),
            "Pairing window closed"
        );
        let code = pairing_code(&s.id, &nonce, &id, &remote_nonce);
        let computer = remote.computer.clone();
        if let Some(t) = s.trust.get(&id) {
            ensure!(
                t.computer == computer,
                "Peer computer identity changed; pair again"
            );
        }
        s.peers.insert(
            id.clone(),
            Peer {
                tx: tx.clone(),
                status: PeerStatus {
                    id: id.clone(),
                    name: remote.name.clone(),
                    address,
                    computer,
                    trusted,
                    connected: true,
                    ready: remote.ready,
                    reason: remote.desktop.reason.clone(),
                    displays: remote.desktop.displays.clone(),
                    generation: remote.desktop.generation,
                    config_generation: remote.config_generation,
                },
                session,
                code,
                ours: trusted,
                theirs: trusted,
                expires: Instant::now() + Duration::from_secs(120),
            },
        );
        s.publish();
    }
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (input_tx, mut input) = mpsc::channel(32);
    let read_task = tokio::spawn(async move {
        loop {
            let result = read_wire(&mut reader).await;
            let failed = result.is_err();
            if input_tx.send(result).await.is_err() || failed {
                break;
            }
        }
    });
    let mut seen = BTreeSet::new();
    let mut reservation: Option<(String, u64, Instant)> = None;
    let result=async{loop{
        let trusted=shared.lock().unwrap().trust.contains_key(&id);
        let expire=if !trusted{shared.lock().unwrap().peers.get(&id).map(|p|p.expires)}else{reservation.as_ref().map(|(_,_,t)|*t)};
        let deadline=expire.unwrap_or_else(||Instant::now()+Duration::from_secs(86400*365));
        tokio::select!{
            message=output.recv()=>{let Some(message)=message else{break};if matches!(message,Wire::Bye){break;}write_wire(&mut writer,&message).await?;},
            _=tokio::time::sleep_until(deadline.into())=>{if !trusted{bail!("Pairing expired");}
if let Some((_,token,_))=reservation.take()&& let Some(d)=&shared.lock().unwrap().desktop{let _=d.send(desktop::Command::Cancel(token));}},
            message=input.recv()=>{let message=message.context("Peer closed")??; let trusted=shared.lock().unwrap().trust.contains_key(&id);
                if !trusted && !matches!(message,Wire::Confirm|Wire::Bye){bail!("Unpaired peer attempted an operation");}
                let commit=matches!(&message,Wire::Commit{..});
                match message {
                    Wire::Bye=>break,
                    Wire::Confirm=>{let mut s=shared.lock().unwrap();let p=s.peers.get_mut(&id).context("Pairing lost")?;ensure!(p.expires>Instant::now(),"Pairing expired");p.theirs=true;finish_pair(&mut s,&id)?;s.publish();},
                    Wire::State{state}=>{ensure!(state.desktop.displays.len()<=32 && state.desktop.displays.iter().all(|d|d.valid()),"Invalid peer displays");let mut s=shared.lock().unwrap();let p=s.peers.get_mut(&id).context("Peer gone")?;ensure!(p.status.computer==state.computer,"Peer logical identity changed");p.status.ready=state.ready;p.status.reason=state.desktop.reason;p.status.displays=state.desktop.displays;p.status.generation=state.desktop.generation;p.status.config_generation=state.config_generation;s.publish();},
                    Wire::Prepare{id:transaction,destination,fraction,layout,config_generation,computer,group,mouse}=>{
                        ensure!(transaction.len()==32 && seen.len()<256 && seen.insert(transaction.clone()),"Replayed handoff or session transaction limit reached");
                        let preparation=(||->Result<desktop::Handle>{let s=shared.lock().unwrap();ensure!(s.snapshot().ready && s.local.generation==config_generation && s.local.config.follow.local_computer==computer,"Destination not ready or configuration changed");ensure!(reservation.is_none(),"Destination busy");ensure!(s.local.config.groups.iter().any(|g|g.id==group && g.devices.iter().any(|m|m.id==mouse && m.channels.contains_key(&computer))),"Destination group/mouse mapping not enrolled");s.desktop.clone().context("No desktop capability")})();
                        let placed=match preparation {Ok(d)=>match tokio::time::timeout(Duration::from_millis(400),d.place(destination,fraction,layout)).await {Ok(result)=>result.map_err(|e|e.to_string()),Err(_)=>Err("Cursor placement timed out".into())},Err(e)=>Err(e.to_string())};
                        if let Ok(token)=placed{reservation=Some((transaction.clone(),token,Instant::now()+Duration::from_secs(5)));}
                        write_wire(&mut writer,&Wire::Prepared{id:transaction,result:placed}).await?;
                    },
                    Wire::Prepared{id:transaction,result}=>{if let Some(reply)=shared.lock().unwrap().pending.remove(&format!("{id}:{transaction}")){let _=reply.send(result);}},
                    Wire::Commit{id:transaction}|Wire::Cancel{id:transaction}=>{if reservation.as_ref().is_some_and(|(key,_,_)|key==&transaction){let(_,token,_)=reservation.take().unwrap();if let Some(d)=&shared.lock().unwrap().desktop{let _=d.send(if commit{desktop::Command::Complete(token)}else{desktop::Command::Cancel(token)});}}},
                    Wire::OfferRule{mut rule}=>{let s=shared.lock().unwrap();ensure!(rule.peer==id && rule.computer==s.peers.get(&id).context("Peer gone")?.status.computer,"Invalid reverse rule origin");rule.enabled=false;rule.source.validate()?;rule.destination.validate()?;let mut status=s.status.lock().unwrap();ensure!(status.proposed_rules.len()<32,"Too many rule proposals");status.proposed_rules.push(rule);},
                    Wire::Hello{..}=>bail!("Unexpected greeting"),
                }
            }
        }
    }Ok(())}.await;
    read_task.abort();
    let mut s = shared.lock().unwrap();
    if let Some((_, token, _)) = reservation
        && let Some(d) = &s.desktop
    {
        let _ = d.send(desktop::Command::Cancel(token));
    }
    if s.peers.get(&id).is_some_and(|p| p.session == session) {
        s.peers.remove(&id);
        s.pending
            .retain(|key, _| !key.starts_with(&format!("{id}:")));
    }
    s.publish();
    result
}
async fn handoff(
    shared: State,
    rule_id: String,
    fraction: f64,
    layout: u64,
    at: Instant,
) -> Result<()> {
    let (rule, peer, generation, target_layout, target_config, dispatch) = {
        let mut s = shared.lock().unwrap();
        ensure!(!s.outbound, "A handoff is already in progress");
        ensure!(
            s.snapshot().ready && s.desktop_state.generation == layout,
            "Source desktop not ready"
        );
        let rule = s
            .local
            .config
            .follow
            .rules
            .iter()
            .find(|r| r.id == rule_id && r.enabled)
            .context("Edge rule removed")?
            .clone();
        let mouse = s
            .local
            .devices
            .iter()
            .find(|d| d.id == rule.mouse && d.online)
            .context("Primary mouse is not connected here")?;
        ensure!(
            mouse.kind == "mouse",
            "Primary device is not identified as a mouse"
        );
        let p = s
            .peers
            .get(&rule.peer)
            .filter(|p| p.status.trusted && p.status.ready && p.status.computer == rule.computer)
            .context("Destination offline, locked or unpaired")?;
        let result = (
            rule,
            p.tx.clone(),
            s.local.generation,
            p.status.generation,
            p.status.config_generation,
            s.dispatch.clone(),
        );
        s.outbound = true;
        result
    };
    let transaction = random_id();
    shared.lock().unwrap().status.lock().unwrap().handoff = Some(HandoffResult {
        transaction: transaction.clone(),
        rule: rule.id.clone(),
        outcome: "preparing".into(),
        edge_to_ready_ms: None,
        ready_to_queue_ms: None,
    });
    let key = format!("{}:{transaction}", rule.peer);
    let (tx, rx) = oneshot::channel();
    shared.lock().unwrap().pending.insert(key.clone(), tx);
    let mut dispatch_requested = false;
    let result = async {
        peer.try_send(Wire::Prepare {
            id: transaction.clone(),
            destination: rule.destination.clone(),
            fraction,
            layout: target_layout,
            config_generation: target_config,
            computer: rule.computer.clone(),
            group: rule.group.clone(),
            mouse: rule.mouse.clone(),
        })?;
        let _token = tokio::time::timeout(Duration::from_millis(500), rx)
            .await
            .context("Destination readiness timed out")??
            .map_err(anyhow::Error::msg)?;
        {
            let s = shared.lock().unwrap();
            ensure!(
                s.snapshot().ready
                    && s.local.generation == generation
                    && s.desktop_state.generation == layout,
                "Source changed during preparation"
            );
            ensure!(
                s.trust.contains_key(&rule.peer)
                    && s.peers.get(&rule.peer).is_some_and(|p| p.status.ready
                        && p.status.generation == target_layout
                        && p.status.config_generation == target_config),
                "Destination changed during preparation"
            );
        }
        let ready = Instant::now();
        if let Some(result) = &mut shared.lock().unwrap().status.lock().unwrap().handoff {
            result.edge_to_ready_ms =
                Some(ready.saturating_duration_since(at).as_secs_f64() * 1000.0);
        }
        let (tx, rx) = oneshot::channel();
        dispatch.try_send(Dispatch {
            transaction: transaction.clone(),
            deadline: ready + Duration::from_millis(100),
            rule: rule.clone(),
            generation,
            at,
            reply: tx,
        })?;
        dispatch_requested = true;
        tokio::time::timeout(Duration::from_millis(100), rx)
            .await
            .context("HID dispatch deadline missed")??
            .map_err(anyhow::Error::msg)?;
        peer.try_send(Wire::Commit {
            id: transaction.clone(),
        })?;
        if let Some(result) = &mut shared.lock().unwrap().status.lock().unwrap().handoff {
            result.ready_to_queue_ms = Some(ready.elapsed().as_secs_f64() * 1000.0);
        }
        shared.lock().unwrap().message(format!(
            "Edge {}: group sent to {}; readiness-to-queue {:.1} ms; radio arrival unverified",
            rule.id,
            rule.computer,
            ready.elapsed().as_secs_f64() * 1000.0
        ));
        Ok(())
    }
    .await;
    if result.is_err() {
        // Once the hardware dispatch request has been submitted, an ACK timeout
        // does not establish that no device moved. Never warp the cursor back
        // underneath a potentially arriving mouse.
        let _ = peer.try_send(if dispatch_requested {
            Wire::Commit { id: transaction }
        } else {
            Wire::Cancel { id: transaction }
        });
    }
    let mut s = shared.lock().unwrap();
    s.pending.remove(&key);
    s.outbound = false;
    if let Some(status) = &mut s.status.lock().unwrap().handoff {
        status.outcome = match &result {
            Ok(()) => "Commands queued; see device-write results; radio arrival unverified".into(),
            Err(e) => format!(
                "{}: {e:#}",
                if dispatch_requested {
                    "Partial or unknown hardware outcome"
                } else {
                    "Cancelled before hardware dispatch"
                }
            ),
        };
    }
    result
}
fn start_discovery(
    shared: State,
    client: Arc<rustls::ClientConfig>,
    port: u16,
) -> Result<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let s = shared.lock().unwrap();
    let service = mdns_sd::ServiceInfo::new(
        "_eslink._tcp.local.",
        &s.id[..12],
        &format!("eslink-{}.local.", &s.id[..12]),
        "",
        port,
        [("id", s.id.as_str()), ("name", s.snapshot().name.as_str())].as_slice(),
    )?
    .enable_addr_auto();
    daemon.register(service)?;
    let events = daemon.browse("_eslink._tcp.local.")?;
    drop(s);
    tokio::spawn(async move {
        while let Ok(event) = events.recv_async().await {
            if let mdns_sd::ServiceEvent::ServiceResolved(service) = event {
                let Some(id) = service.get_property_val_str("id") else {
                    continue;
                };
                let Some(ip) = service.get_addresses_v4().iter().next().copied() else {
                    continue;
                };
                let address = format!("{ip}:{}", service.get_port());
                let s = shared.lock().unwrap();
                if id == s.id {
                    continue;
                }
                if s.trust.contains_key(id) && !s.peers.contains_key(id) {
                    drop(s);
                    spawn_connect(shared.clone(), client.clone(), address, Some(id.into()));
                } else {
                    let mut status = s.status.lock().unwrap();
                    if id.len() == 64
                        && status.peers.len() < 64
                        && !status.peers.iter().any(|p| p.id == id)
                    {
                        status.peers.push(PeerStatus {
                            id: id.into(),
                            name: service
                                .get_property_val_str("name")
                                .unwrap_or("Nearby computer")
                                .into(),
                            address,
                            ..Default::default()
                        });
                    }
                }
            }
        }
    });
    Ok(daemon)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_network_frame_is_rejected() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut writer, mut reader) = tokio::io::duplex(16);
            writer.write_u32(MAX_FRAME as u32 + 1).await.unwrap();
            assert!(read_wire(&mut reader).await.is_err());
        });
    }
}

#[cfg(test)]
#[path = "follow_tests.rs"]
mod simulations;
