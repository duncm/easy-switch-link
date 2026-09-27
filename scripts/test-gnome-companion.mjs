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
let deferredWarp = false;
let pendingWarp;
let nextSource = 0;
const timers = new Map();
const signals = new Map();
let tracking = 0;
const context = vm.createContext({
    TextDecoder, TextEncoder, Uint8Array, DataView, Extension: class {},
    GLib: {get_monotonic_time: () => now, Source: {remove(id) {timers.delete(id);}},
        timeout_add(_priority, _delay, callback) {const id = ++nextSource; timers.set(id, callback); return id;}},
    Gio: {}, Meta: {}, Main: {layoutManager: {monitors: [{x: 0, y: 0, width: 1920, height: 1080}]}},
    Clutter: {ModifierType: {BUTTON1_MASK: 1, BUTTON2_MASK: 2, BUTTON3_MASK: 4, BUTTON4_MASK: 8, BUTTON5_MASK: 16},
        get_default_backend: () => ({get_default_seat: () => ({warp_pointer(x, y) {
            warps.push([x,y]);
            if (deferredWarp) pendingWarp = [x,y,0]; else point = [x, y, 0];
        }})})},
    global: {get_pointer: () => point},
});
vm.runInContext(source, context);
const companion = new context.Companion();
const notices = [];
companion._send = value => notices.push(value);
companion._tracker = {inhibit_cursor_visibility() {visibility.push(false);}, uninhibit_cursor_visibility() {visibility.push(true);},
    connect(name, callback) {assert.equal(name,'position-invalidated'); const id=++nextSource; signals.set(id,callback); return id;},
    track_position() {tracking++;}, untrack_position() {tracking--;},
    disconnect(id) {signals.delete(id);}};
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
deferredWarp = true;
let completed = false;
const asyncPlace = companion._command({command:'place',id:6,point:{x:8,y:450},deadline_ns:'2300000000'})
    .then(() => {completed = true;});
await Promise.resolve();
assert.equal(completed,false,'Wayland warp must wait for compositor acknowledgement');
point = pendingWarp;
for (const callback of [...signals.values()]) callback();
await asyncPlace;
assert.equal(notices.at(-1).ok,true);
assert.equal(signals.size,0);
assert.equal(timers.size,0,'successful warp removes its deadline timer');
assert.equal(tracking,0,'completion releases explicit position tracking');
assert.deepEqual(warps.at(-1),[8,450]);

const confined = companion._command({command:'place',id:7,point:{x:8,y:500},deadline_ns:'2300000000'});
for (const callback of [...signals.values()]) callback(); // Pointer did not reach the target.
for (const [id,callback] of [...timers]) {timers.delete(id); callback();}
await confined;
assert.equal(notices.at(-1).ok,false,'confinement must not dispatch hardware');
assert.equal(signals.size,0,'timeout disconnects position listener');
assert.equal(tracking,0);

const interrupted = companion._command({command:'place',id:8,point:{x:8,y:600},deadline_ns:'2300000000'});
companion._clearBarriers();
await interrupted;
assert.equal(notices.at(-1).ok,false,'session/topology/disconnect cancels pending placement');
assert.equal(signals.size,0);
assert.equal(timers.size,0);
assert.equal(tracking,0);
const noticeCount = notices.length;
const stalePlacement = companion._command({command:'place',id:9,point:{x:8,y:600},deadline_ns:'2300000000'});
companion._connection = {};
companion._clearBarriers();
await stalePlacement;
assert.equal(notices.length,noticeCount,'old placement cannot acknowledge to a replacement controller');
assert.equal(tracking,0);
assert.equal(timers.size,0);
companion._reservation = {id:10};
await companion._command({command:'cancel',id:9});
assert.equal(companion._reservation.id,10,'old cancellation cannot clear a newer reservation');
await companion._command({command:'complete',id:9});
assert.equal(companion._reservation.id,10,'old completion cannot clear a newer reservation');
deferredWarp = false;
await companion._command({command:'place',id:4,point:{x:99999,y:0},deadline_ns:'2300000000'});
assert.equal(notices.at(-1).ok,false,'missing display prevents placement');
now += 6_000_000;
await companion._command({command:'place',id:5,point:{x:8,y:400},deadline_ns:'2300000000'});
assert.equal(notices.at(-1).ok,false,'expired placement must not warp');
companion._hide('1950000000');
assert.equal(companion._hidden,false,'old writes cannot hide the cursor');
await assert.rejects(companion._command({command:'execute'}),/Unknown/);
// The receiver-level exception is bounded by the successful write, not delivery.
context.Clutter.EventType = {NOTHING:0, ENTER:1, LEAVE:2, MOTION:3, BUTTON_PRESS:4,
    KEY_PRESS:5, SCROLL:6, DEVICE_REMOVED:7};
const corePointer = {};
companion._seat = {get_pointer: () => corePointer};
companion._inputFilter = 1;
companion._connection = {};
companion._tracker = {set_pointer_visible(v) {visibility.push(v);}};
companion._inhibited = false;
const motion = (node, type=3, buttons=0) => ({type:()=>type, get_source_device:()=>({get_device_node:()=>node}), get_state:()=>buttons, get_time:()=>1});
const arm = id => {
    now += 1_000_000;
    companion._armDeparture({id,nodes:['/dev/input/event2']});
    now += 30_000;
};
arm('20');
companion._reservation = {id:50,serial:companion._serial,previous:[100,100],until:now+1_000_000};
const beforeTailWarp=warps.length;
companion._inputEvent(motion('/dev/input/event2'));
await companion._command({command:'cancel',id:50});
assert.equal(warps.length,beforeTailWarp,'tail motion still defeats cancellation rollback');
companion._hide(String(now*1000),'20');
assert.equal(companion._hidden,true,'receiver tail before acknowledgement does not defeat the hide');
now += 10_000;
companion._inputEvent(motion('/dev/input/event2'));
assert.equal(companion._hidden,true,'receiver tail after acknowledgement does not restore');
now += 250_000;
companion._inputEvent(motion('/dev/input/event2'));
assert.equal(companion._hidden,false,'receiver motion restores outside the 250 ms window even if timer was delayed');
assert.equal(timers.size,0);
for (const event of [motion('/dev/input/event9'), motion(null), motion('/dev/input/event2',4),
    motion('/dev/input/event1',5),motion('/dev/input/event2',6),motion('/dev/input/event2',3,1),motion('/dev/input/event2',7)]) {
    arm('21');
    companion._hide(String(now*1000),'21');
    now += 1000;
    companion._inputEvent(event);
    assert.equal(companion._hidden,false,'unknown/virtual input, clicks, keys, scroll, buttons, unplug restore immediately');
    assert.equal(timers.size,0,'restoration must cancel the delayed final hide');
    companion._hide(String((now-1000)*1000),'21');
    assert.equal(companion._hidden,false,'late write notification cannot resurrect a cancelled departure');
}
arm('22');
companion._inputEvent(motion('/dev/input/event9'));
companion._hide(String(now*1000),'22');
assert.equal(companion._hidden,false,'unknown input before HID acknowledgement invalidates the arm');
arm('23');
const written=now;
now += 251_000;
companion._hide(String(written*1000),'23');
assert.equal(companion._hidden,false,'delayed acknowledgements cannot extend the grace window');
arm('24');
companion._hide(String(now*1000),'24');
const hides=visibility.length;
now += 250_000;
for (const [id,callback] of [...timers]) {timers.delete(id); callback();}
assert.equal(visibility.length,hides+1,'one final hide seals the departure after the tail');
assert.equal(companion._armed,null);
assert.equal(timers.size,0,'no idle cursor polling or recurring hide timer');
companion._inputEvent(motion('/dev/input/event9'));
assert.equal(companion._hidden,false);
arm('25');
companion._restore();
companion._hide(String(now*1000),'25');
assert.equal(companion._hidden,false,'arrival/pause/disconnect cleanup cancels the arm');
arm('26');
companion._hide(String(now*1000),'wrong');
assert.equal(companion._hidden,false,'stale transaction cannot hide');
companion._armDeparture({id:'27',nodes:['/tmp/unknown']});
assert.equal(companion._armed,null,'only validated input-event node paths are accepted');
companion._inputFilter=null;
companion._armDeparture({id:'28',nodes:['/dev/input/event2']});
assert.equal(companion._armed,null,'unsupported input attribution retains strict restoration');
// Register the production observer with the native two-argument GJS signature.
let filterCallback;
let filterRemoved = false;
context.Clutter.EVENT_PROPAGATE = false;
context.Clutter.Event = {
    add_filter(...args) {assert.equal(args.length,2); filterCallback=args[1]; return 42;},
    remove_filter(id) {assert.equal(id,42); filterRemoved=true;},
};
context.Meta.is_wayland_compositor = () => true;
context.Main.sessionMode = {connect:()=>1,disconnect() {}};
context.Main.layoutManager.connect = () => 1;
context.Main.layoutManager.disconnect = () => {};
context.global.backend = {get_cursor_tracker:()=>({set_pointer_visible() {}}),
    get_core_idle_monitor:()=>({})};
const observer = new context.Companion();
observer._connect = () => {};
observer.enable();
assert.equal(observer._inputFilter,42);
const key = motion('/dev/input/event1',5);
key.get_key_code = () => assert.fail('Key contents inspected');
assert.equal(filterCallback(key),false,'input observer must propagate all events');
observer.disable();
assert.equal(filterRemoved,true,'disable removes the observer');

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
