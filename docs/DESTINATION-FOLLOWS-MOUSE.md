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

Edge switching requires a small outward push beyond the boundary: **12 units of
relative mouse motion**, normalized by display scale. Merely reaching the edge,
including a fast approach from inside the display, does not switch. At an internal
monitor seam, only motion beyond that seam counts. Raw mouse sensitivity and OS
acceleration affect how this feels; the threshold is not an exact physical distance.

Pressure resets when the pointer reverses, leaves the segment, changes rules,
pauses for 250 ms, or when buttons are held, synthetic motion occurs, settings
change or the desktop locks. The old 16-pixel inward-movement requirement has been
removed, and the 500 ms cooldown is now **80 ms**. After this brief duplicate-event
guard, a fresh push can switch back immediately. Arrival remains eight logical
pixels inside the destination display. Ambiguous corner crossings are ignored.

On Wayland, the portal temporarily captures real EIS pointer motion to measure
the push. Capture is released as soon as the threshold is reached, movement
reverses, buttons are held, a control command arrives, or after 200 ms. It does
not synthesize a push from an edge-activation notification. This timeout bounds
capture; it is not a dwell requirement or an added delay once enough motion arrives.

An unavailable, paused or locked peer (unless explicitly enabled through the
0.3.0 GNOME system-service option), stale display binding, permission denial,
or missed readiness deadline leaves the devices on the source computer. The
destination has at most 500 ms to acknowledge cursor placement. Commands enter
the existing priority HID queues with a 100 ms expiry; the primary mouse is queued
after followers. Different receiver workers may execute concurrently. Hardware
switches are not atomic. Idle followers may receive bounded recovery attempts as
described below; the primary mouse is never rolled back.

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

Geometry and binding changes are now automatically verified for the same unique
display ID, locally and on previously paired computers. Updated bindings are saved
atomically; segment percentages and chosen displays are preserved. Missing or
ambiguous display identities still need attention in settings; another monitor
is never substituted. Headless
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

## Cursor visibility and About (0.2.1)

On Windows and X11, an edge handoff hides the source cursor after the primary
mouse's HID switch command has been written successfully. Manual Easy-Switch
keys, tray/CLI switches and disconnections do not hide it. Queued, expired or
failed writes do not hide it either; radio arrival remains unverified.

The cursor reappears on destination placement or physical keyboard/mouse input.
This includes unknown and unlinked input devices, plus a linked device returning
manually. Input is never captured or discarded. Receiver input collections do
not always identify individual paired devices, so restoration conservatively
accepts all physical input. Input processed after the switch write wins over a
late hide notification. Pause, disable, layout changes, lock/suspend and clean
exit also restore the cursor. Known synthetic X11 input is ignored.

Windows uses the system cursor visibility API without changing the cursor theme;
X11 uses XFixes, which also releases the hide if the client disconnects.
Wayland portals do not offer independent desktop cursor hiding or global input
observation outside capture, so cursor hiding is unavailable in the portal-only
backend. The optional 0.3.0 GNOME companion provides a compositor-backed path;
see [Linux system operation](SYSTEM-SERVICE.md). Edge
switching remains available. Settings and JSON desktop diagnostics report cursor
visibility support and whether this instance has hidden the cursor.

Open **About** from Settings or the tray, or run `easy-switch-link about`, to
see the executable's version and license. The standalone About window works
without starting the monitor.

## Rapid handoff follower recovery (0.2.3)

Edge dispatch includes known mapped followers even when their cached connection
or host state is stale. Followers are queued before the primary mouse. On Windows
and X11, a missed follower gets up to two follow-up verification attempts, starting
250 ms after dispatch and then 750 ms after the preceding check, within a
three-second recovery window. Each verification has a 200 ms request budget and
uses the existing receiver owner, yielding to urgent switch writes.

A resend requires fresh HID++ replies confirming the stable unit identity and
that its selected host is still this source computer. Feature indices are resolved
again. A missing reply, a different unit in the slot, or an unexpected host never
causes a blind resend. Expired initial commands and reconnecting cached followers
can recover through the same path. Normal idle operation still does no polling.

Any input from the relevant class of device cancels recovery: keyboard input for
keyboards, pointer input for mice, either for other/unknown device types. This is
conservative because receiver input collections do not always distinguish paired
units. Moving the mouse does not cancel an idle keyboard's retry. Activity is
checked again after verification and immediately before a resend; no key or button
contents are retained. HID++ control notifications also cancel that device's
recovery, including Easy-Switch keys that bypass ordinary OS keyboard events.
New edge/manual switches, incoming handoffs, pause,
configuration changes, session lock, receiver removal, and destination loss cancel
superseded recovery. In user-only Wayland mode the standard portal cannot prove keyboard inactivity
outside capture, so automatic retries are disabled there rather than moving a
device that may have been used locally. Version 0.3.0 system mode supplies passive
receiver input observation for guarded Wayland retries. Recovery is bounded and cannot guarantee
radio arrival for an unreachable device.
