# Easy-Switch Link

Version 0.3.0 adds optional Linux system services, passive Wayland retry
observation, and a GNOME companion for cursor hiding and opt-in login/lock-screen
edge handoffs. See [Linux system operation](docs/SYSTEM-SERVICE.md) for activation,
permissions, display bindings and current validation limits.

A Rust desktop utility that makes linked Logitech Easy-Switch devices follow a
computer change through **Logi Bolt or Unifying receivers**. Windows has a native
tray icon; Linux supports StatusNotifierItem trays and a headless user service.
The settings window runs in a separate process and releases its graphics resources
when closed. The monitor waits for HID and OS events; it does not poll idle devices.

## Destination follows mouse

Version 0.2.3 recovers idle followers missed during rapid edge handoffs and
automatically verifies layout changes for known displays on paired computers.

Version 0.2.2 adds a small outward-push edge buffer and reduces the bounce-back
cooldown to 80 ms, with no inward-movement requirement.

Version 0.2.1 hides the source cursor after mouse-edge handoffs on Windows/X11,
restores it on local physical input or return, and adds an About screen with the
application version (Settings, tray, or `easy-switch-link about`).

Version 0.2.0 introduced opt-in display-edge switching with authenticated LAN cursor
placement. Configure directed full or partial edge rules, pair both computers,
and select a linked group in settings. See the [setup, permissions and CLI guide](docs/DESTINATION-FOLLOWS-MOUSE.md).
Normal receiver-based forwarding still works with this feature disabled.

## Install and configure

Download the appropriate artifact from `dist/` or the project's release artifacts:

| Platform | Portable | Installer |
| --- | --- | --- |
| Windows 10/11 x64 | `easy-switch-link-0.3.0-windows-x64.exe` | `easy-switch-link-0.3.0-windows-x64.msi` |
| Linux amd64 | `easy-switch-link-0.3.0-linux-amd64` | `easy-switch-link_0.3.0_amd64.deb` |
| Linux arm64 | `easy-switch-link-0.3.0-linux-arm64` | `easy-switch-link_0.3.0_arm64.deb` |

1. Pair your devices to each computer using Logitech's normal pairing procedure.
   Each participating computer needs a compatible receiver and this application.
2. Launch Easy-Switch Link. First-run settings list awake, compatible devices.
3. Name your computers, select a link group for each device, and assign the correct
   channel for each computer **on each device**. Matching channel numbers are not
   required. Choose **Not paired** for unused destinations.
4. Save. Export/import the configuration to other computers and verify the locally
   discovered device identities before saving there.
5. Switch a device that exposes host-switch events. Reachable members of its group
   receive the corresponding host-switch command.

Start at login is opt-in. Closing settings leaves the monitor running. Choose
**Quit** from the tray to stop and restore reporting in user-only mode. Linux
system services are optional and remain disabled until explicitly enabled.
Windows artifacts are unsigned unless signed by
the distributor; no certificate is included in this repository.

Before uninstalling, turn off **Start at login** and choose **Quit** so reporting
can be restored. User configuration, device caches and recovery journals are retained.

### Linux

Debian 12+ and Ubuntu 22.04+ are the compatibility baseline. Install the `.deb`
using `sudo apt install ./easy-switch-link_0.3.0_amd64.deb` (or the arm64 package).
The package installs receiver permissions, a desktop launcher, icon and optional
systemd user service. Reconnect the receiver if access is denied after installation.

For the standalone executable, make it executable and install standard libraries:

```sh
chmod +x easy-switch-link-0.3.0-linux-amd64
sudo apt install libudev1 libxkbcommon0 libxkbcommon-x11-0 libegl1 libgl1 libx11-6 libxi6 libxrandr2 libxcursor1 libwayland-client0 libwayland-cursor0 libwayland-egl1
sudo install -m 644 packaging/linux/70-easy-switch-link.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
# Reconnect the receiver, then run the executable as your normal user.
```

The udev rule grants access to the active local login session, not all users.
Headless mode does not require a display. For a headless process outside an active
local login session, an administrator must explicitly arrange hidraw permissions.
Some desktops (including stock GNOME configurations) require a tray extension;
`easy-switch-link settings` remains available without one.

To use the user service, disable the tray's start-at-login option and run:

```sh
systemctl --user enable --now easy-switch-link.service
# Stop before uninstalling:
systemctl --user disable --now easy-switch-link.service
```

## What can initiate a switch?

Support is capability-based, not a blanket promise for every Easy-Switch model.
The program discovers `ChangeHost` (HID++ feature `0x1814`) and dynamically resolves
its feature index. Supported trigger paths are:

- Explicit host-switch announcements, including the observed MX Keys S format.
- Passive Easy-Switch key analytics, when advertised by the device.
- Temporary diversion of the three Easy-Switch controls when the device advertises
  diversion but no passive key analytics, or when explicitly selected in settings.

The MX Keys S can initiate through host announcements; passive key analytics is
enabled only when the extended capability flag is advertised. The MX Master 3S supports receiving switch commands;
its underside button is not assumed to produce a usable event. See
[validation results](docs/VALIDATION.md) for the distinction between discovery,
simulated tests, and actual switching tests.

A device can always be marked **Follow only**. Temporary interception does not use
persistent diversion. Pause before pairing devices. On clean exit or pause, the
application restores its reporting changes. If it crashes, restart it or use
`recover`; unreachable devices retain recovery records until they return. A device
power cycle also clears temporary diversion.

**Disconnection is never treated as a switch.** Sleep, receiver removal and startup
do not move devices. A successful command write means the request was sent, not
that the destination received the device. An unavailable follower remains behind;
there is no network channel for retrieving it. Manually align devices when needed.
Bluetooth-direct connections remain unsupported. Optional Linux pre-login
operation is described in [Linux system operation](docs/SYSTEM-SERVICE.md).

Other configuration software can coexist with the HID reader, but overlapping
Easy-Switch rules can conflict. The application checks the actual reporting state
after another settings write and pauses only for a confirmed incompatible change
to an owned control. Harmless writes to unrelated reporting bits do not pause it.
Disable conflicting Options+, Flow, Enhanced Easy-Switch, OpenLogi host-switch
targets or Solaar rules before resuming. Other mouse/keyboard settings can remain enabled. Do not run multiple
synchronization tools against the same devices.

If switching works once and then stops, check `status --json` for a pause reason.
Version 0.1.1 fixes false conflict detection after reconnects in 0.1.0. Quit the
old monitor before upgrading and launch the new version on each computer; existing
channel mappings and groups are preserved.

### Switching latency

Version 0.1.2 prioritizes switch commands while discovery or reporting queries
are waiting for replies. A slow query's two-second deadline no longer holds up a
queued switch. This uses the same event-driven receiver thread, with no idle polling.
Status messages include notification-to-HID-write time; structured logs additionally
record `event_to_write_us` and `hid_write_us` for each dispatched switch. These
measure dispatch, not radio reconnection or the time until the pointer moves.

Networking is not required to dispatch locally connected followers promptly.
Even with immediate dispatch, device firmware and the destination's radio/input
stack determine when the mouse becomes usable. Sending a coordination message
over a LAN does not bypass that reconnection. Keeping the mouse connected to one
computer and forwarding its input over a LAN could avoid the mouse's physical
handoff, but would be a separate input-sharing feature. The source computer would
need to remain awake, with additional security and input-ownership requirements.

## Command line

```text
easy-switch-link                         # tray and first-run settings
easy-switch-link run --headless          # foreground monitor, no tray/window
easy-switch-link run --diagnostics       # opt-in detailed management diagnostics
easy-switch-link settings
easy-switch-link devices --json
easy-switch-link status --json
easy-switch-link switch computer-2 --group desk
easy-switch-link switch test-bravo            # configured computer name
easy-switch-link switch "Office PC"      # quote names containing spaces
easy-switch-link pause
easy-switch-link resume
easy-switch-link doctor --json
easy-switch-link rescan
easy-switch-link recover                 # restore reporting; remain paused
easy-switch-link export /absolute/path/config.toml
easy-switch-link import /absolute/path/config.toml
easy-switch-link quit
```

`switch` accepts an exact computer ID or a case-insensitive configured name.
Exact IDs take precedence over names. Duplicate names are rejected with the IDs
you can use instead; partial names are not matched. Name resolution uses the
running monitor's configuration, so an updated CLI also works with older monitors.

Status/device/doctor commands start a headless monitor if necessary. `--config-dir`
overrides the configuration directory. One monitor runs per Windows user; Linux
uses a locked configuration directory and a private Unix socket. IPC uses bounded
messages, restricted access, connection limits and deadlines.

Configuration defaults to `%APPDATA%\easy-switch-link\config` on Windows and
`$XDG_CONFIG_HOME/easy-switch-link` (normally `~/.config/easy-switch-link`) on Linux.
`doctor` prints the actual path. Configuration is versioned TOML, saved atomically.
Logs rotate daily with five files retained. Normal logs contain management events;
diagnostic report logging records headers without keyboard/mouse input payloads.

## Build

The workspace contains `easy-switch-core` (protocol/configuration/state machine),
`easy-switch-platform` (Windows overlapped HID and Linux hidraw/udev), and
`easy-switch-link` (monitor, local IPC, tray, CLI and settings).

The Rust toolchain and dependency lockfile are committed. Build scripts package
the same executable used for portable distribution.

```sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Windows needs Visual Studio C++ build tools and a Windows SDK:

```powershell
dotnet tool install wix --version 6.0.2 --tool-path target/tools
$env:PATH = "$PWD/target/tools;$env:PATH"
./scripts/build-windows.ps1
```

Linux native builds need a C compiler, pkg-config, libudev and desktop development
libraries; the CI workflow lists the exact packages. Build on Ubuntu 22.04 (or an
equivalent container) to preserve the glibc baseline:

```sh
cargo install cargo-deb --version 3.8.0 --locked
bash scripts/build-linux.sh x86_64-unknown-linux-gnu
# On a native ARM64 runner:
bash scripts/build-linux.sh aarch64-unknown-linux-gnu
```

Each script writes release artifacts and SHA-256 checksums to `dist/`. CI builds
both Linux architectures on native runners and Windows x64, checks packages and
retains downloadable artifacts. It does not claim hardware validation.

## Protocol references

This is an original Rust implementation. References used to understand wire
formats and device behavior:

- [Logitech HID++ public documentation](https://github.com/Logitech/cpg-docs)
- [Logitech HID++ 1.0 receiver specification](https://lekensteyn.nl/files/logitech/logitech_hidpp10_specification_for_Unifying_Receivers.pdf)
- [Solaar receiver and feature implementations](https://github.com/pwr-Solaar/Solaar)
- [CleverSwitch host announcement handling](https://github.com/MikalaiBarysevich/CleverSwitch/blob/main/src/cleverswitch/subscriber/change_host_notification_subscriber.py)
- [Windows HID I/O](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/obtaining-hid-reports)
- [Linux hidraw API](https://www.kernel.org/doc/html/latest/hid/hidraw.html)

MIT licensed. Logitech, Logi Bolt and Unifying are their respective owners' marks;
this project is independent of Logitech.
