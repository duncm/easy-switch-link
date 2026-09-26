// Run with gjs and the installed Mutter typelib/library directories in the environment.
// This checks API availability only; it does not attach to or mutate a desktop.
const {Meta, Clutter} = imports.gi;
const methods = (type, names) => Object.fromEntries(names.map(name => [name, typeof type.prototype[name] === 'function']));
print(JSON.stringify({
    cursor: methods(Meta.CursorTracker, ['set_pointer_visible', 'inhibit_cursor_visibility', 'uninhibit_cursor_visibility']),
    seat: methods(Clutter.Seat, ['warp_pointer']),
    barrier: Meta.Barrier.list_properties().map(p => p.name),
    idle: methods(Meta.IdleMonitor, ['add_user_active_watch', 'get_idletime', 'remove_watch']),
}));
