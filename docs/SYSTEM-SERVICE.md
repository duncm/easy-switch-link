# Linux system operation (0.3.3)

System operation is optional. Installing the package does not enable it or change
the login screen. The ordinary user-session monitor remains the default.

## Enable and disable

First save a working configuration as the intended desktop user. Install the
0.3.3 `.deb`, then choose one of these administrative operations:

```sh
# Hardware synchronization before login, with session-only LAN coordination:
sudo easy-switch-link service enable --user "$USER"

# Alternatively, also enable GNOME/GDM login and lock-screen edge handoffs:
sudo easy-switch-link service enable --user "$USER" --login-handoffs
```

The helper stops the previous monitor, waits for reporting cleanup, preserves
configuration and startup backups, installs receiver permissions, and starts the
system services. Existing pairing identities are retained. Enabling login-screen
networking moves the private pairing state into the peer service's protected
directory. Configuration exports never contain that private state.

Start Easy-Switch Link again in the user session. A newly installed GNOME
extension may require logout/login before Shell recognizes it. GDM loads its
configuration when a new greeter session starts; the installer never restarts
the display manager or logs out the user automatically.

```sh
easy-switch-link service status --json
easy-switch-link status --json
sudo easy-switch-link service disable
```

Disabling restores the latest configuration and pairing state to the designated
user and restores the previous startup files. Restart the user monitor afterward.
Changing the designated owner or login policy requires disabling and re-enabling
system mode. Currently one owner and `seat0` are supported.

The session's Quit action closes the session companion; it does not disable the
hardware service. With login-screen handoffs enabled, the machine peer coordinator
also remains running. Pause controls hardware forwarding for the running service;
restarting the service resumes its saved configuration.

## GNOME companion

The package installs `easy-switch-link@duncm.github.com` system-wide. It supports
the modern extension format for GNOME 45â€“51 with runtime API checks; supported
metadata is not a claim of hardware validation for every Shell release.

To use the companion without login-screen networking:

```sh
easy-switch-link service companion enable
# Restart the user monitor after changing its desktop backend.
easy-switch-link service companion status
easy-switch-link service companion disable
```

Standalone users can install the release's `.shell-extension.zip` with
`gnome-extensions install`, then enable the companion. System-service mode still
requires installing its system units, accounts, helper and permission setup;
simply running the standalone executable as root is not equivalent.

For a manual system installation, use the release executable matching the host
architecture and the matching 0.3.3 source checkout. On a systemd/udev distribution
with Python 3 installed, run these from the checkout (replace the executable path):

```sh
sudo install -m 755 /path/to/easy-switch-link-0.3.3-linux-arm64 /usr/bin/easy-switch-link
sudo install -d /usr/lib/easy-switch-link /usr/lib/sysusers.d /usr/lib/udev/rules.d
sudo install -m 755 packaging/linux/service-admin /usr/lib/easy-switch-link/service-admin
sudo install -m 644 packaging/linux/easy-switch-link.sysusers /usr/lib/sysusers.d/easy-switch-link.conf
sudo install -m 644 packaging/linux/easy-switch-link-hardware.service packaging/linux/easy-switch-link-peer.service /usr/lib/systemd/system/
sudo install -m 644 packaging/linux/70-easy-switch-link.rules /usr/lib/udev/rules.d/
sudo install -d /usr/share/gnome-shell/extensions/easy-switch-link@duncm.github.com
sudo install -m 644 packaging/gnome/easy-switch-link@duncm.github.com/* /usr/share/gnome-shell/extensions/easy-switch-link@duncm.github.com/
sudo systemd-sysusers /usr/lib/sysusers.d/easy-switch-link.conf
sudo systemctl daemon-reload
```

Then use the explicit enable command above. GNOME/GDM integration additionally
requires GDM, `gnome-shell`, `gnome-extensions` and `dconf`; it never installs or
replaces the display manager. Disable system mode before removing a manual
installation. Disabling preserves protected service state as a recovery backup.

The companion handles display geometry, short pointer barriers, placement and
cursor visibility. The 12-unit edge buffer remains in the Rust core; barriers are
released after activation or at most 200 ms. Input is not forwarded between
computers. The cursor hides only after a successful mouse command for an edge
handoff. Manual switching never hides it. Local activity, arrival, pause,
controller loss and desktop transitions restore visibility.

On GNOME Wayland, 0.3.3 permits motion from the departing mouse's receiver for
250 ms after its successful edge-switch write. This lets the mouse's final radio
reports finish without immediately revealing the source cursor. Bolt may merge
multiple paired mice into one input node: their motion shares this brief exception.
Movement during the window is ignored for cursor restoration; movement after it,
or any click/key/scroll, restores visibility. Other receivers, unknown devices
and virtual inputs such as Sunshine restore immediately. Input still reaches
applications normally; no events are consumed. The companion observes event type
and device node only, never key contents. If source attribution is unsupported,
it retains strict restoration. A single end-of-window hide handles compositor
visibility updates; there is no recurring visibility enforcement or idle polling.

The companion uses GNOME display identities, which differ from X11/portal IDs.
Select the new source and destination display bindings after changing backend.
Mirrored groups, missing monitor serials and ambiguous identities are unavailable
rather than silently mapped to another monitor. Existing identities can follow
validated geometry changes. The greeter may have different scaling/layout;
validate its bindings as well.

Only the active designated user's session or the active GDM greeter can control
the desktop bridge. Login and lock-screen handoffs require the explicit option,
existing peer trust, configured rules and successful destination placement.
Authentication is still required to log in or unlock. Other Linux greeters and
Windows secure/login desktops are not supported by this addition.

## Permissions and isolation

- `easy-switch-link` owns receivers, caches and the reporting recovery journal.
  Scoped udev permissions grant HID++ and passive event access only for supported
  Logitech receiver product IDs. No general `input` group membership is added.
- `easy-switch-link-peer` owns LAN pairing keys and the network coordinator when
  login-screen networking is enabled. It has no input-device access.
- Both accounts have no login shell, capabilities, or root privileges. Systemd
  restricts filesystem writes and process privileges; the peer uses a private
  device namespace. The receiver role cannot create Internet sockets.
- Local service sockets authenticate kernel peer credentials. Only the designated
  user's active unlocked session can change configuration or pair computers.
  The coordinator may request configured handoffs and binding-only layout updates;
  it cannot send arbitrary HID reports or read raw input through IPC.
- Raw input is consumed only to update activity counters. Key codes and typed
  content are not stored, logged or sent to the peer coordinator. This service
  nonetheless has access to input from its permitted devices, including before
  login; this is why its account is isolated from the network-facing role.

Trusted peers retain the existing TCP port (default 24873) and optional mDNS
discovery. There are no new network control APIs. Configure the firewall for the
same trusted LAN scope; discovery does not grant trust. Pairing cannot be opened
from the greeter or locked session, and an existing pairing window closes on lock.

## Retry observation and diagnostics

System mode observes receiver input through event-driven evdev reads. Coverage
loss, overflow, missing access, removal, session changes and later handoffs cancel
recovery. When receiver collections cannot identify individual devices, activity
cancels retries conservatively by receiver and keyboard/pointer class.

Retries retain the bounded 0.2.3 policy and require fresh HID++ confirmation of the
same device and source host. The primary mouse is never retried or rolled back.
User-only Wayland operation does not have this privileged observation path and
continues to report automatic retries unavailable.

Settings and `status --json` report the execution mode, designated UID/seat,
observation coverage, desktop controller and cursor capability. Service logs are
available with `journalctl -u easy-switch-link-hardware -u easy-switch-link-peer`;
receiver event logs also live under its private state directory. Consult
`VALIDATION.md` for the distinction between simulated tests and actual hardware
confirmation before relying on pre-login handoffs.
