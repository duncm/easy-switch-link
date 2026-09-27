//! A local, authenticated compositor companion. No general desktop-control API.
use super::*;
use crate::system;
use anyhow::Context;
use easy_switch_core::{
    follow::Display,
    service::{DESKTOP_SOCKET, Policy},
};
use serde::Deserialize;
use serde_json::json;
use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        UnixListener,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};

const LIMIT: usize = 65536;
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Layout {
        version: u32,
        displays: Vec<Display>,
        cursor: bool,
    },
    Motion {
        point: Point,
        delta: Point,
        buttons: bool,
        synthetic: bool,
    },
    Activity,
    Placed {
        id: u64,
        ok: bool,
    },
    Cursor {
        hidden: bool,
        #[serde(default)]
        reason: Option<String>,
    },
}
enum Incoming {
    Input(u64, Input),
    Gone(u64),
    Refresh,
}
async fn read(r: &mut OwnedReadHalf) -> Result<Input> {
    let n = r.read_u32_le().await? as usize;
    ensure!(n > 0 && n <= LIMIT, "Companion frame too large");
    let mut b = vec![0; n];
    r.read_exact(&mut b).await?;
    Ok(serde_json::from_slice(&b)?)
}
async fn send(w: &mut OwnedWriteHalf, v: serde_json::Value) -> Result<()> {
    let b = serde_json::to_vec(&v)?;
    ensure!(b.len() <= LIMIT, "Companion frame too large");
    tokio::time::timeout(Duration::from_millis(100), async {
        w.write_u32_le(b.len() as u32).await?;
        w.write_all(&b).await
    })
    .await??;
    Ok(())
}
struct Client {
    sequence: u64,
    uid: u32,
    pid: u32,
    write: OwnedWriteHalf,
    session: String,
}
type Placement = (u64, Instant, oneshot::Sender<Result<u64, String>>);

pub fn start(
    mut commands: mpsc::Receiver<Command>,
    events: crossbeam_channel::Sender<Event>,
    directory: std::path::PathBuf,
    activity: Arc<InputActivity>,
    _tx: mpsc::Sender<Command>,
) -> Result<Arc<dyn Fn() + Send + Sync>> {
    let policy = system::policy()?;
    let uid = unsafe { libc::geteuid() };
    let machine = policy
        .as_ref()
        .is_some_and(|p| p.login_handoffs && p.peer_uid == uid);
    let path = if machine {
        std::path::PathBuf::from(DESKTOP_SOCKET)
    } else {
        std::path::PathBuf::from(
            std::env::var("XDG_RUNTIME_DIR").context("No session runtime directory")?,
        )
        .join("easy-switch-link-gnome.sock")
    };
    if path.exists() {
        ensure!(
            std::os::unix::net::UnixStream::connect(&path).is_err(),
            "GNOME companion endpoint already owned"
        );
        std::fs::remove_file(&path)?;
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    std::fs::set_permissions(
        &path,
        std::fs::Permissions::from_mode(if machine { 0o666 } else { 0o600 }),
    )?;
    let mut p = policy.unwrap_or(Policy {
        version: 1,
        owner_uid: uid,
        receiver_uid: 0,
        peer_uid: 0,
        greeter_uid: 0,
        seat: "seat0".into(),
        login_handoffs: false,
    });
    if !machine {
        p.login_handoffs = false;
    }
    std::thread::Builder::new().name("gnome-companion".into()).spawn(move||{
        let result=(||->Result<()>{
            let mut model=Model::new("GNOME companion",events);model.activity=activity;model.load_bindings(&directory)?;
            model.unavailable("Enable the Easy-Switch Link GNOME companion");
            let(listener_tx,mut incoming)=mpsc::channel(64);let wake=listener_tx.clone();system::watch_sessions(move||{let _=wake.try_send(Incoming::Refresh);})?;
            let rt=tokio::runtime::Builder::new_current_thread().enable_all().build()?;rt.block_on(async{
            let listener=UnixListener::from_std(listener)?;let mut client:Option<Client>=None;let mut placement:Option<Placement>=None;let mut next=0u64;
            loop {
                let deadline=placement.as_ref().map(|v|v.1).unwrap_or_else(||Instant::now()+Duration::from_secs(86400*365));
                tokio::select!{
                    accepted=listener.accept()=>{
                        let(socket,_)=accepted?;let(uid,pid)=system::credentials(socket.as_raw_fd())?;
                        if uid!=p.owner_uid&&(!p.login_handoffs||uid!=p.greeter_uid){continue;}
                        let Ok(session)=authorize(&p,uid,pid).await else{continue;};
                        next=next.wrapping_add(1);let sequence=next;let(read,write)=socket.into_split();let tx=listener_tx.clone();
                        // Replacing an active controller always restores the old cursor.
                        if let Some(mut old)=client.take(){let _=send(&mut old.write,json!({"command":"restore"})).await;}
                        reset(&mut model,&mut placement,"Compositor connecting");
                        model.state.session=session.id.clone();model.state.session_kind=if session.greeter{"greeter"}else if session.locked{"locked"}else{"user"}.into();model.state.locked=session.locked;model.state.lock_handoffs=p.login_handoffs;
                        client=Some(Client{sequence,uid,pid,write,session:session.id});
                        let task_tx=tx.clone();tokio::spawn(async move{let mut read=read;while let Ok(input)=read_input(&mut read).await{if task_tx.send(Incoming::Input(sequence,input)).await.is_err(){return;}}let _=task_tx.send(Incoming::Gone(sequence)).await;});
                        if let Some(c)=&mut client{send(&mut c.write,json!({"command":"hello","version":1})).await?;}
                    },
                    event=incoming.recv()=>{
                        match event.context("Companion events closed")?{
                            Incoming::Refresh=>{
                                if let Some(c)=&mut client {
                                    let current=authorize(&p,c.uid,c.pid).await;
                                    if !current.as_ref().is_ok_and(|s|s.id==c.session){let _=send(&mut c.write,json!({"command":"restore"})).await;client=None;reset(&mut model,&mut placement,"Active desktop changed");}
                                    else if let Ok(s)=current{reset(&mut model,&mut placement,"Session state changed");model.state.locked=s.locked;model.state.session_kind=if s.greeter{"greeter"}else if s.locked{"locked"}else{"user"}.into();send(&mut c.write,json!({"command":"hello","version":1})).await?;}
                                }
                            }
                            Incoming::Gone(sequence)=>{if client.as_ref().is_some_and(|c|c.sequence==sequence){client=None;reset(&mut model,&mut placement,"GNOME companion disconnected");}}
                            Incoming::Input(sequence,input)=>{
                                let Some(c)=client.as_mut().filter(|c|c.sequence==sequence)else{continue;};
                                match input {
                                    Input::Layout{version,displays,cursor}=>{
                                        ensure!(version==1&&displays.len()<=32,"Incompatible companion layout");
                                        model.cursor.restore();model.topology(displays);model.state.cursor_visibility=if cursor{"GNOME compositor cursor control"}else{"GNOME cursor API unavailable"}.into();model.publish();
                                        send(&mut c.write,json!({"command":"configure","enabled":model.enabled,"rules":model.rules,"displays":model.state.displays})).await?;
                                    }
                                    Input::Motion{point,delta,buttons,synthetic}=>{
                                        if [point.x,point.y,delta.x,delta.y].iter().all(|v|v.is_finite()&&v.abs()<1_000_000.0){let previous=model.cursor.crossing;model.movement(point,delta,buttons,synthetic);if model.cursor.crossing!=previous{send(&mut c.write,json!({"command":"release"})).await?;}}
                                    }
                                    Input::Activity=>{model.input();model.state.cursor_hidden=false;}
                                    Input::Cursor{hidden,reason}=>{model.cursor.hidden=hidden;model.state.cursor_hidden=hidden;if let Some(reason)=reason{tracing::info!(reason=%reason.chars().take(160).collect::<String>(),"GNOME cursor visibility");}model.publish();}
                                    Input::Placed{id,ok}=>{if finish_placement(&mut model,&mut placement,id,ok){send(&mut c.write,json!({"command":"cancel","id":id})).await?;}}
                                }
                            }
                        }
                    },
                    command=commands.recv()=>{
                        let Some(command)=command else{break;};
                        match command{
                            Command::Stop=>break,
                            Command::Configure{enabled,rules}=>{model.cursor.restore();model.latch.reset();model.enabled=enabled;model.rules=rules;if let Some(c)=&mut client{send(&mut c.write,json!({"command":"configure","enabled":enabled,"rules":model.rules,"displays":model.state.displays})).await?;}},
                            Command::Restore(reply)=>{model.cursor.restore();if let Some(c)=&mut client{let _=send(&mut c.write,json!({"command":"restore"})).await;}let _=reply.send(());},
                            Command::Depart{crossing,written}=>{model.cursor.depart(crossing,written,Instant::now());if model.cursor.hidden&&let Some(c)=&mut client{send(&mut c.write,json!({"command":"hide","written_ns":system::monotonic_ns().saturating_sub(written.elapsed().as_nanos() as u64).to_string()})).await?;}},
                            Command::Place{deadline,segment,fraction,generation,reply}=>{
                                let authorized=if let Some(c)=&client{authorize(&p,c.uid,c.pid).await}else{Err(anyhow::anyhow!("No compositor"))};
                                let result=(||->Result<(u64,Point)>{ensure!(Instant::now()<deadline,"Placement expired");let c=client.as_ref().context("No compositor")?;let s=authorized?;ensure!(s.id==c.session,"Desktop changed");model.placement(&segment,fraction,generation)})();
                                match result{Ok((id,point))=>{model.state.pointer=point;placement=Some((id,deadline,reply));if let Some(c)=&mut client{let until=system::monotonic_ns().saturating_add(deadline.saturating_duration_since(Instant::now()).as_nanos() as u64);send(&mut c.write,json!({"command":"place","id":id,"point":point,"deadline_ns":until.to_string()})).await?;}},Err(e)=>{let _=reply.send(Err(e.to_string()));}}
                            }
                            Command::Cancel(id)=>{model.cancel(id);if let Some(c)=&mut client{send(&mut c.write,json!({"command":"cancel","id":id})).await?;}},
                            Command::Complete(id)=>{model.complete(id);if let Some(c)=&mut client{send(&mut c.write,json!({"command":"complete","id":id})).await?;}},
                            Command::Authorize=>{if let Some(c)=&mut client{send(&mut c.write,json!({"command":"hello","version":1})).await?;}},
                            Command::SessionLocked(_)=>{}
                        }
                    },
                    _=tokio::time::sleep_until(deadline.into()),if placement.is_some()=>{if let Some((id,_,reply))=placement.take(){model.cancel(id);let _=reply.send(Err("Compositor placement timed out".into()));if let Some(c)=&mut client{send(&mut c.write,json!({"command":"cancel","id":id})).await?;}}}
                }
            }
            if let Some(mut c)=client{let _=send(&mut c.write,json!({"command":"restore"})).await;}
            Ok(())
        })})();if let Err(e)=result{tracing::warn!(%e,"GNOME companion stopped");}let _=std::fs::remove_file(path);
    })?;
    Ok(Arc::new(|| {}))
}
// Return whether the companion must cancel its matching reservation. Its own
// input serial decides whether restoring the old cursor position is still safe.
fn finish_placement(
    model: &mut Model,
    placement: &mut Option<Placement>,
    id: u64,
    ok: bool,
) -> bool {
    if !placement.as_ref().is_some_and(|v| v.0 == id) {
        return false;
    }
    let (_, deadline, reply) = placement.take().unwrap();
    let accepted =
        ok && Instant::now() < deadline && model.reservation.as_ref().is_some_and(|r| r.id == id);
    if !accepted {
        model.cancel(id);
    }
    let delivered = reply.send(if accepted {
        Ok(id)
    } else {
        Err("Compositor placement failed or expired".into())
    });
    if delivered.is_err() {
        model.cancel(id);
    }
    !accepted || delivered.is_err()
}

async fn authorize(policy: &Policy, uid: u32, pid: u32) -> Result<system::Session> {
    let policy = policy.clone();
    tokio::task::spawn_blocking(move || system::authorize_desktop(&policy, uid, pid)).await?
}
async fn read_input(read: &mut OwnedReadHalf) -> Result<Input> {
    read_frame(read).await
}
async fn read_frame(r: &mut OwnedReadHalf) -> Result<Input> {
    read(r).await
}
fn reset(model: &mut Model, placement: &mut Option<Placement>, reason: &str) {
    model.activity.cancel();
    model.cursor.restore();
    model.latch.reset();
    model.state.generation = model.state.generation.wrapping_add(1);
    model.state.cursor_hidden = false;
    model.unavailable(reason);
    if let Some((_, _, reply)) = placement.take() {
        let _ = reply.send(Err(reason.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_late_and_abandoned_placements_release_reservations() {
        for scenario in 0..5 {
            let (events, _) = crossbeam_channel::unbounded();
            let mut model = Model::new("test", events);
            model.reservation = Some(Reservation {
                id: 7,
                previous: Point::default(),
                serial: 0,
                until: Instant::now() + Duration::from_secs(5),
            });
            // Activity prevents restoration, but must not prevent cleanup.
            model.input();
            let (reply, receiver) = oneshot::channel();
            let mut receiver = Some(receiver);
            if scenario == 2 {
                receiver.take();
            }
            if scenario == 4 {
                model.unavailable("Display removed during placement");
            }
            let deadline = if scenario == 1 {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(1)
            };
            let mut pending = Some((7, deadline, reply));
            assert!(!finish_placement(&mut model, &mut pending, 6, false));
            assert!(pending.is_some());
            let cancel = finish_placement(&mut model, &mut pending, 7, scenario != 0);
            assert_eq!(cancel, scenario != 3);
            assert_eq!(model.reservation.is_none(), scenario != 3);
            assert!(pending.is_none());
        }
    }
}
