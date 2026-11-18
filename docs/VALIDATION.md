# Validation

## 0.3.5 - GitHub release publishing (2026-11-18)

Formatting, strict Clippy, and all 61 Windows workspace tests passed. The
Windows x64 executable and MSI built successfully. The executable reports 0.3.5,
the MSI ProductVersion is 0.3.5, and all Windows artifact checksums verified.

The GNOME companion checks passed and the companion archive carries version
305 (0.3.5). Both GitHub Actions workflows passed actionlint. Local release
simulations verified tag and version matching, checksum checks, draft recovery,
and rejection of missing or corrupt assets.

Linux amd64 and arm64 builds and smoke tests, the Windows installer lifecycle,
physical hardware, and a live GitHub release run remain unverified for 0.3.5.

## 0.3.4 � tray startup ordering (2026-09-27)

Test-bravo's XDG-autostart session companion was running, but its journal reported
StatusNotifierWatcher ServiceUnknown at login. GNOME's AppIndicator extension
became available afterward; the failed tray thread did not register again.
Restarting only the session companion registered the Easy-Switch Link item on
the live GNOME session bus; receiver and peer services stayed active.

The Linux tray now keeps ksni's D-Bus owner-change subscription alive while the
tray host is absent, registers when it appears, and survives watcher restarts.
There is no polling/backoff timer and no new tray-only service. Documentation
identifies the XDG autostart file and distinguishes the headless user unit from
the system-mode session companion.

A private-bus integration test starts the real application without a watcher,
adds one later, and replaces it while checking that the same application/tray
owner registers again. The previous 0.3.3 release reproduces the startup failure
in this test. Version 0.3.4 passes both late-start and watcher-restart cases on a
real private D-Bus with a simulated watcher and the production tray executable.
The regression is included in native Linux CI for both architectures.

Windows x64 passes 61 Rust tests and Linux amd64 passes 68; strict workspace
Clippy and formatting pass. Windows/GNOME cursor and hardware dispatch behavior
are unchanged. The running 0.3.3 tray was restored on test-bravo by restarting only its
session companion. Version 0.3.4 was then installed on test-bravo; its installed executable matches the
ARM64 release hash. Both system services are active, the session companion
registered an Active Easy-Switch Link tray item, and GNOME handoff readiness is
preserved. The companion extension code did not change, so no logout was needed.
Full GNOME login ordering was not retested interactively; the private-bus test
covers the reproduced ordering failure and watcher restart.

All six release artifacts and the GNOME ZIP were built. Linux amd64 package
install/upgrade from 0.3.3/reinstall/removal, headless IPC, activation rollback,
configuration restoration and service-account isolation checks pass. Windows
portable launch and MSI version metadata pass; Windows installer lifecycle and
native ARM64 unit tests were not repeated for this patch.

## 0.3.3 � GNOME receiver-motion grace (2026-09-27)

The user authorized a 250 ms receiver-level motion exception while retaining
both Logi M650 pairings. A read-only Bolt inventory on test-bravo found MX Keys S,
MX Keys Mini, MX Master 3S, and two M650 slots; no pairings were removed. Linux
exposes a shared receiver mouse node, so this is explicitly not per-mouse
attribution. Sunshine's virtual passthrough nodes remain outside the exception.

Before dispatch, the Linux coordinator arms the companion with pointer-node
metadata resolved through the selected mouse's physical USB receiver. The peer
account opens no event devices. GNOME's observation-only event filter always
propagates input and reads only event type, source node and held-button state.
Successful edge-write acknowledgement opens a window ending 250 ms after the
write, not after message delivery. Unknown/other/virtual input, clicks, keys,
scrolling, held buttons, unplugging, arrival, configuration/session changes and
controller loss invalidate it. A single end-of-window hide handles compositor
visibility updates. No recurring cursor polling or visibility forcing is added.
Unsupported observation retains conservative restoration; Windows/X11 behavior
and all HID dispatch/retry rules remain unchanged.

Windows x64 passes 61 Rust tests; Linux amd64 passes 68, with strict workspace
Clippy. JavaScript tests cover receiver tails before/after acknowledgement,
unknown and virtual input, clicks/keys/scroll, held buttons, unplugging, delayed
acknowledgements, cancellation, wrong transactions, invalid node paths, bounded
cleanup, and observation-only filter registration/removal. Native GJS/Gio socket
tests pass with compositor doubles. All six release artifacts and the companion
ZIP built; archive hashes match on Windows/Linux. Windows executable/MSI version
metadata and portable launch pass. Linux amd64 installation, upgrade from 0.3.2,
reinstall, removal, IPC, rollback, configuration restoration and service-account
isolation pass in the disposable root. ARM64 unit tests and Windows installer
lifecycle tests were not repeated locally.

The user installed 0.3.3 on test-bravo and logged back into GNOME Wayland. The loaded
companion is 303, both services are active, and diagnostics advertise the receiver
motion grace. The installed ARM64 executable matches the release hash. During
final testing, test-alpha's installed Windows process also reported 0.3.3; its
installer operation was not observed by this test run.

The user confirmed repeated edge round trips: both MX Keys S and MX Master 3S
follow, and test-bravo's cursor remains hidden after departure. Live diagnostics agree
with cursor_hidden=true. A sampled outgoing handoff recorded 23.67 ms to readiness,
34.09 ms from readiness to queue, and 0.5/0.9 ms receiver dispatch for keyboard/mouse.
These are diagnostic samples, not radio reconnection measurements or an idle soak.
M650/unknown-input restoration, the grace-window boundary on physical hardware,
GDM/locked sessions and new idle resource targets remain hardware-unverified.
M650 and MX Keys Mini pairings were preserved; no receiver enrollment changed.

## 0.3.2 � GNOME Wayland placement (2026-09-27)

After test-bravo switched from X11 to GNOME 48.7 Wayland, a bounded companion-socket
trace showed `placed: false` immediately before a motion notification at the
requested destination. Test-alpha correctly sent no hardware switch commands; the
user confirmed both devices remained there. Mutter queues native pointer warps
on an input thread, so the old immediate position check rejected a valid move.

The companion now observes cursor position notifications after one warp, with
balanced tracking and the existing preparation deadline. It does not poll or
repeatedly warp. Timeout, session/topology interruption and controller replacement
release the listener. Failed/expired/abandoned placement acknowledgements release
the Rust reservation; compositor cancellation retains its independent no-input
restore guard. Old cancellation/completion IDs cannot erase a newer reservation.
Rejected hide requests now report a bounded reason in the local management log.

Windows x64 passes 61 Rust tests and Linux amd64 passes 68; strict workspace
Clippy and formatting pass. JavaScript regression tests cover delayed completion,
confinement/deadline failure, cleanup, stale controllers/transactions, input during
cancellation, abandoned replies and desktop invalidation. Native GJS/Gio socket
checks pass on test-bravo with compositor doubles. ARM64 was cross-built against the
Ubuntu 22.04 baseline; native ARM64 unit tests were not rerun. The native package
upgrade from 0.3.1 to 0.3.2 succeeded on test-bravo, both system services restarted,
and the installed executable hash matches the release build. After the user's
logout/login, GNOME reports companion 302 active in a Wayland session.

All six release artifacts and the companion ZIP built. Windows executable/MSI
version metadata and CLI launch checks pass; the live Windows installation
remains 0.3.1 because this fix changes the Linux/GNOME path only. Windows 0.3.2
installer lifecycle testing was not repeated locally. Linux amd64 package
install, upgrade from 0.3.1, reinstall, removal, headless IPC, activation rollback,
configuration restoration and service-account isolation checks pass in the
isolated root. Companion ZIP hashes match across Windows/Linux packaging.
The user confirmed repeated edge round trips with both MX Keys S and MX Master
3S following every time. The companion trace shows three successful placement
acknowledgements followed by completion. Windows logged both device writes for
each sampled outgoing handoff. Cursor placement was not separately confirmed in
the user's reply. GDM/locked sessions and new idle resource measurements were
not tested for this patch.

Cursor hiding remains an open issue: the user reports that test-bravo's cursor stays
visible. On one traced departure, the companion acknowledged hidden=true, then
reported hidden=false about 4 ms later alongside outward physical motion, with
another motion report about 4 ms after that. Other departures received motion
after the switch write before a hide request was sent. The current all-input
restoration/delayed-hide guard therefore conflicts with the mouse's trailing
radio reports. This patch does not claim that cursor hiding is fixed.

## 0.3.1 — return switching in system mode (2026-09-27)

An incoming edge handoff called receiver invalidation, which advanced the shared
configuration epoch without applying a new configuration to the worker. Later
manual/edge writes were silently discarded and notifications carried an obsolete
epoch. A configuration change temporarily repaired the mismatch, explaining the
intermittent behavior. On test-bravo, an observed failed return was reported as queued
without corresponding device-write messages, while device availability was stale.

Switch cancellation now uses its own generation. It rejects old queued commands
and follower retries while preserving the applied configuration and subsequent
notifications. Configuration, pause, identity, disconnect and expiry checks remain
independent. Regression coverage exercises three cancellation cycles followed by
manual and edge writes, notification delivery, and monitor-level invalidation.

Windows x64 passes 61 tests; Linux amd64 passes 67 tests in the Ubuntu 22.04
baseline environment. Strict workspace Clippy and formatting pass. ARM64 is
cross-built against the same baseline; native ARM64 unit tests were not rerun for
this patch after test-bravo's reboot cleared the temporary build environment.
All six release artifacts and the companion ZIP were rebuilt with verified
checksums. The ARM64 portable executable launches on test-bravo and reports 0.3.1.
Linux package upgrade/reinstall/removal, activation rollback, configuration
restoration and service-account isolation checks pass in the disposable root.

The user installed 0.3.1 on test-bravo; its installed binary hash matches the release
portable executable. Both dedicated services restarted and the session tray was
restored. The user confirmed three test-alpha → test-bravo → test-alpha edge round trips
and a further manual return, with both MX Keys S and MX Master 3S following every
time. This was GNOME X11 in an unlocked session; test-alpha remained on 0.3.0.

Test-bravo's live status recorded repeated mouse/keyboard HID writes (0.4–1.4 ms
receiver dispatch in the sampled messages), and one guarded keyboard recovery
after fresh source-host and no-input confirmation. A sampled edge preparation
took 0.78 ms and readiness-to-queue took 30.64 ms. These are diagnostic samples,
not measured radio reconnection latency or a performance soak. The 0.3.0 GDM,
lock-screen and physical Wayland validation limits still apply.

## 0.3.0 — optional Linux services and GNOME companion (2026-09-27)

### Automated and native-runtime checks

- Windows x64: 60 Rust tests, strict workspace Clippy and formatting pass.
- Linux amd64 (isolated Ubuntu 22.04 root) and native Linux ARM64 (test-bravo,
  Debian 13): 66 Rust tests and strict workspace Clippy pass. Coverage adds
  service-account authorization, coordinator restrictions, bounded IPC frames,
  configuration isolation, passive input classification, overflow and lost input
  coverage. Existing protocol, TLS, replay, retry and cursor-ordering tests pass.
  Live logind inspection also confirmed that test-bravo's Shell runs in the systemd
  user manager; its narrowly scoped fallback checks the GNOME Shell unit, owning
  UID, desktop type and logind's selected graphical session.
  A read-only call to the production authorization function accepted test-bravo's
  active GNOME X11 Shell through this fallback.
- Production GNOME JavaScript passes tests using compositor doubles for late
  hide requests, intervening input, placement deadlines, held buttons, bounds,
  cancellation and stale controller messages. These are simulated compositor
  operations, not physical pointer tests.
- Native GJS/Gio on test-bravo passes the companion socket test: kernel credentials,
  framed hello, hide/restore protocol and disconnect cleanup. The compositor is
  replaced by a test double in this test; the socket implementation is real.
- Read-only GNOME 48.7 API inspection on test-bravo confirms cursor visibility,
  pointer warping, barrier construction and idle activity APIs. GNOME 48 uses
  `set_pointer_visible`; newer inhibit/uninhibit APIs are selected only when both
  are present. Metadata permits 45–51, with runtime checks; other versions have
  not been tested.
- An isolated nested Shell loaded the companion and reported it active in logs.
  Subsequent extension-status queries timed out. An empty no-op extension
  reproduced that failure in the same test environment. Full nested compositor
  barrier/placement/cleanup acceptance therefore remains inconclusive.

### Packaging and service isolation

- All six 0.3.0 artifacts and the deterministic companion ZIP are produced with
  locked dependencies. Linux release binaries target the Ubuntu 22.04 baseline;
  ARM64 compilation against that baseline is cross-compiled, with Rust tests
  separately run on native ARM64. Windows retains its static CRT and resources.
  Final portable ARM64 version/help launch passes on test-bravo; Windows executable
  resources and MSI metadata report 0.3.0. Both Debian packages require glibc 2.35,
  preserving the Ubuntu 22.04 / Debian 12 baseline.
- The disposable amd64 Ubuntu root passed 0.2.3-to-0.3.0 package upgrade,
  reinstall, removal, portable headless IPC and systemd unit verification.
  Installation leaves system mode disabled.
- Dedicated service-account processes passed real Unix-socket authorization:
  the designated owner can read status, while an unrelated UID and the peer
  account cannot issue owner commands. Each service account is denied access to
  the other's private state directory.
- The root activation helper passed forced activation-failure rollback and a
  configuration round-trip returning the latest service configuration to the
  user. Service-manager commands were stubbed for those migration tests; receiver
  process startup and IPC were tested separately with a private D-Bus daemon.
  This does not establish successful live systemd sandbox or GDM activation.

### Hardware and performance acceptance still required

The installed applications and GNOME/GDM settings on test-alpha and test-bravo were not
changed for these tests. No 0.3.0 pre-login or locked-session handoff has yet been
confirmed on physical hardware. Earlier release confirmations and resource
samples do not validate this release.

Outstanding checks include test-alpha ↔ test-bravo GDM/locked/user-session round trips,
mapped entry position and cursor hiding, input from unlinked devices, rapid
reverse crossings and guarded retries, actual evdev permission coverage, GDM
restart and logout/login, display scaling/hotplug, suspend/resume, failed extension
loading, native ARM64 package upgrade/removal, and Windows MSI installation.
Measure combined receiver/coordinator/session CPU and memory, preparation/dispatch
latency and radio reconnection separately on that setup. The existing 0.1% CPU,
30 MiB resident-memory and 100 ms dispatch goals remain unverified for system mode.

Use `docs/SYSTEM-SERVICE.md` for explicit opt-in and recovery instructions. Missing
or unauthorized compositor sessions must report unavailable and leave hardware
on the source; physical Easy-Switch synchronization remains independent of LAN
and compositor readiness. Windows secure desktops and non-GNOME greeters are
outside this release.

## 0.2.0 — destination follows mouse (2026-09-24)

### Automated and package checks

- 43 tests pass on Windows x64 and Linux amd64, with strict Clippy and formatting.
  Coverage includes full/partial edges, proportional placement, negative coordinates,
  DPI scaling, internal seams, corner ambiguity, overlap rejection, injected motion,
  held buttons, cooldown/inward movement, cancellation, concurrent crossings,
  readiness timeout, permission failure, connection loss, stale topology/configuration,
  revocation, and partial dispatch. Display-binding epochs survive process restarts.
- Real TLS handshakes over simulated streams verify pairing/pin requirements and
  revocation. A loopback TLS connection rejects a replayed preparation transaction.
  Tests also check frame-size limits, two-sided pairing confirmation/expiry,
  configuration migration and exclusion of private-key data from exported TOML.
- All six release artifacts are built with locked dependencies. Windows uses a
  static CRT and embedded resources. Linux binaries are built against Ubuntu 22.04.
  Native Windows MSI upgrade and portable launch pass. Linux amd64 installation,
  headless IPC, removal, and ARM64 QEMU launch pass. The user installed the ARM64
  package on test-bravo; its executable hash matched the corresponding portable artifact.
- Settings rendering was inspected under Xvfb with recorded hardware/peer status.
  This checks layout, not native Wayland interaction.

### Physical hardware confirmation

The user confirmed three round trips with **MX Keys S and MX Master 3S over Bolt**
between test-alpha (Windows 11, channel 1) and test-bravo (GNOME X11, channel 2): both devices
followed, the cursor entered at the mapped height, and there was no immediate bounce.
With test-bravo paused, the user also confirmed that pushing the configured edge kept
both devices on test-alpha; the source reported destination unavailability. Test-bravo
was resumed immediately afterward.
The configured directed rules are test-alpha's rightmost display, right edge
33.333333–100%, to test-bravo's full left edge, and the explicitly requested reverse.

Native display discovery reported 200%, 175%, and 250% scaling on test-alpha, and one
3840×2160 display at 200% scaling on test-bravo. Pairing codes and certificate fingerprints
were verified on both machines. Pairing and rules remained available after monitor
restarts. Existing keyboard-initiated switching configuration was preserved.

Observed return handoffs queued commands about 0.1 ms after destination readiness.
Receiver logs recorded approximately 1.2–62.7 ms from validated edge event to mouse
HID write across the observed return switches. These figures exclude input latency
before the validated event and **do not measure radio reconnection**. The user's
visual confirmation establishes mapped arrival, not a quantified reconnection time.
Handoff JSON now includes transaction IDs and separate preparation/queue durations;
receiver logs carry the same transaction ID.

### Resource samples

Settings were closed, peer coordination and discovery enabled. Input-idle checks
used the OS last-input counter (Windows) and XScreenSaver idle time (X11).

| Host / sample | CPU time / wall time | One-core average | Resident memory |
| --- | --- | --- | --- |
| test-alpha, both devices selected locally | 0.015625 s / 30.019 s | 0.052% | 28.9 MiB |
| test-bravo, devices selected on test-alpha | 0.04 s / 55.001 s | 0.073% | 16.9 MiB |

A shorter 35-second Linux sample was 0.114%; OS CPU-counter quantization and network
activity affect short samples. These are observations, not universal limits or a
long-duration soak. Linux with both devices selected locally remains unmeasured.
The shared Linux D-Bus runtime is limited to one worker by default, reducing the
observed monitor thread count from 76 to 13 on test-bravo. When the feature is disabled,
the peer coordinator is not started and no network listener is opened.

### Still unverified

- GNOME and KDE **Wayland** portal/EIS interaction, permission restoration/revocation,
  exposed-boundary rejection, and cursor placement on those desktops. The backend
  is compiled and capability-gated; it must not be advertised as hardware confirmed.
- Physical display unplug/rotation/scaling and lock/suspend cycles under edge
  switching; the relevant generation, lifecycle and cancellation paths have tests.
- Unifying/mixed-receiver edge handoffs, other firmware, native ARM64 package
  uninstall, a fresh login-startup cycle, and long-duration resource measurements.
- Atomic all-device switching is not possible. Commands to different receivers
  may execute concurrently; queued followers precede the primary mouse, and partial
  failures are reported without rollback. Radio reconnection still causes a pause.

## 0.1.3 — computer names in switch commands (2026-09-23)

- `switch` resolves configured computer names case-insensitively and preserves
  exact ID precedence. Duplicate names are rejected with candidate IDs, before
  any commands are dispatched; unknown and partial names are also rejected.
- The CLI resolves using the running monitor's configuration and sends the
  canonical ID. The monitor also resolves names for direct IPC/older clients.
- 25 tests pass on Windows x64 and Linux amd64; strict Clippy and formatting pass.
  Name-resolution coverage includes IDs, names, case, spaces, unknown/partial
  names, ambiguous labels and collisions between a label and an existing ID.
- An isolated Linux IPC smoke test passed with a 0.1.3 CLI/0.1.2 monitor and
  a 0.1.2 CLI/0.1.3 monitor. Both combinations accepted `test-bravo`, `TEST-ALPHA`,
  `Office PC` and `computer-2`, and rejected the partial name `test-b`. Empty test
  groups were used; no physical device was switched for this validation.
- All six 0.1.3 artifacts built. Linux amd64 package install/removal and headless
  IPC smoke tests pass; the ARM64 version smoke test passes under QEMU. Test-alpha's
  portable monitor/startup entry is updated. Test-bravo's ARM64 package is staged in
  the user's home directory for installation with sudo; a monitor restart is
  unnecessary for name support through the updated CLI.

## 0.1.2 — switch dispatch priority (2026-09-23)

- Code inspection found that switch commands shared the receiver's management
  queue and could wait behind discovery/reporting transactions with two-second
  reply deadlines. Those transactions forwarded notifications but did not service
  newly queued switch commands until returning to the outer command loop.
- Switches now use a separate bounded queue, serviced by the same transport owner
  before and during transaction waits. The existing OS wake signal interrupts the
  wait; no periodic poll, extra thread, or network connection is introduced.
- Notification-receipt timestamps follow the command through the coordinator to
  the HID write. Logs expose notification-to-write and write-call duration.
  These do not measure physical-key-to-notification or radio/input reconnection.
- 24 tests pass on Windows x64 and Linux amd64, with strict Clippy checks. New
  tests cover a switch interrupting an unrelated pending query, routing around a
  full management queue, and preservation of pause/epoch/disconnect/identity guards.
  In the simulated pending-query test, the switch must be written before the query
  reply is delivered, with total test execution below 100 ms.
- All six release artifacts built. Windows MSI install, repair and uninstall
  succeed; Linux amd64 package install/removal and headless IPC pass in the Ubuntu
  22.04 build environment. The ARM64 executable passes its QEMU version smoke test.
- Both computers were updated to 0.1.2 (test-bravo's native package is 0.1.2-1).
  During user-operated MX Keys S/MX Master 3S Bolt switches, recorded dispatch
  times were 0.714, 0.417, 0.701 and 0.473 ms on test-alpha, and 0.440, 3.040,
  0.766 and 0.432 ms on test-bravo. The eight samples span 0.417–3.040 ms from receipt
  of the keyboard notification to completion of the follower HID write.
- **User feedback: visible delay still noticeable.** No instrumented 0.1.1
  baseline exists, so a perceptual improvement has not been demonstrated. The
  remaining delay in this trial is outside the measured dispatch interval;
  physical press-to-notification timing, receiver/radio delivery and destination
  input readiness were not separately measured. A successful HID write does not
  establish arrival at the destination. Network input sharing was not implemented.

## 0.1.1 — reconnect/conflict fix (2026-09-23)

- Test-bravo (Debian 13 ARM64) logs showed repeated automatic pauses from the old
  reporting-conflict detector. OpenLogi 0.8.6 also had the MX Keys S configured
  to switch the MX Master 3S. With the user's authorization, only that OpenLogi
  `host_switch_targets` rule was disabled; its other settings were retained and
  its agent restarted. The original configuration was backed up on test-bravo.
- A `setCidReporting` response echoes a masked update, not the complete resulting
  state. The old detector paused for every foreign write to an owned control.
  The fix coalesces relevant replies and reads the actual reporting state before
  declaring an incompatible change. Reconnect/disconnect clears pending checks;
  failed queries do not imply conflicts. There is no periodic reporting poll.
- Corrected analytics-capability decoding to use byte 8 of `getCidInfo`, not the
  NONSTANDARD flag in byte 4. Both devices were rediscovered on Windows; the
  keyboard still advertises analytics on all three Easy-Switch controls.
- **User-confirmed hardware result:** three keyboard-initiated round trips from
  test-alpha (Windows 11, channel 1) to test-bravo (Linux, channel 2) and back succeeded.
  The MX Master 3S followed every time and test-bravo remained unpaused. Both monitors
  ran 0.1.1, with Bolt receivers. This validates the combined code/configuration
  fix, not coexistence with OpenLogi's host-switch rule enabled.
- 21 tests pass on Windows x64 and Linux amd64. Added coverage includes repeated
  harmless reporting writes, coalesced genuine conflicts, high software IDs,
  reconnect cleanup, disconnect during verification, failed verification, and
  capability/state decoding. Strict Clippy and formatting checks pass.
- All six 0.1.1 artifacts built. Windows MSI install/repair/uninstall pass;
  Linux amd64 package install/removal and headless IPC pass in Ubuntu 22.04.
  The ARM64 executable starts under QEMU and runs natively on test-bravo with its tray.
- Test-alpha runs the new portable executable and its existing login startup points
  to that executable. Test-bravo's package upgrade initially required the user's sudo
  password, so the standalone build was used for the round-trip test. A subsequent
  check confirmed the native ARM64 system package was upgraded to 0.1.1-1 and its
  enabled systemd user service was running the installed build, unpaused. That
  service owns the monitor, so a separate tray instance is not started alongside
  it. Configuration and channel mappings are preserved.
- Idle resource measurements and latency have not been repeated for this patch.

## 0.1.0 — original validation record

Recorded on 2026-09-23. Hardware claims below distinguish capability discovery
from physically switching between multiple computers.

## Automated checks

- 15 Rust tests pass on Windows x64 and Linux amd64: report validation, dynamic
  feature routing, channel mapping, duplicate/self-generated event suppression,
  pause/disconnect handling, stale hotplug sessions, partial failure, notification/reply interleaving,
  bounded request timeout, durable recovery, atomic configuration and IPC limits.
- Strict Clippy checks pass on Windows and Linux. Formatting is checked.
- All three release targets compile using Rust 1.98.1 and the committed lockfile.
- Windows release embeds an icon and statically links the C runtime.
- Linux binaries are built against Ubuntu 22.04, requiring glibc >= 2.35.
- Windows MSI install, repair and uninstall smoke tests return exit code 0;
  the installed executable reports version 0.1.0 and is removed on uninstall.
- Linux amd64 `.deb` installation/removal and headless status/pause/resume/quit
  over the private Unix socket pass in an isolated Ubuntu 22.04 environment.
- The ARM64 executable starts and serves status/quit requests under QEMU user
  emulation. Its `.deb` metadata and architecture are checked. Native ARM hardware
  and native ARM `.deb` installation are not validated locally; CI contains a
  native ARM64 job.
- The settings window renders under Xvfb/X11. Its screenshot was inspected and
  a duplicate widget ID and missing X11 runtime dependency were fixed.

## Physical hardware checks

Windows x64, one Bolt receiver (`046d:c548`):

| Device | Observed result |
| --- | --- |
| MX Keys S | Discovered stable unit/model identity, 3 host channels, dynamic ChangeHost and reprogrammable-control indices; passive capability was initially decoded incorrectly (corrected and rechecked in 0.1.1 above) |
| MX Master 3S | Discovered stable unit/model identity, 3 host channels and dynamic ChangeHost index; no reprogrammable Easy-Switch controls advertised |

Both devices were queried successfully while awake. Initial 400 ms discovery
deadlines missed delayed Bolt responses; discovery now allows 2 seconds. Cached
switch-command dispatch does not wait for these discovery queries in steady state.

A configuration with only each device's **current** host mapped exercised saving,
passive reporting setup, status and pause without sending any host-change command.
The keyboard's passive reporting was already enabled, so this run required no owned
restoration entries. Physical diversion and restoration remain untested; those
paths have simulated transport tests.

## Resource measurement

Windows optimized build, two discovered Bolt devices, settings closed, current-host
only validation group, 30.01-second idle sample:

| Measurement | Observed |
| --- | ---: |
| Working set | 17.11 MiB |
| Private memory | 6.64 MiB |
| Threads | 9 |
| CPU-time increase | None measurable at Windows process-counter resolution |

This sample meets the 30 MiB working-set and 0.1%-of-one-core idle targets for this
setup. It is not a universal bound or a Linux performance measurement.

## Unverified at the original 0.1.0 release

- Actual MX Keys S → MX Master 3S forwarding between two/three computers and
  round-trip operation with the program running on each.
- The 100 ms dispatch target measured from an actual switch notification.
- Unifying, mixed receivers, different channel mappings on physical devices,
  temporary diversion on older keyboards, hotplug, sleep/resume and firmware variants.
- Linux USB hardware access, Wayland rendering, desktop tray integration,
  login startup and coexistence with conflicting Options+/Solaar rules.
- MSI major-version upgrade from a prior release (none exists yet), native ARM64
  package installation and a long-duration resource soak.

No physical host switch was issued during development. Unsupported or unverified
trigger behavior must not be represented as proven device compatibility.

## 0.2.1 cursor visibility and About

- Windows x64 and Linux amd64: 47 unit/simulation tests pass, including successful
  HID-write notification, failed/expired/paused/stale/disconnected write rejection,
  manual switches, late/duplicate hide suppression, local input restoration,
  synthetic movement, arrival restoration and invalidated crossings.
- Strict workspace Clippy passes on Windows and Linux (including the Wayland backend).
- Native Windows Magnification API initialization, hide and restore calls returned
  success in a bounded probe; this is not a visual hardware acceptance test.
- Cursor visibility during actual Bolt handoffs, unknown/unlinked physical input,
  crash recovery on Windows, and GNOME/KDE Wayland remain hardware-unverified for
  this update. Wayland cursor hiding is unsupported through the standard portals.
- All six 0.2.1 artifacts built. Windows executable version/help and MSI version
  metadata checked; Linux package architecture/content/checksums checked, amd64
  headless start/status/quit passed, and ARM64 executable version checked under QEMU.
- About screen rendered under Xvfb and visually checked: version 0.2.1 and license
  are visible, with no clipped text. These builds have not been installed on the
  paired hardware; package upgrade and physical round-trip checks remain pending.
- Prior 0.2.0 timing/resource measurements do not measure this update.

## 0.2.2 edge buffer

- Windows x64 and Linux amd64: all 52 unit/simulation tests pass.
- Automated coverage includes all four edges, DPI scaling, fast approach without
  pressure, accumulation past internal monitor seams, direction reversal, held
  buttons, synthetic motion, rule changes, idle gaps, and fresh reverse crossings
  after 80 ms without an inward detour. X11 no longer counts both raw motion and
  the duplicate barrier-hit notification toward the buffer.
- Wayland measures actual EIS motion during bounded capture, rather than treating
  activation as a synthetic outward push. Portal interaction remains unverified
  on physical GNOME/KDE Wayland desktops.
- Physical test-alpha/test-bravo crossings, perceived buffer sensitivity, and resource
  measurements have not been revalidated for this update. Builds are not installed
  automatically on those computers.

## 0.2.3 rapid handoffs and automatic layout verification

- Windows x64 and Linux amd64: all 58 tests and strict workspace Clippy pass.
- Windows 0.2.2 logs confirmed repeated edge dispatches that listed the keyboard
  as unavailable and sent only the mouse. This establishes a stale-availability
  failure path; it does not prove every reported miss has the same cause.
- Tests cover stale/offline followers across repeated handoffs, expired initial
  writes, fresh feature indices, stable identity checks, source-host confirmation,
  already-switched devices, input before/during verification, superseding handoffs,
  bounded no-reply checks, direct HID++ control input before/during verification,
  and event-driven idle after recovery ends.
- Layout tests preserve display identity and segment mapping, reject missing or
  ambiguous IDs, and refuse updates from untrusted peers. Updates carry the source
  configuration generation so stale proposals cannot overwrite newer settings.
- Actual 0.2.3 rapid Bolt round trips, retry timing, per-class input cancellation,
  and live display hotplug/rebinding have not been hardware-validated. Wayland
  automatic retries are disabled because global idle input cannot be established.
