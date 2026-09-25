use super::*;
use anyhow::{Context, ensure};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, net::UnixStream},
    },
    path::Path,
};

pub fn discover() -> Result<Vec<ReceiverInfo>> {
    if !Path::new("/sys/class/hidraw").is_dir() {
        return Ok(Vec::new());
    }
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("hidraw")?;
    let mut found = Vec::new();
    for device in enumerator.scan_devices()? {
        let Some(usb) = device.parent_with_subsystem_devtype("usb", "usb_device")? else {
            continue;
        };
        let value = |key| {
            usb.attribute_value(key)
                .and_then(|v| v.to_str())
                .and_then(|v| u16::from_str_radix(v, 16).ok())
        };
        let Some(pid) = value("idProduct").filter(|p| RECEIVER_PIDS.contains(p)) else {
            continue;
        };
        if value("idVendor") != Some(0x046d) {
            continue;
        }
        let Some(node) = device.devnode() else {
            continue;
        };
        let Some(hid) = device.parent_with_subsystem("hid")? else {
            continue;
        };
        let descriptor = std::fs::read(hid.syspath().join("report_descriptor")).unwrap_or_default();
        // Vendor page FF00 plus HID++ long report ID, not normal keyboard/mouse input.
        if !descriptor.windows(3).any(|v| v == [0x06, 0x00, 0xff])
            || !descriptor.windows(2).any(|v| v == [0x85, 0x11])
        {
            continue;
        }
        // Exclude kernel-created child devices: address the receiver once using its physical interface.
        if hid.parent_with_subsystem("hid")?.is_some() {
            continue;
        }
        found.push(ReceiverInfo {
            id: usb.syspath().to_string_lossy().into(),
            product_id: pid,
            endpoints: vec![Endpoint {
                path: node.to_string_lossy().into(),
                input_len: 64,
                output_len: 20,
            }],
        });
    }
    Ok(found)
}

struct LinuxTransport {
    file: File,
    wake_read: UnixStream,
    wake_write: Arc<UnixStream>,
}
pub fn open(info: &ReceiverInfo) -> Result<Box<dyn Transport>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&info.endpoints[0].path)
        .context("Cannot open hidraw; install the supplied udev rule and reconnect the receiver")?;
    let (read, write) = UnixStream::pair()?;
    read.set_nonblocking(true)?;
    write.set_nonblocking(true)?;
    Ok(Box::new(LinuxTransport {
        file,
        wake_read: read,
        wake_write: Arc::new(write),
    }))
}
impl Transport for LinuxTransport {
    fn waker(&self) -> Wake {
        let stream = self.wake_write.clone();
        Arc::new(move || {
            let _ = (&*stream).write(&[1]);
        })
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.file.write(bytes)? == bytes.len(),
            "Incomplete HID write"
        );
        Ok(())
    }
    fn read(&mut self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>> {
        let mut fds = [
            libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.wake_read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ms = timeout
            .map(|d| d.as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(-1);
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, ms) };
        if result < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(e.into());
        }
        ensure!(
            fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0,
            "Receiver disconnected"
        );
        if fds[1].revents & libc::POLLIN != 0 {
            let mut bytes = [0; 128];
            while self.wake_read.read(&mut bytes).is_ok() {}
            return Ok(None);
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let mut bytes = [0; 64];
            let n = self.file.read(&mut bytes)?;
            ensure!(n > 0, "Receiver disconnected");
            return Ok(Some(bytes[..n].to_vec()));
        }
        Ok(None)
    }
}

pub fn watch_changes(tx: crossbeam_channel::Sender<()>) -> Result<()> {
    let device_tx = tx.clone();
    std::thread::Builder::new()
        .name("udev-events".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let monitor = udev::MonitorBuilder::new()?
                    .match_subsystem("hidraw")?
                    .listen()?;
                loop {
                    let mut fd = libc::pollfd {
                        fd: monitor.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    if unsafe { libc::poll(&mut fd, 1, -1) } > 0 {
                        for _ in monitor.iter() {
                            let _ = device_tx.try_send(());
                        }
                    }
                }
            })();
            if let Err(e) = result {
                tracing::warn!("udev watcher: {e:#}");
            }
        })?;
    std::thread::Builder::new()
        .name("resume-events".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                let conn = zbus::blocking::Connection::system()?;
                let proxy = zbus::blocking::Proxy::new(
                    &conn,
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                )?;
                for message in proxy.receive_signal("PrepareForSleep")? {
                    if let Ok((sleeping,)) = message.body().deserialize::<(bool,)>()
                        && !sleeping
                    {
                        let _ = tx.try_send(());
                    }
                }
                Ok(())
            })();
            if let Err(e) = result {
                tracing::warn!("Resume watcher unavailable: {e:#}");
            }
        })?;
    Ok(())
}
pub fn user_id() -> Result<String> {
    Ok(unsafe { libc::geteuid() }.to_string())
}
pub fn set_autostart(enabled: bool, executable: &Path) -> Result<()> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    let dir = base.join("autostart");
    let file = dir.join("easy-switch-link.desktop");
    if enabled {
        std::fs::create_dir_all(&dir)?;
        let path = executable
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%");
        ensure!(!path.contains(['\n', '\r']), "Invalid executable path");
        std::fs::write(
            file,
            format!(
                "[Desktop Entry]\nType=Application\nName=Easy-Switch Link\nExec=\"{path}\" run\nTerminal=false\n"
            ),
        )?;
    } else if file.exists() {
        std::fs::remove_file(file)?;
    }
    Ok(())
}
