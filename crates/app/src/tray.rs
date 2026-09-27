use crate::{ipc, storage::Paths};
use anyhow::Result;
use easy_switch_core::Request;

pub fn spawn(paths: Paths) -> Result<()> {
    #[cfg(windows)]
    return windows_tray(paths);
    #[cfg(target_os = "linux")]
    return linux_tray(paths);
}

fn action(paths: &Paths, action: &str) {
    let result = match action {
        "about" => crate::cli::child(paths, &["about"]).map(|_| ()),
        "settings" | "switch" => crate::cli::spawn_settings(paths),
        "toggle" => ipc::call(paths, Request::Status).and_then(|reply| {
            ipc::call(
                paths,
                if reply.status.is_some_and(|s| s.paused) {
                    Request::Resume
                } else {
                    Request::Pause
                },
            )
            .map(|_| ())
        }),
        "startup" => ipc::call(paths, Request::Status).and_then(|reply| {
            let mut config = reply
                .status
                .ok_or_else(|| anyhow::anyhow!("Missing status"))?
                .config;
            config.start_at_login = !config.start_at_login;
            ipc::call(paths, Request::Save { config }).map(|_| ())
        }),
        "diagnostics" => {
            let _ = ipc::call(paths, Request::Rescan);
            crate::cli::spawn_settings(paths)
        }
        "quit" => ipc::call(paths, Request::Quit).map(|_| ()),
        _ => Ok(()),
    };
    if let Err(error) = result {
        tracing::warn!("Tray action: {error:#}");
    }
}

pub fn icon_rgba() -> Vec<u8> {
    let mut data = vec![0; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let arrow = ((10..=12).contains(&y) && (6..=25).contains(&x))
                || ((19..=21).contains(&y) && (6..=25).contains(&x))
                || ((7..=15).contains(&y) && x == 25 - (y as i32 - 11).unsigned_abs() as usize)
                || ((16..=24).contains(&y) && x == 6 + (y as i32 - 20).unsigned_abs() as usize);
            data[(y * 32 + x) * 4..(y * 32 + x + 1) * 4].copy_from_slice(if arrow {
                &[98, 222, 193, 255]
            } else {
                &[21, 51, 74, 255]
            });
        }
    }
    data
}

#[cfg(windows)]
fn windows_tray(paths: Paths) -> Result<()> {
    std::thread::Builder::new()
        .name("windows-tray".into())
        .spawn(move || {
            use tray_icon::{
                Icon, TrayIconBuilder,
                menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
            };
            use windows::Win32::UI::WindowsAndMessaging::*;
            let result = (|| -> Result<()> {
                let menu = Menu::new();
                menu.append(&MenuItem::new("Easy-Switch Link", false, None))?;
                menu.append(&PredefinedMenuItem::separator())?;
                let entries = [
                    ("Settings and status...", "settings"),
                    ("Switch computer...", "switch"),
                    ("Pause / resume", "toggle"),
                    ("Toggle start at login", "startup"),
                    ("Diagnostics...", "diagnostics"),
                    ("About...", "about"),
                    ("Quit", "quit"),
                ];
                let mut actions = std::collections::HashMap::new();
                for (label, action) in entries {
                    let item = MenuItem::new(label, true, None);
                    actions.insert(item.id().clone(), action);
                    menu.append(&item)?;
                }
                MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                    if let Some(name) = actions.get(&event.id) {
                        action(&paths, name);
                    }
                }));
                let _tray = TrayIconBuilder::new()
                    .with_tooltip("Easy-Switch Link — device synchronization")
                    .with_menu(Box::new(menu))
                    .with_icon(Icon::from_rgba(icon_rgba(), 32, 32)?)
                    .build()?;
                unsafe {
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                tracing::error!(
                    "Tray unavailable: {error:#}; settings remain available from the command line"
                );
            }
        })?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_tray(paths: Paths) -> Result<()> {
    struct LinuxTray {
        paths: Paths,
    }
    impl ksni::Tray for LinuxTray {
        fn id(&self) -> String {
            "easy-switch-link".into()
        }
        fn title(&self) -> String {
            "Easy-Switch Link".into()
        }
        fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
            tracing::info!("Tray host unavailable; waiting for desktop tray support");
            true
        }
        fn watcher_online(&self) {
            tracing::info!("Desktop tray host available");
        }
        fn icon_name(&self) -> String {
            "easy-switch-link".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            let rgba = icon_rgba();
            let mut argb = Vec::with_capacity(rgba.len());
            for p in rgba.as_chunks::<4>().0 {
                argb.extend_from_slice(&[p[3], p[0], p[1], p[2]]);
            }
            vec![ksni::Icon {
                width: 32,
                height: 32,
                data: argb,
            }]
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            [
                ("Settings and status...", "settings"),
                ("Switch computer...", "switch"),
                ("Pause / resume", "toggle"),
                ("Toggle start at login", "startup"),
                ("Diagnostics...", "diagnostics"),
                ("About...", "about"),
                ("Quit", "quit"),
            ]
            .into_iter()
            .map(|(label, name)| {
                ksni::menu::StandardItem {
                    label: label.into(),
                    activate: Box::new(move |tray: &mut Self| action(&tray.paths, name)),
                    ..Default::default()
                }
                .into()
            })
            .collect()
        }
    }
    std::thread::Builder::new()
        .name("linux-tray".into())
        .spawn(move || {
            use ksni::blocking::TrayMethods;
            // GNOME's indicator extension may start after XDG autostart apps.
            // Keep the D-Bus owner-change subscription alive until it appears.
            match (LinuxTray { paths }).assume_sni_available(true).spawn() {
                Ok(_handle) => loop {
                    std::thread::park();
                },
                Err(error) => tracing::warn!(
                    "Tray unavailable: {error}; use `easy-switch-link settings` or headless mode"
                ),
            }
        })?;
    Ok(())
}
