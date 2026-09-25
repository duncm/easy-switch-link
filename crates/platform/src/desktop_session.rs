//! logind signals, not a periodic lock-state query. User services are not always
//! inside a login session, so resolve the active graphical session for our UID.
use super::{Command, Handle};
use anyhow::{Context, Result};
use futures_util::StreamExt;
pub fn start(handle: Handle) {
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(watch(&handle))
        })();
        if let Err(e) = result {
            tracing::warn!("Desktop session watcher unavailable: {e:#}");
            // Fail closed: a user must not lose their mouse into a locked session.
            let _ = handle.send(Command::SessionLocked(true));
        }
    });
}
async fn watch(handle: &Handle) -> Result<()> {
    let connection = zbus::Connection::system().await?;
    let manager = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let sessions: Vec<(String, u32, String, String, zbus::zvariant::OwnedObjectPath)> =
        manager.call("ListSessions", &()).await?;
    let wanted = std::env::var("XDG_SESSION_ID").ok();
    let mut selected = None;
    for (id, uid, _, _, path) in sessions {
        if uid != unsafe { libc::geteuid() } {
            continue;
        }
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.login1",
            path.clone(),
            "org.freedesktop.login1.Session",
        )
        .await?;
        let kind: String = proxy.get_property("Type").await?;
        let active: bool = proxy.get_property("Active").await?;
        if (kind == "x11" || kind == "wayland") && (wanted.as_ref() == Some(&id) || active) {
            selected = Some(path);
            break;
        }
    }
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        selected.context("No active graphical login session")?,
        "org.freedesktop.login1.Session",
    )
    .await?;
    let mut locks = proxy.receive_property_changed::<bool>("LockedHint").await;
    let mut activities = proxy.receive_property_changed::<bool>("Active").await;
    let mut sleep = manager.receive_signal("PrepareForSleep").await?;
    let mut locked: bool = proxy.get_property("LockedHint").await?;
    let mut active: bool = proxy.get_property("Active").await?;
    let mut sleeping = false;
    loop {
        handle.send(Command::SessionLocked(locked || !active || sleeping))?;
        tokio::select! {
            _=handle.tx.closed()=>return Ok(()),
            value=locks.next()=>{locked=value.context("Lock notification stream closed")?.get().await?;},
            value=activities.next()=>{active=value.context("Session notification stream closed")?.get().await?;},
            value=sleep.next()=>{let message=value.context("Sleep notification stream closed")?;let (value,)=message.body().deserialize::<(bool,)>()?;sleeping=value;
                if !sleeping {locked=proxy.get_property("LockedHint").await?;active=proxy.get_property("Active").await?;}
            }
        }
    }
}
