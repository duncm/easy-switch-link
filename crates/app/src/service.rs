//! Authenticated Linux service bridge. Network messages never reach this API.
use crate::{ipc, receiver::Event, storage::Paths};
use anyhow::{Context, Result, ensure};
use easy_switch_core::{Request, Response, Status, service::*};
use easy_switch_platform::system;
use std::{
    collections::BTreeMap,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{broadcast, oneshot},
};

pub struct Envelope {
    pub uid: u32,
    pub control: Control,
    pub reply: oneshot::Sender<Response>,
}
impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceEnvelope")
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}
pub struct Host {
    pub policy: Policy,
    pub activity: Option<system::Activity>,
    pub instance: String,
    notices: broadcast::Sender<Notice>,
    snapshot: Arc<Mutex<Status>>,
}
fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}
async fn read<T: serde::de::DeserializeOwned>(s: &mut UnixStream) -> Result<T> {
    let n = s.read_u32_le().await? as usize;
    ensure!(n > 0 && n <= LIMIT, "Service frame too large");
    let mut b = vec![0; n];
    s.read_exact(&mut b).await?;
    Ok(serde_json::from_slice(&b)?)
}
async fn write<T: serde::Serialize>(s: &mut UnixStream, v: &T) -> Result<()> {
    let b = serde_json::to_vec(v)?;
    ensure!(b.len() <= LIMIT, "Service response too large");
    s.write_u32_le(b.len() as u32).await?;
    s.write_all(&b).await?;
    Ok(())
}
pub fn call(path: &str, control: Control) -> Result<Response> {
    runtime()?.block_on(async{tokio::time::timeout(Duration::from_secs(3),async{
    let mut s=UnixStream::connect(path).await.context("Configured system service is unavailable; refusing to start another receiver owner")?;
    write(&mut s,&Packet{version:VERSION,control}).await?;
    match read::<Notice>(&mut s).await?{Notice::Reply{response}=>Ok(response),_=>anyhow::bail!("Unexpected service reply")}
}).await.context("System service request timed out")?})
}
pub fn hardware(request: Request) -> Result<Response> {
    call(HARDWARE_SOCKET, Control::User { request })
}
fn allowed(p: &Policy, uid: u32, control: &Control, peer: bool) -> Result<()> {
    match control {
        Control::Watch => ensure!(
            uid == p.owner_uid || uid == p.peer_uid,
            "Not a service client"
        ),
        Control::User { request } => {
            ensure!(
                uid == p.owner_uid,
                "Only the designated user controls this service"
            );
            if !matches!(request, Request::Status) {
                let s = system::active_session(p)?;
                ensure!(
                    s.uid == uid && !s.greeter && !s.locked,
                    "Control requires the designated user's unlocked session"
                );
            }
            ensure!(
                !matches!(request, Request::Quit),
                "Use service disable to stop system operation"
            );
            if !peer {
                ensure!(
                    !matches!(request, Request::Follow { .. }),
                    "Use the session or peer coordinator for this operation"
                );
            }
        }
        _ => {
            ensure!(!peer, "Not a receiver endpoint");
            ensure!(
                uid == if p.login_handoffs {
                    p.peer_uid
                } else {
                    p.owner_uid
                },
                "Not the configured coordinator"
            );
        }
    }
    Ok(())
}
impl Host {
    pub fn start(
        policy: Policy,
        peer: bool,
        tx: crossbeam_channel::Sender<Event>,
        epoch: Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Self> {
        let expected = if peer {
            policy.peer_uid
        } else {
            policy.receiver_uid
        };
        ensure!(
            unsafe { libc::geteuid() } == expected,
            "Wrong account for service role"
        );
        let path = if peer { PEER_SOCKET } else { HARDWARE_SOCKET };
        let listener = std::os::unix::net::UnixListener::bind(path).or_else(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse
                && std::os::unix::net::UnixStream::connect(path).is_err()
            {
                std::fs::remove_file(path)?;
                std::os::unix::net::UnixListener::bind(path)
            } else {
                Err(e)
            }
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        listener.set_nonblocking(true)?;
        let (notices, _) = broadcast::channel(64);
        let snapshot = Arc::new(Mutex::new(Status::default()));
        let out = notices.clone();
        let initial = snapshot.clone();
        let p = policy.clone();
        std::thread::Builder::new().name("system-control".into()).spawn(move||{let result=(||->Result<()>{runtime()?.block_on(async move{
            let listener=UnixListener::from_std(listener)?;let slots=Arc::new(tokio::sync::Semaphore::new(16));
            loop {let(mut socket,_)=listener.accept().await?;let Ok(permit)=slots.clone().try_acquire_owned()else{continue;};let p=p.clone();let tx=tx.clone();let initial=initial.clone();let mut rx=out.subscribe();let epoch=epoch.clone();
                tokio::spawn(async move{let _permit=permit;let result=async{
                    let(uid,_)=system::credentials(socket.as_raw_fd())?;
                    // Drop unauthorized clients before parsing their requests.
                    ensure!(uid==p.owner_uid||uid==p.peer_uid,"Unauthorized service identity");
                    let packet:Packet=tokio::time::timeout(Duration::from_secs(3),read(&mut socket)).await??;
                    ensure!(packet.version==VERSION,"Incompatible service protocol");
                    let auth_policy=p.clone();let auth_control=packet.control.clone();
                    tokio::task::spawn_blocking(move||allowed(&auth_policy,uid,&auth_control,peer)).await??;
                    if matches!(packet.control,Control::Watch){
                        let first=initial.lock().unwrap().clone();write(&mut socket,&Notice::Snapshot{status:first}).await?;
                        let outcome=async{loop{tokio::select!{event=rx.recv()=>{write(&mut socket,&event?).await?;},unexpected=socket.read_u8()=>{let _=unexpected;return Ok::<(),anyhow::Error>(());}}}}.await;
                        if uid==if p.login_handoffs{p.peer_uid}else{p.owner_uid}{epoch.fetch_add(1,std::sync::atomic::Ordering::SeqCst);let _=tx.try_send(Event::SystemDisconnected("Coordinator connection closed".into()));}
                        return outcome;
                    }
                    let(reply,rx)=oneshot::channel();tx.try_send(Event::SystemControl(Box::new(Envelope{uid,control:packet.control,reply})))?;
                    let response=tokio::time::timeout(Duration::from_secs(3),rx).await??;write(&mut socket,&Notice::Reply{response}).await
                }.await;
                if let Err(e)=result{let _=write(&mut socket,&Notice::Reply{response:Response{ok:false,message:e.to_string(),status:None}}).await;}
                });
            }
            #[allow(unreachable_code)] Ok::<(),anyhow::Error>(())
        })})();if let Err(e)=result{tracing::error!(%e,"System control server stopped");}})?;
        let activity = if peer {
            None
        } else {
            Some(system::Activity::start()?)
        };
        Ok(Self {
            policy,
            activity,
            instance: format!("{:032x}", rand::random::<u128>()),
            notices,
            snapshot,
        })
    }
    pub fn publish(&self, status: Status) {
        let mut previous = self.snapshot.lock().unwrap();
        if serde_json::to_vec(&*previous).ok() != serde_json::to_vec(&status).ok() {
            *previous = status.clone();
            let _ = self.notices.send(Notice::Snapshot { status });
        }
    }
    pub fn departure(&self, transaction: String, written_ns: u64) {
        let _ = self.notices.send(Notice::Departure {
            transaction,
            written_ns,
        });
    }
    pub fn restore_cursor(&self) {
        let _ = self.notices.send(Notice::RestoreCursor);
    }
}
fn subscribe(
    tx: crossbeam_channel::Sender<Notice>,
    errors: crossbeam_channel::Sender<Event>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("receiver-subscription".into())
        .spawn(move || {
            let r = (|| -> Result<()> {
                runtime()?.block_on(async {
                    let mut s = UnixStream::connect(HARDWARE_SOCKET).await?;
                    write(
                        &mut s,
                        &Packet {
                            version: VERSION,
                            control: Control::Watch,
                        },
                    )
                    .await?;
                    loop {
                        tx.send(read::<Notice>(&mut s).await?)?;
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), anyhow::Error>(())
                })
            })();
            let _ = errors.send(Event::SystemDisconnected(format!(
                "Receiver service disconnected: {r:?}"
            )));
        })?;
    Ok(())
}

pub fn run_client(paths: Paths, headless: bool, peer: bool) -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter("easy_switch_link=info,easy_switch_platform=warn")
        .try_init();
    use fs2::FileExt;
    let policy = system::policy()?.context("System service is not enabled")?;
    ensure!(
        !peer || policy.login_handoffs,
        "Machine peer coordination is not enabled"
    );
    ensure!(
        unsafe { libc::geteuid() }
            == if peer {
                policy.peer_uid
            } else {
                policy.owner_uid
            },
        "Wrong service client account"
    );
    if !peer {
        ensure!(
            paths.root == Paths::new(None)?.root,
            "System mode uses the designated user's default configuration directory"
        );
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.root.join("monitor.lock"))?;
    lock.try_lock_exclusive()
        .context("Session companion is already running")?;
    let (control_tx, control_rx) = crossbeam_channel::bounded(32);
    let (events, rx) = crossbeam_channel::bounded(256);
    let host = if peer {
        Some(Host::start(
            policy.clone(),
            true,
            events.clone(),
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
        )?)
    } else {
        ipc::serve(paths.clone(), control_tx.clone())?;
        None
    };
    let quit = control_tx.clone();
    ctrlc::set_handler(move || {
        let (tx, _) = oneshot::channel();
        let _ = quit.try_send((Request::Quit, tx));
    })?;
    if !headless && !peer {
        crate::tray::spawn(paths.clone())?;
    }
    let (notices, nrx) = crossbeam_channel::bounded(64);
    subscribe(notices, events.clone())?;
    let (dispatch, drx) = crossbeam_channel::bounded(16);
    let owns_coordinator = peer || !policy.login_handoffs;
    let manager = if owns_coordinator {
        Some(crate::follow::Manager::start(
            paths.clone(),
            dispatch,
            events.clone(),
        )?)
    } else {
        None
    };
    let mut status = Status::default();
    let mut departures: BTreeMap<
        String,
        (easy_switch_platform::desktop::Handle, Instant, Instant),
    > = BTreeMap::new();
    let mut running = true;
    let mut hardware_snapshot = Vec::new();
    while running {
        crossbeam_channel::select! {
            recv(nrx)->notice=>match notice?{
                Notice::RestoreCursor=>{departures.clear();if let Some(m)=&manager{let _=m.send(crate::follow::Command::RestoreCursor);}},
                Notice::Snapshot{status:new}=>{
                    let key=serde_json::to_vec(&(&new.config,&new.devices,new.paused,new.service.generation,&new.service.instance))?;
                    if key!=hardware_snapshot{if let Some(m)=&manager{m.send(crate::follow::Command::Update(crate::follow::Local{config:new.config.clone(),devices:new.devices.clone(),paused:new.paused,generation:new.service.generation}))?;}hardware_snapshot=key;}
                    status=new;
                }
                Notice::Departure{transaction,written_ns}=>{if let Some((desktop,at,until))=departures.remove(&transaction) && Instant::now()<until{let elapsed=Duration::from_nanos(system::monotonic_ns().saturating_sub(written_ns));if let Some(written)=Instant::now().checked_sub(elapsed){let _=desktop.send(easy_switch_platform::desktop::Command::Depart{crossing:at,written});}}}
                _=>{}
            },
            recv(control_rx)->v=>{let(request,reply)=v?;if matches!(request,Request::Quit){running=false;let _=reply.send(Response{ok:true,message:"Session companion stopped; hardware service remains active".into(),status:Some(status.clone())});}else{let result=client_request(request,&policy,&manager,&status);let _=reply.send(response(result,&status,&manager));}},
            recv(rx)->event=>match event?{
                Event::SystemControl(envelope)=>{let result=match envelope.control{Control::User{request}=>client_request(request,&policy,&manager,&status),_=>Err(anyhow::anyhow!("Unsupported peer operation"))};let _=envelope.reply.send(response(result,&status,&manager));},
                Event::LayoutConfig{generation,config}=>{let r=call(HARDWARE_SOCKET,Control::Layout{generation,config});if let Err(e)=r{tracing::warn!(%e,"Layout update unavailable");}},
                Event::CoordinatorStatus(state)=>{let _=call(HARDWARE_SOCKET,Control::Coordinator{state});},
                Event::Supersede(reply)=>{let result=call(HARDWARE_SOCKET,Control::Supersede).and_then(|r|{ensure!(r.ok,"{}",r.message);Ok(())}).map_err(|e|e.to_string());let _=reply.send(result);},
                Event::SystemDisconnected(reason)=>{if let Some(m)=&manager{m.shutdown();}anyhow::bail!("{reason}");},
                Event::Message(m)=>tracing::info!("{m}"),_=>{}
            },
            recv(drx)->request=>{let request=request?;let now=Instant::now();departures.retain(|_,(_,_,until)|*until>now);
                let result=(||->Result<()>{
                    ensure!(now<request.deadline,"Dispatch expired");
                    let state=manager.as_ref().context("No coordinator")?.status();
                    call(HARDWARE_SOCKET,Control::Coordinator{state:Box::new(state.clone())})?;
                    if let Some(d)=request.desktop.clone(){ensure!(departures.len()<32,"Too many pending departures");departures.insert(request.transaction.clone(),(d,request.at,now+Duration::from_secs(1)));}
                    let response=call(HARDWARE_SOCKET,Control::Edge{instance:status.service.instance.clone(),generation:request.generation,transaction:request.transaction.clone(),rule:request.rule.id.clone(),session:state.desktop.session,desktop_generation:state.desktop.generation,deadline_ns:system::monotonic_ns().saturating_add(request.deadline.saturating_duration_since(Instant::now()).as_nanos() as u64)})?;
                    ensure!(response.ok,"{}",response.message);Ok(())
                })();if result.is_err(){departures.remove(&request.transaction);}let _=request.reply.send(result.map_err(|e|e.to_string()));
            }
        }
        if let Some(host) = &host {
            let mut s = status.clone();
            if let Some(m) = &manager {
                s.follow = m.status();
            }
            host.publish(s);
        }
    }
    if let Some(m) = manager {
        m.shutdown();
        let _ = call(HARDWARE_SOCKET, Control::Cancel);
    }
    Ok(())
}
fn response(
    result: Result<String>,
    status: &Status,
    manager: &Option<crate::follow::Manager>,
) -> Response {
    if status.service.instance.is_empty() {
        return Response {
            ok: false,
            message: "Connecting to the receiver service".into(),
            status: None,
        };
    }
    let mut status = status.clone();
    if let Some(m) = manager {
        status.follow = m.status();
    }
    Response {
        ok: result.is_ok(),
        message: result.unwrap_or_else(|e| e.to_string()),
        status: Some(status),
    }
}
fn client_request(
    request: Request,
    p: &Policy,
    manager: &Option<crate::follow::Manager>,
    status: &Status,
) -> Result<String> {
    ensure!(
        !status.service.instance.is_empty(),
        "Connecting to the receiver service"
    );
    if matches!(request, Request::Status) {
        return Ok("Ready".into());
    }
    if let Request::Follow {
        action: easy_switch_core::follow::Action::SetRule { id, enabled },
    } = &request
    {
        let mut config = status.config.clone();
        config
            .follow
            .rules
            .iter_mut()
            .find(|r| &r.id == id)
            .context("Unknown rule")?
            .enabled = *enabled;
        let r = hardware(Request::Save { config })?;
        ensure!(r.ok, "{}", r.message);
        return Ok(r.message);
    }
    if let Request::Follow { action } = &request {
        if let Some(m) = manager {
            let s = system::active_session(p)?;
            ensure!(
                s.uid == p.owner_uid && !s.greeter && !s.locked,
                "Settings require the owner's unlocked desktop"
            );
            if let easy_switch_core::follow::Action::SetRule { id, enabled } = action {
                let mut config = status.config.clone();
                config
                    .follow
                    .rules
                    .iter_mut()
                    .find(|r| &r.id == id)
                    .context("Unknown rule")?
                    .enabled = *enabled;
                let r = hardware(Request::Save { config })?;
                ensure!(r.ok, "{}", r.message);
                return Ok(r.message);
            }
            m.send(crate::follow::Command::Action(action.clone()))?;
            return Ok("Edge action queued".into());
        }
        let r = call(PEER_SOCKET, Control::User { request })?;
        ensure!(r.ok, "{}", r.message);
        return Ok(r.message);
    }
    let r = hardware(request)?;
    ensure!(r.ok, "{}", r.message);
    Ok(r.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            version: 1,
            owner_uid: 1000,
            receiver_uid: 901,
            peer_uid: 902,
            greeter_uid: 120,
            seat: "seat0".into(),
            login_handoffs: true,
        }
    }
    #[test]
    fn peer_and_greeter_cannot_change_receiver_configuration() {
        let p = policy();
        for uid in [0, 120, 902, 1001] {
            assert!(
                allowed(
                    &p,
                    uid,
                    &Control::User {
                        request: Request::Save {
                            config: Default::default()
                        }
                    },
                    false
                )
                .is_err()
            );
        }
        assert!(
            allowed(
                &p,
                1000,
                &Control::User {
                    request: Request::Status
                },
                false
            )
            .is_ok()
        );
        assert!(allowed(&p, 120, &Control::Watch, false).is_err());
        assert!(allowed(&p, 902, &Control::Watch, false).is_ok());
    }
    #[test]
    fn only_selected_coordinator_can_dispatch() {
        let mut p = policy();
        assert!(allowed(&p, 902, &Control::Cancel, false).is_ok());
        assert!(allowed(&p, 1000, &Control::Cancel, false).is_err());
        assert!(allowed(&p, 902, &Control::Cancel, true).is_err());
        p.login_handoffs = false;
        assert!(allowed(&p, 902, &Control::Cancel, false).is_err());
        assert!(allowed(&p, 1000, &Control::Cancel, false).is_ok());
    }
    #[test]
    fn oversized_service_frames_are_rejected_before_body_read() {
        runtime().unwrap().block_on(async {
            let (mut a, mut b) = UnixStream::pair().unwrap();
            a.write_u32_le(u32::MAX).await.unwrap();
            assert!(
                read::<Packet>(&mut b)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("too large")
            );
        });
    }
}
