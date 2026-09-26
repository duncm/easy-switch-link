//! Linux system policy, local peer authentication and passive receiver activity.
use anyhow::{Context, Result, ensure};
use easy_switch_core::service::{POLICY, Policy};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

pub fn policy() -> Result<Option<Policy>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(POLICY)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0,
        "System policy must be a root-owned, non-writable regular file"
    );
    ensure!(meta.len() <= 16384, "System policy too large");
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 16384, "System policy grew beyond size limit");
    let p: Policy = serde_json::from_slice(&bytes)?;
    p.validate()?;
    Ok(Some(p))
}
pub fn monotonic_ns() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t);
    }
    (t.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(t.tv_nsec as u64)
}
pub fn credentials(fd: std::os::fd::RawFd) -> Result<(u32, u32)> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    ensure!(
        unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } == 0,
        "Cannot authenticate local socket"
    );
    ensure!(cred.pid > 0, "Invalid peer PID");
    Ok((cred.uid, cred.pid as u32))
}
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub uid: u32,
    pub locked: bool,
    pub greeter: bool,
    pub desktop_type: String,
}
pub fn active_session(p: &Policy) -> Result<Session> {
    let bus = zbus::blocking::Connection::system()?;
    let manager = zbus::blocking::Proxy::new(
        &bus,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    ensure!(
        !manager.get_property::<bool>("PreparingForSleep")?,
        "System is suspending"
    );
    let seat: zbus::zvariant::OwnedObjectPath = manager.call("GetSeat", &(&p.seat,))?;
    let seat = zbus::blocking::Proxy::new(
        &bus,
        "org.freedesktop.login1",
        seat,
        "org.freedesktop.login1.Seat",
    )?;
    let (id, path): (String, zbus::zvariant::OwnedObjectPath) =
        seat.get_property("ActiveSession")?;
    ensure!(!id.is_empty(), "No active session");
    let session = zbus::blocking::Proxy::new(
        &bus,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
    )?;
    let (uid, _): (u32, zbus::zvariant::OwnedObjectPath) = session.get_property("User")?;
    let kind: String = session.get_property("Type")?;
    let class: String = session.get_property("Class")?;
    ensure!(
        kind == "wayland" || kind == "x11",
        "Active session is not graphical"
    );
    let greeter = class == "greeter";
    let locked = session.get_property("LockedHint")?;
    ensure!(
        (!greeter && uid == p.owner_uid) || (p.login_handoffs && greeter && uid == p.greeter_uid),
        "Active session is not authorized"
    );
    Ok(Session {
        id,
        uid,
        locked,
        greeter,
        desktop_type: kind,
    })
}
pub fn authorize_desktop(p: &Policy, uid: u32, pid: u32) -> Result<Session> {
    let s = active_session(p)?;
    ensure!(s.uid == uid, "Desktop peer is not the active user");
    let bus = zbus::blocking::Connection::system()?;
    let manager = zbus::blocking::Proxy::new(
        &bus,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    let path: Result<zbus::zvariant::OwnedObjectPath, _> = manager.call("GetSessionByPID", &(pid,));
    let id: String = match path {
        Ok(path) => zbus::blocking::Proxy::new(
            &bus,
            "org.freedesktop.login1",
            path,
            "org.freedesktop.login1.Session",
        )?
        .get_property("Id")?,
        Err(_) => {
            // Modern GNOME Shell runs in the user's systemd manager rather than
            // session-N.scope. Require its exact Shell unit and logind's chosen
            // graphical session; an arbitrary user service is not a controller.
            let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
            ensure!(
                shell_user_unit(&cgroup, uid, &s.desktop_type),
                "Desktop peer has no attributable GNOME session"
            );
            let user: zbus::zvariant::OwnedObjectPath = manager.call("GetUserByPID", &(pid,))?;
            let proxy = zbus::blocking::Proxy::new(
                &bus,
                "org.freedesktop.login1",
                user,
                "org.freedesktop.login1.User",
            )?;
            ensure!(
                proxy.get_property::<u32>("UID")? == uid,
                "User manager identity changed"
            );
            let (display, _): (String, zbus::zvariant::OwnedObjectPath) =
                proxy.get_property("Display")?;
            display
        }
    };
    ensure!(id == s.id, "Desktop peer belongs to an inactive session");
    ensure!(
        !s.locked || p.login_handoffs,
        "Locked-session handoffs disabled"
    );
    Ok(s)
}
fn shell_user_unit(cgroup: &str, uid: u32, desktop_type: &str) -> bool {
    if !matches!(desktop_type, "x11" | "wayland") {
        return false;
    }
    let expected = format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/session.slice/org.gnome.Shell@{desktop_type}.service"
    );
    cgroup.lines().any(|line| {
        line.splitn(3, ':')
            .nth(2)
            .is_some_and(|path| path == expected)
    })
}
/// Any logind transition invalidates queued operations, without a timer.
pub fn watch_sessions(changed: impl Fn() + Send + 'static) -> Result<()> {
    let bus = zbus::blocking::Connection::system()?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.login1")?
        .build();
    let iter = zbus::blocking::MessageIterator::for_match_rule(rule, &bus, Some(32))?;
    std::thread::Builder::new()
        .name("service-sessions".into())
        .spawn(move || {
            for message in iter {
                let Ok(message) = message else {
                    break;
                };
                let header = message.header();
                let member = header.member().map(|s| s.as_str()).unwrap_or("");
                if matches!(member, "PrepareForSleep" | "SessionNew" | "SessionRemoved") {
                    changed();
                } else if member == "PropertiesChanged" {
                    type Properties = (
                        String,
                        std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                        Vec<String>,
                    );
                    if let Ok((_, values, invalidated)) = message.body().deserialize::<Properties>()
                        && ["ActiveSession", "Active", "LockedHint"].iter().any(|key| {
                            values.contains_key(*key) || invalidated.iter().any(|s| s == key)
                        })
                    {
                        changed();
                    }
                }
            }
            changed();
        })?;
    Ok(())
}

#[derive(Default, Clone)]
struct Counts {
    keyboard: u64,
    pointer: u64,
    covered: bool,
}
#[derive(Default)]
struct Observation {
    generation: u64,
    receivers: BTreeMap<String, Counts>,
    reason: String,
}
struct Node {
    file: File,
    receiver: String,
    keyboard: bool,
    pointer: bool,
}
#[derive(Clone)]
pub struct Activity {
    state: Arc<Mutex<Observation>>,
    tx: crossbeam_channel::Sender<crossbeam_channel::Sender<()>>,
    wake: Arc<Mutex<UnixStream>>,
}
impl Activity {
    pub fn start() -> Result<Self> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        let (tx, rx) = crossbeam_channel::bounded::<crossbeam_channel::Sender<()>>(8);
        let state = Arc::new(Mutex::new(Observation::default()));
        let shared = state.clone();
        std::thread::Builder::new()
            .name("receiver-activity".into())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let monitor = udev::MonitorBuilder::new()?
                        .match_subsystem("input")?
                        .listen()?;
                    let mut wake = read;
                    let mut nodes = scan(&shared)?;
                    loop {
                        let mut fds = vec![
                            libc::pollfd {
                                fd: wake.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                            libc::pollfd {
                                fd: monitor.as_raw_fd(),
                                events: libc::POLLIN,
                                revents: 0,
                            },
                        ];
                        fds.extend(nodes.iter().map(|n| libc::pollfd {
                            fd: n.file.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        }));
                        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
                        if n < 0 {
                            if std::io::Error::last_os_error().kind()
                                == std::io::ErrorKind::Interrupted
                            {
                                continue;
                            }
                            anyhow::bail!("Input readiness failed");
                        }
                        if fds[0].revents & libc::POLLHUP != 0 {
                            return Ok(());
                        }
                        // Drain every node before acknowledging a verification fence.
                        for node in &mut nodes {
                            drain(node, &shared)?;
                        }
                        if fds[1].revents != 0
                            || fds
                                .iter()
                                .skip(2)
                                .any(|f| f.revents & (libc::POLLHUP | libc::POLLERR) != 0)
                        {
                            for _ in monitor.iter() {}
                            nodes = scan(&shared)?;
                        }
                        if fds[0].revents != 0 {
                            let mut bytes = [0; 64];
                            while wake.read(&mut bytes).is_ok() {}
                        }
                        for reply in rx.try_iter() {
                            let _ = reply.try_send(());
                        }
                    }
                })();
                let mut s = shared.lock().unwrap();
                s.generation += 1;
                s.receivers.clear();
                s.reason = format!("Activity observer stopped: {result:?}");
            })?;
        Ok(Self {
            state,
            tx,
            wake: Arc::new(Mutex::new(write)),
        })
    }
    pub fn description(&self) -> String {
        let s = self.state.lock().unwrap();
        if s.receivers.is_empty() {
            s.reason.clone()
        } else {
            format!(
                "Passive receiver activity: {} covered receivers",
                s.receivers.values().filter(|v| v.covered).count()
            )
        }
    }
    fn fence(&self) -> bool {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.tx.try_send(tx).is_err() {
            return false;
        }
        let _ = self.wake.lock().unwrap().write(&[1]);
        rx.recv_timeout(Duration::from_millis(20)).is_ok()
    }
    pub fn guard(&self, receiver: &str, kind: &str) -> Option<Arc<dyn Fn() -> bool + Send + Sync>> {
        if !self.fence() {
            return None;
        }
        let s = self.state.lock().unwrap();
        let c = s.receivers.get(receiver)?.clone();
        if !c.covered {
            return None;
        }
        let generation = s.generation;
        drop(s);
        let this = self.clone();
        let receiver = receiver.to_owned();
        let keyboard = kind != "mouse";
        let pointer = kind != "keyboard";
        Some(Arc::new(move || {
            if !this.fence() {
                return false;
            }
            let s = this.state.lock().unwrap();
            s.generation == generation
                && s.receivers.get(&receiver).is_some_and(|now| {
                    now.covered
                        && (!keyboard || now.keyboard == c.keyboard)
                        && (!pointer || now.pointer == c.pointer)
                })
        }))
    }
}
fn scan(state: &Mutex<Observation>) -> Result<Vec<Node>> {
    let mut s = state.lock().unwrap();
    s.generation += 1;
    s.receivers.clear();
    s.reason = "No accessible receiver input nodes".into();
    let mut e = udev::Enumerator::new()?;
    e.match_subsystem("input")?;
    let mut nodes = vec![];
    for d in e.scan_devices()? {
        let Some(path) = d.devnode().filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("event"))
        }) else {
            continue;
        };
        let Some(usb) = d.parent_with_subsystem_devtype("usb", "usb_device")? else {
            continue;
        };
        if usb.attribute_value("idVendor").and_then(|s| s.to_str()) != Some("046d") {
            continue;
        }
        let pid = usb
            .attribute_value("idProduct")
            .and_then(|s| s.to_str())
            .and_then(|s| u16::from_str_radix(s, 16).ok());
        if !pid.is_some_and(|p| crate::RECEIVER_PIDS.contains(&p)) {
            continue;
        }
        let receiver = usb.syspath().to_string_lossy().into_owned();
        let counts = s.receivers.entry(receiver.clone()).or_insert(Counts {
            covered: true,
            ..Default::default()
        });
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => {
                let k = d.property_value("ID_INPUT_KEYBOARD").is_some();
                let p = d.property_value("ID_INPUT_MOUSE").is_some()
                    || d.property_value("ID_INPUT_TOUCHPAD").is_some();
                nodes.push(Node {
                    file,
                    receiver,
                    keyboard: k || !p,
                    pointer: p || !k,
                });
            }
            Err(_) => counts.covered = false,
        }
    }
    drop(s);
    for node in &mut nodes {
        drain(node, state)?;
    }
    Ok(nodes)
}
#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}
fn drain(node: &mut Node, state: &Mutex<Observation>) -> Result<()> {
    let size = std::mem::size_of::<InputEvent>();
    let mut bytes = vec![0u8; size * 64];
    // Bounded work: saturation loses coverage rather than delaying a HID write.
    for _ in 0..64 {
        let n = match node.file.read(&mut bytes) {
            Ok(0) => {
                state
                    .lock()
                    .unwrap()
                    .receivers
                    .get_mut(&node.receiver)
                    .unwrap()
                    .covered = false;
                return Ok(());
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(_) => {
                state
                    .lock()
                    .unwrap()
                    .receivers
                    .get_mut(&node.receiver)
                    .unwrap()
                    .covered = false;
                return Ok(());
            }
        };
        let mut s = state.lock().unwrap();
        let c = s
            .receivers
            .get_mut(&node.receiver)
            .context("Receiver observation disappeared")?;
        if n % size != 0 {
            c.covered = false;
            return Ok(());
        }
        for chunk in bytes[..n].chunks_exact(size) {
            let event = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast::<InputEvent>()) };
            if event.kind == 0 && event.code == 3 {
                c.covered = false;
            }
            if matches!(event.kind, 1..=3) {
                if node.keyboard {
                    c.keyboard = c.keyboard.wrapping_add(1);
                }
                if node.pointer {
                    c.pointer = c.pointer.wrapping_add(1);
                }
            }
        }
    }
    state
        .lock()
        .unwrap()
        .receivers
        .get_mut(&node.receiver)
        .unwrap()
        .covered = false;
    Ok(())
}

pub fn invalidate_epoch(epoch: &AtomicU64) {
    epoch.fetch_add(1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, IntoRawFd};
    #[test]
    fn only_matching_gnome_user_unit_can_use_display_session_fallback() {
        let group = "0::/user.slice/user-1000.slice/user@1000.service/session.slice/org.gnome.Shell@wayland.service\n";
        assert!(shell_user_unit(group, 1000, "wayland"));
        assert!(!shell_user_unit(group, 1001, "wayland"));
        assert!(!shell_user_unit(group, 1000, "x11"));
        assert!(!shell_user_unit(
            &group.replace("org.gnome.Shell", "other"),
            1000,
            "wayland"
        ));
        assert!(!shell_user_unit(
            &group.replace(".service\n", ".service/child\n"),
            1000,
            "wayland"
        ));
    }
    #[test]
    fn receiver_activity_is_classified_and_overflow_invalidates_coverage() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut node = Node {
            file: unsafe { File::from_raw_fd(reader.into_raw_fd()) },
            receiver: "bolt".into(),
            keyboard: true,
            pointer: false,
        };
        let state = Mutex::new(Observation {
            receivers: BTreeMap::from([(
                "bolt".into(),
                Counts {
                    covered: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        let mut event = InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            kind: 1,
            code: 30,
            value: 1,
        };
        fn send(writer: &mut UnixStream, event: &InputEvent) {
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    (event as *const InputEvent).cast::<u8>(),
                    std::mem::size_of::<InputEvent>(),
                )
            };
            writer.write_all(bytes).unwrap();
        }
        send(&mut writer, &event);
        drain(&mut node, &state).unwrap();
        {
            let s = state.lock().unwrap();
            let c = &s.receivers["bolt"];
            assert_eq!(c.keyboard, 1);
            assert_eq!(c.pointer, 0);
            assert!(c.covered);
        }
        node.keyboard = false;
        node.pointer = true;
        event.kind = 2;
        event.code = 0;
        event.value = 4;
        send(&mut writer, &event);
        drain(&mut node, &state).unwrap();
        assert_eq!(state.lock().unwrap().receivers["bolt"].pointer, 1);
        event.kind = 0;
        event.code = 3;
        event.value = 0;
        send(&mut writer, &event);
        drain(&mut node, &state).unwrap();
        assert!(!state.lock().unwrap().receivers["bolt"].covered);
    }
    #[test]
    fn input_disconnect_and_truncated_event_fail_closed() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut node = Node {
            file: unsafe { File::from_raw_fd(reader.into_raw_fd()) },
            receiver: "bolt".into(),
            keyboard: true,
            pointer: true,
        };
        let state = Mutex::new(Observation {
            receivers: BTreeMap::from([(
                "bolt".into(),
                Counts {
                    covered: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        });
        writer.write_all(&[1, 2, 3]).unwrap();
        drain(&mut node, &state).unwrap();
        assert!(!state.lock().unwrap().receivers["bolt"].covered);
        state
            .lock()
            .unwrap()
            .receivers
            .get_mut("bolt")
            .unwrap()
            .covered = true;
        drop(writer);
        drain(&mut node, &state).unwrap();
        assert!(!state.lock().unwrap().receivers["bolt"].covered);
    }
}
