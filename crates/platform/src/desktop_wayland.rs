//! Wayland uses only public portals/EIS. No compositor-private APIs, input-device
//! permissions, screen streams, or persistent input diversion are required.
use super::*;
use anyhow::{Context, bail};
use ashpd::desktop::{PersistMode, Session, input_capture as capture, remote_desktop as remote};
use easy_switch_core::follow::{Display, Edge};
use futures_util::StreamExt;
use reis::{
    ei,
    event::{DeviceCapability, EiEvent},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    os::unix::net::UnixStream,
};

pub fn start(
    mut commands: mpsc::Receiver<Command>,
    events: crossbeam_channel::Sender<Event>,
    private_dir: std::path::PathBuf,
) -> Result<Arc<dyn Fn() + Send + Sync>> {
    std::thread::Builder::new()
        .name("wayland-portals".into())
        .spawn(move || {
            let mut model = Model::new("Wayland portals/EIS", events);
            model.unavailable("Grant desktop access in Destination follows mouse settings");
            let result = (|| -> Result<()> {
                model.load_bindings(&private_dir)?;
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                rt.block_on(async {
                    while let Some(command) = commands.recv().await {
                        match command {
                            Command::Stop => break,
                            Command::Restore(reply) => {
                                let _ = reply.send(());
                            }
                            Command::Configure { enabled, rules } => {
                                model.enabled = enabled;
                                model.rules = rules;
                            }
                            Command::SessionLocked(locked) => {
                                model.session_locked = locked;
                                model.state.locked = locked;
                                model.publish();
                            }
                            Command::Authorize => {
                                if let Err(e) =
                                    session(&mut commands, &mut model, &private_dir).await
                                {
                                    model.unavailable(format!(
                                        "Wayland permission/capability: {e:#}"
                                    ));
                                }
                            }
                            Command::Place { reply, .. } => {
                                let _ = reply.send(Err(
                                    "Wayland desktop permission is not established".into(),
                                ));
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                })
            })();
            if let Err(e) = result {
                model.unavailable(e.to_string());
            }
        })?;
    Ok(Arc::new(|| {}))
}
async fn set_barriers(
    portal: &capture::InputCapture,
    session: &Session<capture::InputCapture>,
    model: &Model,
) -> Result<BTreeMap<u32, String>> {
    let _ = portal.disable(session, Default::default()).await;
    let zones = portal
        .zones(session, Default::default())
        .await?
        .response()?;
    let mut barriers = vec![];
    let mut names = BTreeMap::new();
    if model.enabled && !model.state.locked {
        for (index, rule) in model.rules.iter().enumerate().filter(|(_, r)| r.enabled) {
            let s = &rule.source;
            let Some(d) = model
                .state
                .displays
                .iter()
                .find(|d| d.id == s.display && d.binding == s.binding)
            else {
                continue;
            };
            ensure!(
                zones.regions().iter().any(|z| z.x_offset() as f64 == d.x
                    && z.y_offset() as f64 == d.y
                    && z.width() as f64 == d.width
                    && z.height() as f64 == d.height),
                "Portal zone no longer matches EIS display"
            );
            let (x1, y1, x2, y2) = match s.edge {
                Edge::Left => (
                    d.x,
                    d.y + d.height * s.start / 100.0,
                    d.x,
                    d.y + d.height * s.end / 100.0 - 1.0,
                ),
                Edge::Right => (
                    d.x + d.width,
                    d.y + d.height * s.start / 100.0,
                    d.x + d.width,
                    d.y + d.height * s.end / 100.0 - 1.0,
                ),
                Edge::Top => (
                    d.x + d.width * s.start / 100.0,
                    d.y,
                    d.x + d.width * s.end / 100.0 - 1.0,
                    d.y,
                ),
                Edge::Bottom => (
                    d.x + d.width * s.start / 100.0,
                    d.y + d.height,
                    d.x + d.width * s.end / 100.0 - 1.0,
                    d.y + d.height,
                ),
            };
            let id = index as u32 + 1;
            barriers.push(capture::Barrier::new(
                NonZeroU32::new(id).unwrap(),
                (x1 as i32, y1 as i32, x2 as i32, y2 as i32),
            ));
            names.insert(id, rule.id.clone());
        }
    }
    let response = portal
        .set_pointer_barriers(session, &barriers, zones.zone_set(), Default::default())
        .await?
        .response()?;
    ensure!(
        response.failed_barriers().is_empty(),
        "Compositor rejected edge barriers (only exposed desktop boundaries are supported)"
    );
    if model.enabled && !model.state.locked {
        portal.enable(session, Default::default()).await?;
    }
    Ok(names)
}
async fn session(
    commands: &mut mpsc::Receiver<Command>,
    model: &mut Model,
    private_dir: &std::path::Path,
) -> Result<()> {
    let mut tokens: Tokens = std::fs::read(private_dir.join("portal-tokens.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let input = capture::InputCapture::new().await?;
    let input_session = if input.version() >= 2 {
        let s = input.create_session2(Default::default()).await?;
        let granted = input
            .start(
                &s,
                None,
                capture::StartOptions::default()
                    .set_restore_token(tokens.capture.clone())
                    .set_capabilities(capture::Capabilities::Pointer.into())
                    .set_persist_mode(PersistMode::ExplicitlyRevoked),
            )
            .await?
            .response()?;
        tokens.capture = granted.restore_token().map(str::to_owned);
        s
    } else {
        input
            .create_session(
                None,
                capture::CreateSessionOptions::default()
                    .set_capabilities(capture::Capabilities::Pointer.into()),
            )
            .await?
            .0
    };
    let desktop = remote::RemoteDesktop::new().await?;
    let desktop_session = desktop.create_session(Default::default()).await?;
    desktop
        .select_devices(
            &desktop_session,
            remote::SelectDevicesOptions::default()
                .set_restore_token(tokens.desktop.as_deref())
                .set_devices(Some(remote::DeviceType::Pointer.into()))
                .set_persist_mode(PersistMode::ExplicitlyRevoked),
        )
        .await?;
    let granted = desktop
        .start(&desktop_session, None, Default::default())
        .await?
        .response()?;
    tokens.desktop = granted.restore_token().map(str::to_owned);
    tokens.save(private_dir)?;
    let send_stream = UnixStream::from(
        desktop
            .connect_to_eis(&desktop_session, Default::default())
            .await?,
    );
    send_stream.set_nonblocking(true)?;
    let send_context = ei::Context::new(send_stream)?;
    let (send_connection, mut sender) = send_context
        .handshake_tokio(
            "Easy-Switch Link cursor placement",
            ei::handshake::ContextType::Sender,
        )
        .await?;
    let receive_stream = UnixStream::from(
        input
            .connect_to_eis(&input_session, Default::default())
            .await?,
    );
    receive_stream.set_nonblocking(true)?;
    let receive_context = ei::Context::new(receive_stream)?;
    let (_receive_connection, mut receiver) = receive_context
        .handshake_tokio(
            "Easy-Switch Link edge capture",
            ei::handshake::ContextType::Receiver,
        )
        .await?;
    let mut activated = input.receive_activated().await?;
    let mut changed = input.receive_zones_changed().await?;
    let mut disabled = input.receive_disabled().await?;

    let mut devices: Vec<reis::event::Device> = vec![];
    let mut resumed = BTreeSet::new();
    let mut names = BTreeMap::new();
    let mut buttons = BTreeSet::new();
    let mut sequence = 0u32;
    let mut capture_sequence = None;
    let result=async{loop{tokio::select!{
        command=commands.recv()=>{let Some(command)=command else{break};match command {
            Command::Stop=>{commands.close();break;},Command::Authorize=>{},
            // Portals cannot hide a desktop cursor without capturing all input.
            Command::Depart{..}=>{},
            Command::Restore(reply)=>{let _=reply.send(());},
            Command::SessionLocked(locked)=>{model.session_locked=locked;model.state.locked=locked;model.reservation=None;model.publish();names=set_barriers(&input,&input_session,model).await?;},
            Command::Configure{enabled,rules}=>{model.enabled=enabled;model.rules=rules;model.reservation=None;names=set_barriers(&input,&input_session,model).await?;},
            Command::Place{segment,fraction,generation,reply,deadline}=>{ if reply.is_closed() || Instant::now()>=deadline { continue; }
                let result=(||->Result<u64>{let(id,p)=model.placement(&segment,fraction,generation)?;let device=devices.iter().find(|d|resumed.contains(&format!("{:?}",d.device())) && d.regions().iter().any(|r|r.mapping_id.as_ref().is_some_and(|v|format!("wayland:{v}")==segment.display))).context("No active EIS absolute-position region")?;let pointer=device.interface::<ei::PointerAbsolute>().context("Compositor does not support absolute cursor placement")?;sequence=sequence.wrapping_add(1);let serial=send_connection.serial();device.device().start_emulating(sequence,serial);pointer.motion_absolute(p.x as f32,p.y as f32);device.device().frame(serial,0);device.device().stop_emulating(serial);send_connection.flush()?;model.state.pointer=p;Ok(id)})();let _=reply.send(result.map_err(|e|e.to_string()));
            },
            // InputCapture does not expose input outside active capture. We cannot
            // prove the pointer stayed untouched, so never roll it back blindly.
            Command::Cancel(id)|Command::Complete(id)=>model.complete(id),
        }},
        event=sender.next()=>{match event.context("EIS sender disconnected")?? {
            EiEvent::SeatAdded(e)=>{e.seat.bind_capabilities(DeviceCapability::PointerAbsolute.into());send_context.flush()?;},
            EiEvent::DeviceAdded(e)=>{if e.device.has_capability(DeviceCapability::PointerAbsolute){devices.push(e.device);}},
            EiEvent::DeviceResumed(e)=>{resumed.insert(format!("{:?}",e.device.device()));let mut displays=vec![];for d in &devices{for region in d.regions(){if let Some(id)=&region.mapping_id {displays.push(Display{id:format!("wayland:{id}"),name:format!("{} ({id})",d.name().unwrap_or("Display")),x:region.x as f64,y:region.y as f64,width:region.width as f64,height:region.height as f64,scale:1.0,binding:format!("{id}:{},{},{},{},{}",region.x,region.y,region.width,region.height,region.scale)});}}}displays.sort_by(|a,b|a.id.cmp(&b.id));displays.dedup_by(|a,b|a.id==b.id);model.state.ready= !displays.is_empty();model.state.locked=model.session_locked;model.state.reason=if displays.is_empty(){"No mapped absolute-position regions supplied by compositor".into()}else{String::new()};model.topology(displays);names=set_barriers(&input,&input_session,model).await?;model.publish();},
            EiEvent::DevicePaused(e)=>{resumed.remove(&format!("{:?}",e.device.device()));model.state.locked=true;model.publish();},
            EiEvent::DeviceRemoved(e)=>{devices.retain(|d|d!=&e.device);resumed.remove(&format!("{:?}",e.device.device()));model.unavailable("EIS display removed; verify display bindings");},
            EiEvent::Disconnected(_)=>bail!("EIS permission/session closed"),_=>{}
        }},
        event=receiver.next()=>{match event.context("EIS receiver disconnected")?? {EiEvent::SeatAdded(e)=>{e.seat.bind_capabilities(DeviceCapability::Pointer|DeviceCapability::PointerAbsolute|DeviceCapability::Button);receive_context.flush()?;},EiEvent::DeviceStartEmulating(e)=>{capture_sequence=Some(e.sequence);buttons.clear();},EiEvent::Button(e)=>{if e.state==ei::button::ButtonState::Press{buttons.insert(e.button);}else{buttons.remove(&e.button);}model.state.input_serial=model.state.input_serial.wrapping_add(1);},EiEvent::PointerMotion(e)=>{model.state.input_serial=model.state.input_serial.wrapping_add(1);let p=Point{x:model.state.pointer.x+e.dx as f64,y:model.state.pointer.y+e.dy as f64};let _=model.latch.eligible(p,Instant::now());},EiEvent::Disconnected(_)=>bail!("InputCapture EIS session closed"),_=>{}}},
        event=activated.next()=>{let e=event.context("Capture portal closed")?;let position=e.cursor_position();let barrier=e.barrier_id();
            // Synchronize portal activation with an EIS frame before examining
            // held buttons. Always release capture within a bounded interval.
            let state_ready=tokio::time::timeout(Duration::from_millis(40),async {
                loop {match receiver.next().await {
                    Some(Ok(EiEvent::DeviceStartEmulating(event)))=>{capture_sequence=Some(event.sequence);buttons.clear();},
                    Some(Ok(EiEvent::Button(event)))=>{if event.state==ei::button::ButtonState::Press{buttons.insert(event.button);}else{buttons.remove(&event.button);}},
                    Some(Ok(EiEvent::Frame(_))) if capture_sequence==e.activation_id()=>return true,
                    None|Some(Err(_))|Some(Ok(EiEvent::Disconnected(_)))=>return false,
                    _=>{}
                }}
            }).await.unwrap_or(false);
            if state_ready && let (Some((x,y)),Some(capture::ActivatedBarrier::Barrier(barrier)))=(position,barrier)&& let Some(id)=names.get(&barrier.get())&& let Some(rule)=model.rules.iter().find(|r|r.id==*id).cloned()&& let Some(d)=model.state.displays.iter().find(|d|d.id==rule.source.display){let p=Point{x:(x as f64).clamp(d.x,d.x+d.width-1.0),y:(y as f64).clamp(d.y,d.y+d.height-1.0)};let delta=match rule.source.edge{Edge::Left=>Point{x:-1.0,y:0.0},Edge::Right=>Point{x:1.0,y:0.0},Edge::Top=>Point{x:0.0,y:-1.0},Edge::Bottom=>Point{x:0.0,y:1.0}};model.latch.cooldown_elapsed(Instant::now());model.movement(p,delta,!buttons.is_empty(),false);}
            input.release(&input_session,capture::ReleaseOptions::default().set_activation_id(e.activation_id())).await?;
        },
        _=disabled.next()=>{model.unavailable("Desktop disabled input capture; grant access again");bail!("Input capture disabled");},
        _=changed.next()=>{model.unavailable("Display topology changed; authorize and verify display bindings again");bail!("Display zones changed");},

    }}Ok(())}.await;
    let _ = input.disable(&input_session, Default::default()).await;
    let _ = input_session.close().await;
    let _ = desktop_session.close().await;
    result
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Tokens {
    capture: Option<String>,
    desktop: Option<String>,
}
impl Tokens {
    fn save(&self, dir: &std::path::Path) -> Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let temporary = dir.join("portal-tokens.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        std::fs::rename(temporary, dir.join("portal-tokens.json"))?;
        Ok(())
    }
}
