use crate::{ipc, storage::Paths};
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use easy_switch_core::{Request, config::Config};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    name = "easy-switch-link",
    version,
    about = "Link Logitech Easy-Switch devices over Bolt or Unifying receivers"
)]
struct Args {
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Displays, peers, edge rules and pairing diagnostics.
    Edges {
        #[command(subcommand)]
        command: EdgeAction,
    },
    /// Run the monitor and tray (no configuration changes until saved in settings).
    Run {
        #[arg(long)]
        headless: bool,
        #[arg(long)]
        diagnostics: bool,
    },
    Settings,
    Devices {
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Switch a group to a configured computer name or ID.
    Switch {
        /// Computer ID (e.g. computer-2) or name (case-insensitive; quote spaces).
        computer: String,
        #[arg(long, default_value = "desk")]
        group: String,
    },
    Pause,
    Resume,
    /// Pause and restore this application's saved key-reporting changes.
    Recover,
    /// Inspect receiver access and locally identified capabilities.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    Rescan,
    Quit,
    Export {
        path: PathBuf,
    },
    Import {
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum EdgeAction {
    Status {
        #[arg(long)]
        json: bool,
    },
    Displays {
        #[arg(long)]
        json: bool,
    },
    Peers {
        #[arg(long)]
        json: bool,
    },
    Rules {
        #[arg(long)]
        json: bool,
    },
    Authorize,
    Pair {
        address: Option<String>,
    },
    Confirm {
        peer: String,
        code: String,
    },
    Revoke {
        peer: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Reverse {
        id: String,
    },
}
pub fn main() -> Result<()> {
    let args = Args::parse();
    let paths = Paths::new(args.config_dir)?;
    match args.command {
        Some(Action::Run {
            headless,
            diagnostics,
        }) => crate::monitor::run(paths, headless, diagnostics),
        None => {
            if ipc::call(&paths, Request::Status).is_ok() {
                spawn_settings(&paths)
            } else {
                crate::monitor::run(paths, false, false)
            }
        }
        Some(Action::Settings) => {
            ensure_running(&paths, false)?;
            crate::ui::run(paths)
        }
        Some(Action::Export { path }) => {
            let config = paths.load()?;
            crate::storage::atomic_write(
                &absolute(path)?,
                toml::to_string_pretty(&config)?.as_bytes(),
            )
        }
        Some(action) => {
            if !matches!(action, Action::Quit) {
                ensure_running(&paths, true)?;
            }
            let (request, json) = match action {
                Action::Devices { json } | Action::Status { json } | Action::Doctor { json } => {
                    (Request::Status, json)
                }
                Action::Edges { command } => {
                    use easy_switch_core::follow::Action as F;
                    match command {
                        EdgeAction::Status { json }
                        | EdgeAction::Displays { json }
                        | EdgeAction::Peers { json }
                        | EdgeAction::Rules { json } => (Request::Status, json),
                        EdgeAction::Authorize => (
                            Request::Follow {
                                action: F::Authorize,
                            },
                            false,
                        ),
                        EdgeAction::Pair { address } => (
                            Request::Follow {
                                action: address
                                    .map(|address| F::Connect { address })
                                    .unwrap_or(F::OpenPairing),
                            },
                            false,
                        ),
                        EdgeAction::Confirm { peer, code } => (
                            Request::Follow {
                                action: F::Confirm { peer, code },
                            },
                            false,
                        ),
                        EdgeAction::Revoke { peer } => (
                            Request::Follow {
                                action: F::Revoke { peer },
                            },
                            false,
                        ),
                        EdgeAction::Enable { id } => (
                            Request::Follow {
                                action: F::SetRule { id, enabled: true },
                            },
                            false,
                        ),
                        EdgeAction::Disable { id } => (
                            Request::Follow {
                                action: F::SetRule { id, enabled: false },
                            },
                            false,
                        ),
                        EdgeAction::Reverse { id } => (
                            Request::Follow {
                                action: F::Reverse { id },
                            },
                            false,
                        ),
                    }
                }
                Action::Switch { computer, group } => {
                    // Resolve against the running monitor's configuration, not a
                    // potentially stale file. Sending the canonical ID also lets
                    // this CLI work with monitors predating name support.
                    let response = ipc::call(&paths, Request::Status)?;
                    ensure!(response.ok, "{}", response.message);
                    let status = response
                        .status
                        .context("Monitor returned no configuration")?;
                    let computer = status.config.resolve_computer(&computer)?.to_owned();
                    (Request::Switch { computer, group }, false)
                }
                Action::Pause => (Request::Pause, false),
                Action::Resume => (Request::Resume, false),
                Action::Recover => (Request::Recover, false),
                Action::Rescan => (Request::Rescan, false),
                Action::Quit => (Request::Quit, false),
                Action::Import { path } => {
                    let config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
                    config.validate()?;
                    (Request::Save { config }, false)
                }
                _ => unreachable!(),
            };
            let mut reply = ipc::call(&paths, request.clone())?;
            if matches!(request, Request::Status) {
                let deadline = Instant::now() + Duration::from_secs(20);
                while reply.status.as_ref().is_some_and(|s| s.discovering > 0)
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(200));
                    reply = ipc::call(&paths, Request::Status)?;
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&reply)?);
            } else {
                println!("{}", reply.message);
                if matches!(request, Request::Status)
                    && let Some(status) = &reply.status
                {
                    println!(
                        "Monitoring: {} | Configured: {}",
                        if status.paused { "paused" } else { "active" },
                        status.configured
                    );
                    println!(
                        "Desktop: {} | {}",
                        status.follow.desktop.backend, status.follow.desktop.reason
                    );
                    println!("{}", status.follow.message);
                    for p in &status.follow.peers {
                        println!(
                            "Peer {}: {} [{}] {}",
                            p.name,
                            p.id,
                            p.address,
                            if p.trusted { "paired" } else { "untrusted" }
                        );
                    }
                    for p in &status.follow.pairing {
                        println!("Pairing {}: {} (confirm on both computers)", p.id, p.code);
                    }
                    for d in &status.follow.desktop.displays {
                        println!("Display {}: {} (binding {})", d.id, d.name, d.binding);
                    }
                    for (id, state) in &status.follow.rules {
                        println!("Edge {id}: {state}");
                    }
                    for (id, state) in &status.receivers {
                        println!("Receiver {id}: {state}");
                    }
                    for d in &status.devices {
                        println!(
                            "{} [{}]\n  {} | channel {}/{} | {}\n  {}",
                            d.name,
                            d.id,
                            if d.online { "connected" } else { "unavailable" },
                            d.current_host + 1,
                            d.host_count,
                            d.receiver,
                            d.capability
                        );
                    }
                    for message in status.messages.iter().rev().take(5) {
                        println!("  {message}");
                    }
                    println!("Configuration: {}", paths.config().display());
                    println!(
                        "Hardware support and destination arrival require physical validation."
                    );
                }
            }
            ensure!(reply.ok, "{}", reply.message);
            Ok(())
        }
    }
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}

pub fn child(paths: &Paths, args: &[&str]) -> Result<std::process::Child> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--config-dir")
        .arg(&paths.root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if args.first() == Some(&"run") {
        command.env("EASY_SWITCH_SETTINGS_OPENING", "1");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(command.spawn()?)
}
pub fn spawn_settings(paths: &Paths) -> Result<()> {
    child(paths, &["settings"])?;
    Ok(())
}
pub fn ensure_running(paths: &Paths, headless: bool) -> Result<()> {
    if ipc::call(paths, Request::Status).is_ok() {
        return Ok(());
    }
    let mut process = child(
        paths,
        if headless {
            &["run", "--headless"]
        } else {
            &["run"]
        },
    )?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if ipc::call(paths, Request::Status).is_ok() {
            return Ok(());
        }
        if let Some(status) = process.try_wait()? {
            anyhow::bail!(
                "Monitor exited ({status}); run `easy-switch-link run --headless` to see the error"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(anyhow::anyhow!("Monitor did not start")).context("Starting Easy-Switch Link")
}
