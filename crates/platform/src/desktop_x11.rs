use super::*;
use easy_switch_core::follow::{Display, Edge};
use std::{
    io::{Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event as XEvent,
        randr::{ConnectionExt as _, NotifyMask},
        screensaver::ConnectionExt as _,
        xfixes::{BarrierDirections, ConnectionExt as _},
        xinput::{BarrierReleasePointerInfo, ConnectionExt as _, Device, EventMask, XIEventMask},
        xproto::{ConnectionExt as _, KeyButMask, Window},
    },
    rust_connection::RustConnection,
};
fn displays(c: &RustConnection, root: Window) -> Result<Vec<Display>> {
    let monitors = c.randr_get_monitors(root, true)?.reply()?;
    let resource = c.intern_atom(false, b"RESOURCE_MANAGER")?.reply()?.atom;
    let resources = c
        .get_property(
            false,
            root,
            resource,
            x11rb::protocol::xproto::AtomEnum::STRING,
            0,
            65536,
        )?
        .reply()?
        .value;
    let scale = String::from_utf8_lossy(&resources)
        .lines()
        .find_map(|line| {
            line.strip_prefix("Xft.dpi:")
                .and_then(|value| value.trim().parse::<f64>().ok())
        })
        .map(|dpi| (dpi / 96.0).clamp(0.25, 8.0))
        .unwrap_or(1.0);
    let mut out = vec![];
    for m in monitors.monitors {
        let name = String::from_utf8_lossy(&c.get_atom_name(m.name)?.reply()?.name).into_owned();
        let mut identity = name.clone();
        if let Some(output) = m.outputs.first() {
            let edid = c.intern_atom(false, b"EDID")?.reply()?.atom;
            let data = c
                .randr_get_output_property(
                    *output,
                    edid,
                    x11rb::protocol::xproto::AtomEnum::ANY,
                    0,
                    128,
                    false,
                    false,
                )?
                .reply()?
                .data;
            for b in data {
                identity.push_str(&format!("{b:02x}"));
            }
        }
        out.push(Display {
            id: format!("x11:{identity}"),
            name,
            x: m.x as f64,
            y: m.y as f64,
            width: m.width as f64,
            height: m.height as f64,
            scale,
            binding: format!("{},{},{},{},{}", m.x, m.y, m.width, m.height, scale),
        });
    }
    Ok(out)
}
fn warp(c: &RustConnection, root: Window, p: Point) -> Result<()> {
    c.warp_pointer(
        x11rb::NONE,
        root,
        0,
        0,
        0,
        0,
        p.x.round() as i16,
        p.y.round() as i16,
    )?
    .check()?;
    c.flush()?;
    let (actual, _) = pointer(c, root)?;
    ensure!(
        (actual.x - p.x).abs() <= 1.0 && (actual.y - p.y).abs() <= 1.0,
        "Another application constrained cursor placement"
    );
    Ok(())
}
fn pointer(c: &RustConnection, root: Window) -> Result<(Point, bool)> {
    let p = c.query_pointer(root)?.reply()?;
    Ok((
        Point {
            x: p.root_x as f64,
            y: p.root_y as f64,
        },
        u16::from(p.mask)
            & u16::from(
                KeyButMask::BUTTON1
                    | KeyButMask::BUTTON2
                    | KeyButMask::BUTTON3
                    | KeyButMask::BUTTON4
                    | KeyButMask::BUTTON5,
            )
            != 0,
    ))
}
fn barriers(c: &RustConnection, root: Window, model: &Model, old: &mut Vec<u32>) -> Result<()> {
    for b in old.drain(..) {
        c.xfixes_delete_pointer_barrier(b)?.check()?;
    }
    if !model.enabled || model.state.locked {
        return Ok(());
    }
    for rule in model.rules.iter().filter(|r| r.enabled) {
        let s = &rule.source;
        let Some(d) = model
            .state
            .displays
            .iter()
            .find(|d| d.id == s.display && d.binding == s.binding)
        else {
            continue;
        };
        let (x1, y1, x2, y2, dir) = match s.edge {
            Edge::Left => (
                d.x,
                d.y + d.height * s.start / 100.0,
                d.x,
                d.y + d.height * s.end / 100.0,
                BarrierDirections::POSITIVE_X,
            ),
            Edge::Right => (
                d.x + d.width,
                d.y + d.height * s.start / 100.0,
                d.x + d.width,
                d.y + d.height * s.end / 100.0,
                BarrierDirections::NEGATIVE_X,
            ),
            Edge::Top => (
                d.x + d.width * s.start / 100.0,
                d.y,
                d.x + d.width * s.end / 100.0,
                d.y,
                BarrierDirections::POSITIVE_Y,
            ),
            Edge::Bottom => (
                d.x + d.width * s.start / 100.0,
                d.y + d.height,
                d.x + d.width * s.end / 100.0,
                d.y + d.height,
                BarrierDirections::NEGATIVE_Y,
            ),
        };
        let id = c.generate_id()?;
        c.xfixes_create_pointer_barrier(
            id,
            root,
            x1 as u16,
            y1 as u16,
            x2 as u16,
            y2 as u16,
            dir,
            &[],
        )?
        .check()?;
        old.push(id);
    }
    c.flush()?;
    Ok(())
}
pub fn start(
    mut commands: mpsc::Receiver<Command>,
    events: crossbeam_channel::Sender<Event>,
    private_dir: std::path::PathBuf,
) -> Result<Arc<dyn Fn() + Send + Sync>> {
    let (mut read, write) = UnixStream::pair()?;
    read.set_nonblocking(true)?;
    write.set_nonblocking(true)?;
    let write = Arc::new(write);
    std::thread::Builder::new()
        .name("x11-edges".into())
        .spawn(move || {
            let mut model = Model::new("X11 XInput2/XFixes", events);
            let result = (|| -> Result<()> {
                model.load_bindings(&private_dir)?;
                let (c, screen) = x11rb::connect(None)?;
                let root = c.setup().roots[screen].root;
                c.xinput_xi_query_version(2, 3)?.reply()?;
                let injected: std::collections::BTreeSet<u16> = c
                    .xinput_xi_query_device(Device::ALL)?
                    .reply()?
                    .infos
                    .iter()
                    .filter(|d| String::from_utf8_lossy(&d.name).contains("XTEST"))
                    .map(|d| d.deviceid)
                    .collect();
                c.xfixes_query_version(5, 0)?.reply()?;
                c.randr_select_input(
                    root,
                    NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE,
                )?
                .check()?;
                c.xinput_xi_select_events(
                    root,
                    &[EventMask {
                        deviceid: Device::ALL_MASTER.into(),
                        mask: vec![
                            XIEventMask::RAW_KEY_PRESS
                                | XIEventMask::RAW_KEY_RELEASE
                                | XIEventMask::RAW_MOTION
                                | XIEventMask::RAW_BUTTON_PRESS
                                | XIEventMask::RAW_BUTTON_RELEASE
                                | XIEventMask::BARRIER_HIT,
                        ],
                    }],
                )?
                .check()?;
                c.screensaver_select_input(root, 1u32.into())?;
                model.state.ready = true;
                model.state.locked = model.session_locked;
                model.state.pointer = pointer(&c, root)?.0;
                model.topology(displays(&c, root)?);
                let mut active = vec![];
                let mut screensaver_locked = false;
                loop {
                    while let Ok(command) = commands.try_recv() {
                        match command {
                            Command::Stop => return Ok(()),
                            Command::Authorize => {}
                            Command::SessionLocked(locked) => {
                                model.session_locked = locked;
                                model.state.locked = locked || screensaver_locked;
                                model.reservation = None;
                                model.publish();
                                barriers(&c, root, &model, &mut active)?;
                            }
                            Command::Configure { enabled, rules } => {
                                model.enabled = enabled;
                                model.rules = rules;
                                if !enabled {
                                    model.reservation = None;
                                }
                                barriers(&c, root, &model, &mut active)?;
                            }
                            Command::Place {
                                deadline,
                                segment,
                                fraction,
                                generation,
                                reply,
                            } => {
                                if reply.is_closed() || Instant::now() >= deadline {
                                    continue;
                                }
                                let result = (|| -> Result<u64> {
                                    let (p, buttons) = pointer(&c, root)?;
                                    ensure!(!buttons, "Destination mouse buttons are held");
                                    model.state.pointer = p;
                                    let (id, p) =
                                        model.placement(&segment, fraction, generation)?;
                                    ensure!(Instant::now() < deadline, "Cursor placement expired");
                                    warp(&c, root, p)?;
                                    Ok(id)
                                })();
                                if result.is_err() {
                                    model.reservation = None;
                                }
                                if let Err(Ok(token)) =
                                    reply.send(result.map_err(|e| e.to_string()))
                                    && let Some(previous) = model.cancel(token)
                                {
                                    let _ = warp(&c, root, previous);
                                }
                            }
                            Command::Cancel(id) => {
                                if let Some(p) = model.cancel(id) {
                                    warp(&c, root, p)?;
                                }
                            }
                            Command::Complete(id) => model.complete(id),
                        }
                    }
                    while let Some(event) = c.poll_for_event()? {
                        match event {
                            XEvent::RandrNotify(_) | XEvent::RandrScreenChangeNotify(_) => {
                                model.topology(displays(&c, root)?);
                                barriers(&c, root, &model, &mut active)?;
                            }
                            XEvent::ScreensaverNotify(e) => {
                                screensaver_locked = u8::from(e.state) != 0;
                                model.state.locked = screensaver_locked || model.session_locked;
                                model.publish();
                                barriers(&c, root, &model, &mut active)?;
                            }
                            XEvent::XinputRawMotion(e) => {
                                if e.response_type & 0x80 != 0 || injected.contains(&e.sourceid) {
                                    continue;
                                }
                                let (p, buttons) = pointer(&c, root)?;
                                let mut values = e.axisvalues_raw.iter();
                                let mut delta = Point::default();
                                for (word, bits) in e.valuator_mask.iter().enumerate() {
                                    for bit in 0..32 {
                                        if bits & (1 << bit) != 0
                                            && let Some(v) = values.next()
                                        {
                                            let value =
                                                v.integral as f64 + v.frac as f64 / 4294967296.0;
                                            match word * 32 + bit {
                                                0 => delta.x = value,
                                                1 => delta.y = value,
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                                model.movement(p, delta, buttons, false);
                            }
                            XEvent::XinputRawKeyPress(_)
                            | XEvent::XinputRawKeyRelease(_)
                            | XEvent::XinputRawButtonPress(_)
                            | XEvent::XinputRawButtonRelease(_) => {
                                model.state.input_serial = model.state.input_serial.wrapping_add(1);
                            }
                            XEvent::XinputBarrierHit(e) => {
                                let (p, buttons) = pointer(&c, root)?;
                                let delta = Point {
                                    x: e.dx.integral as f64 + e.dx.frac as f64 / 4294967296.0,
                                    y: e.dy.integral as f64 + e.dy.frac as f64 / 4294967296.0,
                                };
                                if e.response_type & 0x80 == 0 && !injected.contains(&e.sourceid) {
                                    model.movement(p, delta, buttons, false);
                                }
                                c.xinput_xi_barrier_release_pointer(&[
                                    BarrierReleasePointerInfo {
                                        deviceid: e.deviceid,
                                        barrier: e.barrier,
                                        eventid: e.eventid,
                                    },
                                ])?;
                                c.flush()?;
                            }
                            _ => {}
                        }
                    }
                    let mut fds = [
                        libc::pollfd {
                            fd: c.stream().as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: read.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                    if result < 0 {
                        let e = std::io::Error::last_os_error();
                        if e.kind() != std::io::ErrorKind::Interrupted {
                            return Err(e.into());
                        }
                    }
                    if fds[1].revents & libc::POLLIN != 0 {
                        let mut b = [0; 128];
                        while read.read(&mut b).is_ok() {}
                    }
                    ensure!(
                        fds[0].revents & (libc::POLLHUP | libc::POLLERR) == 0,
                        "X11 connection lost"
                    );
                }
            })();
            if let Err(e) = result {
                model.unavailable(format!("X11 unavailable: {e:#}"));
            }
        })?;
    Ok(Arc::new(move || {
        let _ = (&*write).write(&[1]);
    }))
}
