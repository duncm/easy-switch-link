# Destination follows mouse (0.2.0)

Enable this feature only on computers where you want edge switching. Ordinary
Easy-Switch forwarding remains available with the feature disabled. It moves
Logitech devices using their radio host-switch command; it cannot eliminate radio
reconnection time. It does not forward ongoing input, screen content or clipboard.

## Setup

1. Install 0.2.0 on both computers. Existing version 1 configuration is migrated
   atomically to version 2; `config-v1.backup.toml` preserves the original. Groups,
   channels and startup preferences are retained. Edge switching starts disabled.
2. Open settings, select **This computer**, enable **Destination follows mouse**,
   and save. Use **Detect displays / grant desktop access** if needed.
3. Open pairing on both computers. Choose a discovered peer or enter
   `hostname:24873`. Compare the six digits on both computers, then confirm on
   **both**. Discovery by itself grants no trust. Pairing expires after two minutes.
4. Add a directed edge rule. Select the source display and percentage range,
   destination display/range, group and primary mouse. When only one mouse is
   identified in the group it is selected automatically. Save and enable the rule.
5. **Add reverse rule** offers a disabled proposal on the other computer. Review,
   enable and save it there. It does not silently create an active reciprocal rule.

Segments run top-to-bottom on vertical edges and left-to-right on horizontal
edges. For example, the bottom two-thirds is **33.333333–100%**. A crossing halfway
through that segment enters halfway through the chosen destination segment.
Arrival is eight logical pixels inside the destination display.

Windows and X11 suppress crossings during mouse-button drags and require a
500 ms cooldown plus 16 logical pixels of inward movement before another edge
activation. Wayland uses the explicitly approved alternative: a 500 ms cooldown
and eight-pixel arrival inset, because the standard portal cannot observe ordinary
pointer movement while capture is inactive. Ambiguous corner crossings are ignored.

An unavailable, paused or locked peer, stale display binding, permission denial,
or missed readiness deadline leaves the devices on the source computer. The
destination has at most 500 ms to acknowledge cursor placement. Commands enter
the existing priority HID queues with a 100 ms expiry; the primary mouse is queued
after followers. Different receiver workers may execute concurrently. Hardware
switches are not atomic: a partial failure is reported without automatic rollback.

## Desktop support

- **Windows:** Raw Input, per-monitor DPI coordinates, display-device identities,
  desktop/session notifications and `SetCursorPos`. Secure/inactive desktops are
  unavailable. No permanent hooks or administrator privileges are required.
- **X11:** XRandR, XI2.3 raw motion and XFixes5 pointer barriers, XWarpPointer,
  Xft DPI and logind lock/session/suspend signals. An active graphical logind
  session is required; an unavailable session watcher fails closed.
- **Wayland:** InputCapture v1 or v2 and RemoteDesktop/EIS with mapped absolute
  pointer regions are required. Only pointer capabilities are requested. No
  ScreenCast stream is created. Missing portals, rejected barriers or missing EIS
  mappings make edge switching unavailable without affecting normal HID operation.
  Portal v2 restoration tokens and RemoteDesktop tokens are kept locally; a
  compositor may still request permission again. GNOME/KDE support depends on
  their portal implementation and version, not merely the desktop name.

Wayland source segments must be on the exposed desktop boundary. Internal monitor
seams are not supported by the standard InputCapture portal. Destination coordinates
use EIS regions, not assumed physical pixels. Capture is released promptly; a
missing synchronized EIS frame cancels activation. Wayland never restores a cursor
on cancellation when absence of intervening local input cannot be established.
See [InputCapture](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)
and [RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html).

If a display is absent or its geometry/binding changes, reselect and verify it in
settings. Never select a different display merely to clear a diagnostic. Headless
HID service operation works without these desktop capabilities; edge switching
needs the session's `DISPLAY`/`XAUTHORITY` or `WAYLAND_DISPLAY` and session D-Bus.
For a systemd user service, import the graphical environment at login:

```sh
systemctl --user import-environment DISPLAY XAUTHORITY WAYLAND_DISPLAY XDG_SESSION_ID XDG_SESSION_TYPE
systemctl --user restart easy-switch-link.service
```

## LAN and permissions

The optional peer endpoint listens on TCP **24873** (configurable). mDNS discovery
uses UDP **5353**. If a firewall blocks inbound LAN traffic, allow the executable
on the private network, or allow this TCP port from the specific peer's LAN address.
Manual addressing avoids dependence on mDNS. There is no automatic router/UPnP
configuration. A persistent connection initiated from either side carries both
directions, with bounded reconnect backoff. Disabling the feature closes the peer
listener and connections.

TLS 1.3 authenticates per-installation certificates pinned after two-sided code
confirmation. The limited pairing flow is the only time unpinned certificates are
accepted. Network messages do not expose local configuration commands or local IPC.
**Revoke** removes a peer's trust and closes its connection. To replace an identity,
revoke it and pair again; copying a hostname does not establish trust.

Private identity, trust and portal-token files stay in the user configuration
directory, separate from `config.toml`. Export/import includes rules and peer
references but never these credentials. Pair again after importing on another
installation, choose its local identity, and verify display bindings. Do not copy
the entire private configuration directory to another computer.

## CLI

```text
easy-switch-link edges displays --json
easy-switch-link edges peers --json
easy-switch-link edges rules --json
easy-switch-link edges status --json
easy-switch-link edges authorize
easy-switch-link edges pair
easy-switch-link edges pair other-computer:24873
easy-switch-link edges confirm <peer-fingerprint> <matching-code>
easy-switch-link edges revoke <peer-fingerprint>
easy-switch-link edges enable <rule-id>
easy-switch-link edges disable <rule-id>
easy-switch-link edges reverse <rule-id>
```

JSON diagnostics include desktop geometry, readiness, pairing state, rule reasons
and the latest handoff outcome. Pairing/control actions are queued; inspect status
for the result. Neither diagnostics nor logs include private keys. Handoff timing
reports LAN preparation/queue timing separately from unverified radio arrival.

See [validation](VALIDATION.md) for measured results and untested combinations.
