import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

// Exercise the production companion's state machine against compositor doubles.
let source = fs.readFileSync(new URL('../packaging/gnome/easy-switch-link@duncm.github.com/extension.js', import.meta.url), 'utf8');
source = source.replace(/^import .*;\r?\n/gm, '').replace('export default class EasySwitchLink', 'globalThis.Companion = class EasySwitchLink');
let now = 2_000_000;
let point = [200, 200, 0];
let warps = [];
let visibility = [];
const context = vm.createContext({
    TextDecoder, TextEncoder, Uint8Array, DataView, Extension: class {},
    GLib: {get_monotonic_time: () => now, Source: {remove() {}}},
    Gio: {}, Meta: {}, Main: {layoutManager: {monitors: [{x: 0, y: 0, width: 1920, height: 1080}]}},
    Clutter: {ModifierType: {BUTTON1_MASK: 1, BUTTON2_MASK: 2, BUTTON3_MASK: 4, BUTTON4_MASK: 8, BUTTON5_MASK: 16},
        get_default_backend: () => ({get_default_seat: () => ({warp_pointer(x, y) { point = [x, y, 0]; warps.push([x,y]); }})})},
    global: {get_pointer: () => point},
});
vm.runInContext(source, context);
const companion = new context.Companion();
const notices = [];
companion._send = value => notices.push(value);
companion._tracker = {inhibit_cursor_visibility() {visibility.push(false);}, uninhibit_cursor_visibility() {visibility.push(true);}};
companion._idle = {get_idletime: () => 100};
companion._lastInput = 0;
companion._serial = 0;
companion._hide('1950000000');
assert.equal(companion._hidden, true);
companion._activity();
assert.equal(companion._hidden, false);
companion._hide('1950000000');
assert.equal(companion._hidden, false, 'late acknowledgement cannot hide after local input');
assert.deepEqual(visibility, [false, true]);
await companion._command({command:'place', id:1, point:{x:8,y:350},deadline_ns:'2300000000'});
assert.equal(notices.at(-1).ok, true);
await companion._command({command:'cancel',id:1});
assert.deepEqual(warps.at(-1), [200,200]);
await companion._command({command:'place',id:2,point:{x:8,y:400},deadline_ns:'2300000000'});
companion._activity();
const before = warps.length;
await companion._command({command:'cancel',id:2});
assert.equal(warps.length,before,'cancellation cannot override intervening input');
point[2] = 1;
await companion._command({command:'place',id:3,point:{x:8,y:400},deadline_ns:'2300000000'});
assert.equal(notices.at(-1).ok,false,'held buttons prevent placement');
point[2] = 0;
await companion._command({command:'place',id:4,point:{x:99999,y:0},deadline_ns:'2300000000'});
assert.equal(notices.at(-1).ok,false,'missing display prevents placement');
now += 6_000_000;
await companion._command({command:'place',id:5,point:{x:8,y:400},deadline_ns:'2300000000'});
assert.equal(notices.at(-1).ok,false,'expired placement must not warp');
companion._hide('1950000000');
assert.equal(companion._hidden,false,'old writes cannot hide the cursor');
await assert.rejects(companion._command({command:'execute'}),/Unknown/);
// A read completing after controller replacement cannot apply its old command.
const oldConnection = {get_input_stream: () => ({})};
const commandBytes = new TextEncoder().encode(JSON.stringify({command:'hide',written_ns:'1'}));
const header = new Uint8Array(4);
new DataView(header.buffer).setUint32(0,commandBytes.length,true);
let read = 0;
companion._connection = oldConnection;
companion._readExactly = async () => {
    if (++read === 1) return header;
    companion._connection = {};
    return commandBytes;
};
companion._command = async () => assert.fail('Stale connection command executed');
await companion._readLoop(oldConnection);
console.log('GNOME companion: hide/input races, placement, cancellation, held buttons, bounds and stale messages passed');
