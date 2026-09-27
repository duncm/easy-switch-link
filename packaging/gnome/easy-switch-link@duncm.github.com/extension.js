import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Clutter from 'gi://Clutter';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const LIMIT = 65536;
const decoder = new TextDecoder();
const encoder = new TextEncoder();

export default class EasySwitchLink extends Extension {
    enable() {
        this._enabled = true;
        this._barriers = [];
        this._queue = [];
        this._delay = 1;
        this._serial = 0;
        this._lastInput = 0;
        this._reservation = null;
        this._tracker = global.backend.get_cursor_tracker?.() ?? Meta.CursorTracker.get_for_display(global.display);
        this._idle = global.backend.get_core_idle_monitor();
        this._seat = Clutter.get_default_backend().get_default_seat();
        this._inputFilter = null;
        if (Meta.is_wayland_compositor?.() && Clutter.Event?.add_filter && Clutter.Event?.remove_filter) {
            try {
                this._inputFilter = Clutter.Event.add_filter(global.stage, event => {
                    try { this._inputEvent(event); }
                    catch (_) { try { this._activity(); } catch (_) { this._cancelArm(); } }
                    // Observe only; never consume, redirect, or retain input contents.
                    return Clutter.EVENT_PROPAGATE;
                });
            } catch (_) { /* Older introspection: retain conservative idle restoration. */ }
        }
        this._signals = [
            [Main.layoutManager, Main.layoutManager.connect('monitors-changed', () => this._layout())],
            [Main.sessionMode, Main.sessionMode.connect('updated', () => {
                this._restore(); this._clearBarriers(); this._disconnect(); this._connect();
            })],
        ];
        this._connect();
    }

    disable() {
        this._enabled = false;
        this._disconnect();
        if (this._inputFilter !== null) Clutter.Event.remove_filter(this._inputFilter);
        this._inputFilter = null;
        if (this._retry) GLib.Source.remove(this._retry);
        this._retry = 0;
        if (this._watch) this._idle.remove_watch(this._watch);
        this._watch = 0;
        for (const [object, id] of this._signals ?? []) object.disconnect(id);
        this._signals = [];
    }

    _watchInput() {
        if (this._inputFilter != null) return;
        if (!this._enabled || this._watch) return;
        this._watch = this._idle.add_user_active_watch(() => {
            this._watch = 0;
            this._activity();
        });
    }

    _activity() {
        const notify = this._hidden || this._reservation;
        this._lastInput = GLib.get_monotonic_time();
        this._serial++;
        this._restore();
        // Only activity, never key codes or text, crosses the local socket.
        if (this._connection && notify) this._send({event: 'activity'});
    }

    _inputEvent(event) {
        const type = event.type();
        if ([Clutter.EventType.NOTHING, Clutter.EventType.ENTER, Clutter.EventType.LEAVE].includes(type)) return;
        const device = event.get_source_device();
        // Mutter's own warp uses the core pointer with timestamp zero.
        if (type === Clutter.EventType.MOTION && device === this._seat.get_pointer() && event.get_time() === 0) return;
        const armed = this._armed;
        const now = GLib.get_monotonic_time();
        if (type === Clutter.EventType.MOTION && armed && now < armed.until &&
            armed.nodes.has(device?.get_device_node?.()) && !this._buttons(event.get_state())) {
            // Ignored only for visibility, never for the no-input rollback guard.
            this._serial++;
            return;
        }
        this._activity();
    }

    _buttons(mask) {
        return mask & (Clutter.ModifierType.BUTTON1_MASK | Clutter.ModifierType.BUTTON2_MASK |
            Clutter.ModifierType.BUTTON3_MASK | Clutter.ModifierType.BUTTON4_MASK | Clutter.ModifierType.BUTTON5_MASK);
    }

    _cancelArm() {
        if (this._armTimer) GLib.Source.remove(this._armTimer);
        this._armTimer = 0;
        this._armed = null;
    }

    _armDeparture(c) {
        this._restore();
        if (this._inputFilter == null || typeof c.id !== 'string' || c.id.length > 32 ||
            !Array.isArray(c.nodes) || c.nodes.length > 16 ||
            !c.nodes.every(n => typeof n === 'string' && /^\/dev\/input\/event\d+$/.test(n))) return;
        const now = GLib.get_monotonic_time();
        this._armed = {id: c.id, nodes: new Set(c.nodes), started: now, until: now + 1000000};
        this._armTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1000, () => {
            this._armTimer = 0; this._armed = null; return GLib.SOURCE_REMOVE;
        });
    }

    _restore() {
        this._cancelArm();
        if (!this._hidden) return;
        this._hidden = false;
        if (this._inhibited) {
            this._tracker.uninhibit_cursor_visibility();
            this._inhibited = false;
        } else {
            this._tracker.set_pointer_visible(true);
        }
        this._send({event: 'cursor', hidden: false});
    }

    _hide(written, armId = null) {
        const writtenUs = Number(written) / 1000;
        const now = GLib.get_monotonic_time();
        const lastActivity = now - this._idle.get_idletime() * 1000;
        if (this._hidden) return;
        if (armId == null) this._cancelArm();
        const armed = armId == null ? null : this._armed;
        if (armId != null && (!armed || armed.id !== armId || now >= armed.until ||
            writtenUs < armed.started || now >= writtenUs + 250000)) {
            this._cancelArm();
            this._send({event: 'cursor', hidden: false, reason: 'Departure cancelled by input or expired'}); return;
        }
        if (!Number.isFinite(writtenUs) || writtenUs > now || now - writtenUs > 1000000) {
            this._cancelArm();
            this._send({event: 'cursor', hidden: false, reason: 'Hide request expired or invalid'}); return;
        }
        if (this._lastInput > writtenUs || (this._inputFilter == null && lastActivity + 1000 > writtenUs)) {
            this._cancelArm();
            this._send({event: 'cursor', hidden: false, reason: 'Local input since mouse switch write'}); return;
        }
        if (this._tracker.inhibit_cursor_visibility && this._tracker.uninhibit_cursor_visibility) {
            this._tracker.inhibit_cursor_visibility();
            this._inhibited = true;
        } else if (this._tracker.set_pointer_visible) {
            this._tracker.set_pointer_visible(false);
        } else {
            this._cancelArm();
            this._send({event: 'cursor', hidden: false, reason: 'Cursor visibility API unavailable'}); return;
        }
        this._hidden = true;
        if (armed) {
            if (this._armTimer) GLib.Source.remove(this._armTimer);
            armed.until = writtenUs + 250000;
            this._armTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, Math.max(1, Math.ceil((armed.until - now) / 1000)), () => {
                this._armTimer = 0;
                if (this._armed === armed && this._hidden) {
                    this._armed = null;
                    // Some Mutter paths reveal the cursor on motion themselves.
                    // Seal this single departure once, never force visibility continuously.
                    if (!this._inhibited) this._tracker.set_pointer_visible(false);
                }
                return GLib.SOURCE_REMOVE;
            });
        }
        this._watchInput();
        this._send({event: 'cursor', hidden: true});
    }

    _endpoint() {
        const uid = Gio.Credentials.new().get_unix_user();
        try {
            const [, bytes] = GLib.file_get_contents('/etc/easy-switch-link/policy.json');
            const policy = JSON.parse(decoder.decode(bytes));
            if (policy.login_handoffs && [policy.owner_uid, policy.greeter_uid].includes(uid))
                return ['/run/easy-switch-link/peer/desktop.sock', policy.peer_uid];
        } catch (_) { /* User-only mode. */ }
        if (Main.sessionMode.isGreeter || Main.sessionMode.isLocked) return null;
        return [`${GLib.get_user_runtime_dir()}/easy-switch-link-gnome.sock`, uid];
    }

    _connect() {
        if (!this._enabled || this._connecting || this._connection) return;
        const endpoint = this._endpoint();
        if (!endpoint) return;
        this._connecting = true;
        const cancellable = new Gio.Cancellable();
        this._cancellable = cancellable;
        // Direct nonblocking Unix sockets avoid proxy/network-monitor initialization
        // inside Shell's main loop. No Internet connection is made here.
        let socket;
        try {
            socket = Gio.Socket.new(Gio.SocketFamily.UNIX, Gio.SocketType.STREAM, Gio.SocketProtocol.DEFAULT);
            socket.set_blocking(false);
            socket.connect(Gio.UnixSocketAddress.new(endpoint[0]), cancellable);
            const connection = new Gio.UnixConnection({socket});
            this._connecting = false;
            if (!this._enabled || cancellable.is_cancelled()) { connection.close(null); return; }
            if (connection.get_socket().get_credentials().get_unix_user() !== endpoint[1]) {
                connection.close(null); throw new Error('Unexpected companion server identity');
            }
            this._connection = connection;
            this._input = connection.get_input_stream();
            this._output = connection.get_output_stream();
            this._delay = 1;
            this._readLoop(connection).catch(() => this._lost(connection));
        } catch (_) {
            try { socket?.close(); } catch (_) { /* Already closed. */ }
            this._connecting = false; this._schedule();
        }
    }

    _schedule() {
        if (!this._enabled || this._retry) return;
        this._retry = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, this._delay, () => {
            this._retry = 0; this._connect(); return GLib.SOURCE_REMOVE;
        });
        this._delay = Math.min(this._delay * 2, 30);
    }

    _disconnect() {
        this._restore(); this._clearBarriers(); this._reservation = null;
        this._cancellable?.cancel();
        const connection = this._connection;
        this._connection = null; this._queue = [];
        try { connection?.close(null); } catch (_) { /* Already closed. */ }
    }

    _lost(connection) {
        if (connection !== this._connection) return;
        this._disconnect(); this._schedule();
    }

    async _readExactly(input, count) {
        const bytes = new Uint8Array(count);
        let offset = 0;
        while (offset < count) {
            const part = await new Promise((resolve, reject) => input.read_bytes_async(
                count - offset, GLib.PRIORITY_DEFAULT, this._cancellable, (stream, result) => {
                    try { resolve(stream.read_bytes_finish(result).get_data()); } catch (e) { reject(e); }
                }));
            if (!part.length) throw new Error('Companion disconnected');
            bytes.set(part, offset); offset += part.length;
        }
        return bytes;
    }

    async _readLoop(connection) {
        const input = connection.get_input_stream();
        while (connection === this._connection) {
            const header = await this._readExactly(input, 4);
            const size = new DataView(header.buffer).getUint32(0, true);
            if (!size || size > LIMIT) throw new Error('Invalid companion frame');
            const command = JSON.parse(decoder.decode(await this._readExactly(input, size)));
            if (connection !== this._connection) return;
            await this._command(command);
        }
    }

    _send(value) {
        if (!this._connection) return;
        if (this._queue.length >= 64) { this._lost(this._connection); return; }
        const data = encoder.encode(JSON.stringify(value));
        if (data.length > LIMIT) { this._lost(this._connection); return; }
        const frame = new Uint8Array(data.length + 4);
        new DataView(frame.buffer).setUint32(0, data.length, true); frame.set(data, 4);
        this._queue.push(frame); this._flush();
    }

    _flush() {
        if (this._writing || !this._connection || !this._queue.length) return;
        this._writing = true;
        const connection = this._connection;
        this._output.write_all_async(this._queue.shift(), GLib.PRIORITY_DEFAULT, this._cancellable, (stream, result) => {
            this._writing = false;
            try { stream.write_all_finish(result); this._flush(); }
            catch (_) { this._lost(connection); }
        });
    }

    async _layout() {
        this._restore(); this._clearBarriers();
        this._reservation = null;
        if (!this._connection) return;
        const connection = this._connection;
        const generation = this._layoutGeneration = (this._layoutGeneration ?? 0) + 1;
        try {
            const value = await new Promise((resolve, reject) => Gio.DBus.session.call(
                'org.gnome.Mutter.DisplayConfig', '/org/gnome/Mutter/DisplayConfig',
                'org.gnome.Mutter.DisplayConfig', 'GetCurrentState', null, null,
                Gio.DBusCallFlags.NONE, 2000, this._cancellable, (bus, result) => {
                    try { resolve(bus.call_finish(result).deep_unpack()); } catch (e) { reject(e); }
                }));
            const [, , logical] = value;
            const displays = [];
            for (const [x, y, scale, transform, , specs] of logical) {
                // Mirrored groups and missing serials cannot establish a unique stable display.
                if (specs.length !== 1 || !specs[0][3]) throw new Error('Ambiguous display identity');
                const monitor = Main.layoutManager.monitors.find(m => m.x === x && m.y === y);
                if (!monitor) throw new Error('Logical display unavailable');
                const id = `gnome:${JSON.stringify(specs[0])}`;
                displays.push({id, name: specs[0][0], x, y, width: monitor.width, height: monitor.height,
                    scale: 1, binding: `${id}:${x},${y},${monitor.width},${monitor.height},${scale},${transform}`});
            }
            if (connection !== this._connection || generation !== this._layoutGeneration) return;
            this._send({event: 'layout', version: 1, displays,
                input_source: this._inputFilter != null,
                cursor: !!(this._tracker.set_pointer_visible || this._tracker.inhibit_cursor_visibility)});
        } catch (_) {
            if (connection === this._connection && generation === this._layoutGeneration)
                this._send({event: 'layout', version: 1, displays: [], cursor: false, input_source: false});
        }
    }

    _clearBarriers() {
        this._placementWait?.(false);
        this._release();
        for (const barrier of this._barriers ?? []) barrier.destroy();
        this._barriers = [];
    }

    _release() {
        if (this._captureTimer) GLib.Source.remove(this._captureTimer);
        this._captureTimer = 0;
        if (this._hit) { this._hit[0].release(this._hit[1]); this._hit = null; }
    }

    _configure(c) {
        this._restore(); this._clearBarriers();
        this._reservation = null;
        if (!c.enabled) return;
        const hasBackend = Meta.Barrier.list_properties().some(p => p.name === 'backend');
        for (const rule of c.rules.filter(r => r.enabled)) {
            const s = rule.source;
            const d = c.displays.find(v => v.id === s.display && v.binding === s.binding);
            if (!d) continue;
            let x1, y1, x2, y2, directions;
            if (s.edge === 'left' || s.edge === 'right') {
                x1 = x2 = s.edge === 'left' ? d.x : d.x + d.width;
                y1 = Math.round(d.y + d.height * s.start / 100); y2 = Math.round(d.y + d.height * s.end / 100);
                directions = s.edge === 'left' ? Meta.BarrierDirection.POSITIVE_X : Meta.BarrierDirection.NEGATIVE_X;
            } else {
                y1 = y2 = s.edge === 'top' ? d.y : d.y + d.height;
                x1 = Math.round(d.x + d.width * s.start / 100); x2 = Math.round(d.x + d.width * s.end / 100);
                directions = s.edge === 'top' ? Meta.BarrierDirection.POSITIVE_Y : Meta.BarrierDirection.NEGATIVE_Y;
            }
            if (x1 === x2 && y1 === y2) continue;
            const barrier = new Meta.Barrier({...(hasBackend ? {backend: global.backend} : {display: global.display}), x1, y1, x2, y2, directions});
            barrier.connect('hit', (b, event) => {
                const [, , mask] = global.get_pointer();
                const buttons = !!(mask & (Clutter.ModifierType.BUTTON1_MASK | Clutter.ModifierType.BUTTON2_MASK |
                    Clutter.ModifierType.BUTTON3_MASK | Clutter.ModifierType.BUTTON4_MASK | Clutter.ModifierType.BUTTON5_MASK));
                this._send({event: 'motion', point: {x: event.x, y: event.y},
                    delta: {x: event.dx, y: event.dy}, buttons, synthetic: false});
                this._hit = [b, event];
                if (buttons) this._release();
                else if (!this._captureTimer) this._captureTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 200, () => {
                    this._captureTimer = 0; this._release(); return GLib.SOURCE_REMOVE;
                });
            });
            barrier.connect('left', () => this._send({event: 'motion', point: {x: global.get_pointer()[0], y: global.get_pointer()[1]}, delta: {x: 0, y: 0}, buttons: false, synthetic: false}));
            this._barriers.push(barrier);
        }
    }

    _placePointer(point, deadline) {
        // Native Mutter queues warps on its input thread. Checking synchronously
        // sees the old position; observe completion without polling or re-warping.
        return new Promise(resolve => {
            let signal = 0;
            let timer = 0;
            let finished = false;
            const track = !!(this._tracker.track_position && this._tracker.untrack_position);
            const finish = ok => {
                if (finished) return;
                finished = true;
                if (signal) this._tracker.disconnect(signal);
                if (track) this._tracker.untrack_position();
                if (timer) GLib.Source.remove(timer);
                if (this._placementWait === finish) this._placementWait = null;
                resolve(ok && GLib.get_monotonic_time() < deadline);
            };
            const check = () => {
                const after = global.get_pointer();
                if (Math.abs(after[0] - point.x) <= 2 && Math.abs(after[1] - point.y) <= 2)
                    finish(true);
            };
            this._placementWait?.(false);
            this._placementWait = finish;
            signal = this._tracker.connect('position-invalidated', check);
            if (track) this._tracker.track_position();
            timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, Math.max(1,
                Math.ceil((deadline - GLib.get_monotonic_time()) / 1000)), () => {
                timer = 0; finish(false); return GLib.SOURCE_REMOVE;
            });
            try {
                Clutter.get_default_backend().get_default_seat().warp_pointer(Math.round(point.x), Math.round(point.y));
                check(); // X11 may complete synchronously.
            } catch (_) { finish(false); }
        });
    }

    async _command(c) {
        switch (c.command) {
        case 'hello':
            if (c.version !== 1) throw new Error('Incompatible companion protocol');
            this._restore(); await this._layout(); break;
        case 'configure': this._configure(c); break;
        case 'restore': this._restore(); break;
        case 'release': this._release(); break;
        case 'arm': this._armDeparture(c); break;
        case 'hide': this._hide(c.written_ns, c.arm); break;
        case 'place': {
            const connection = this._connection;
            this._restore(); const point = c.point;
            const before = global.get_pointer();
            const buttons = before[2] & (Clutter.ModifierType.BUTTON1_MASK | Clutter.ModifierType.BUTTON2_MASK |
                Clutter.ModifierType.BUTTON3_MASK | Clutter.ModifierType.BUTTON4_MASK | Clutter.ModifierType.BUTTON5_MASK);
            const deadline = Number(c.deadline_ns) / 1000;
            let valid = Number.isFinite(deadline) && GLib.get_monotonic_time() < deadline &&
                deadline - GLib.get_monotonic_time() <= 500000 && !buttons && [point.x, point.y].every(Number.isFinite) && Main.layoutManager.monitors.some(m =>
                point.x >= m.x && point.y >= m.y && point.x < m.x + m.width && point.y < m.y + m.height);
            if (valid) {
                this._reservation = {id: c.id, serial: this._serial, previous: before, until: GLib.get_monotonic_time() + 5000000};
                this._watchInput();
                valid = await this._placePointer(point, deadline);
            }
            if (connection === this._connection)
                this._send({event: 'placed', id: c.id, ok: valid});
            break;
        }
        case 'cancel':
            if (this._reservation?.id !== c.id) break;
            if (this._reservation?.id === c.id && this._reservation.serial === this._serial &&
                GLib.get_monotonic_time() < this._reservation.until)
                Clutter.get_default_backend().get_default_seat().warp_pointer(this._reservation.previous[0], this._reservation.previous[1]);
            this._reservation = null; break;
        case 'complete': if (this._reservation?.id === c.id) this._reservation = null; break;
        default: throw new Error('Unknown companion command');
        }
    }
}
