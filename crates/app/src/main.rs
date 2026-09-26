#![cfg_attr(windows, windows_subsystem = "windows")]

mod cli;
mod ipc;
mod monitor;
mod receiver;
#[cfg(target_os = "linux")]
mod service;
mod storage;
mod tray;
mod ui;

fn main() {
    // zbus's blocking facade shares a Tokio runtime with portal dependencies.
    // Its default is one worker per CPU, wasteful on large workstations. Our own
    // HID/peer runtimes are current-thread; one D-Bus worker is sufficient too.
    #[cfg(target_os = "linux")]
    if std::env::var_os("TOKIO_WORKER_THREADS").is_none() {
        // This is process entry, before any application threads are created.
        unsafe { std::env::set_var("TOKIO_WORKER_THREADS", "1") };
    }
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    if let Err(error) = cli::main() {
        eprintln!("Easy-Switch Link: {error:#}");
        std::process::exit(1);
    }
}

mod follow;
mod follow_ui;
mod peer_tls;
