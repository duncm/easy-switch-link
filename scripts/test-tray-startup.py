#!/usr/bin/env python3
"""Exercise the real tray on a private bus in a hardware-free disposable root.

ESL_DISPOSABLE_ROOT=1 dbus-run-session -- python3 scripts/test-tray-startup.py EXE
Requires python3-gi; never uses the live desktop's tray watcher.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import gi
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib

assert os.environ.get('ESL_DISPOSABLE_ROOT') == '1'
assert not Path('/etc/easy-switch-link/policy.json').exists(), 'Do not test against system-mode owners'
assert not list(Path('/dev').glob('hidraw*')), 'Test root must not expose physical receivers'
executable = str(Path(sys.argv[1]).resolve())
WATCHER = 'org.kde.StatusNotifierWatcher'
PATH = '/StatusNotifierWatcher'
XML = '''<node><interface name="org.kde.StatusNotifierWatcher">
<method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
<method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
<property name="RegisteredStatusNotifierItems" type="as" access="read"/>
<property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
<property name="ProtocolVersion" type="i" access="read"/>
</interface></node>'''


def connection():
    return Gio.DBusConnection.new_for_address_sync(os.environ['DBUS_SESSION_BUS_ADDRESS'],
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)


class Watcher:
    def __init__(self):
        self.items = []
        self.bus = connection()
        info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
        self.object = self.bus.register_object(PATH, info, self.call, self.get, None)
        result = self.bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
            'org.freedesktop.DBus', 'RequestName', GLib.Variant('(su)', (WATCHER, 4)),
            None, Gio.DBusCallFlags.NONE, 2000, None)
        assert result.unpack() == (1,), 'Private watcher name already owned'

    def call(self, bus, sender, path, interface, method, args, invocation):
        if method == 'RegisterStatusNotifierItem':
            self.items.append(args.unpack()[0])
        invocation.return_value(GLib.Variant('()', ()))

    def get(self, bus, sender, path, interface, prop):
        return {
            'RegisteredStatusNotifierItems': GLib.Variant('as', self.items),
            'IsStatusNotifierHostRegistered': GLib.Variant('b', True),
            'ProtocolVersion': GLib.Variant('i', 0),
        }[prop]

    def close(self):
        self.bus.close_sync(None)


with tempfile.TemporaryDirectory(prefix='esl-tray-test-') as directory:
    log_path = Path(directory) / 'tray.log'
    with log_path.open('wb') as log:
        process = subprocess.Popen([executable, '--config-dir', directory, 'run'],
            stdout=log, stderr=log)
        watcher = None
        try:
            def logs():
                return log_path.read_text() + ''.join(p.read_text() for p in
                    (Path(directory) / 'logs').glob('easy-switch-link*'))

            def wait_for(predicate):
                deadline = time.monotonic() + 6
                while time.monotonic() < deadline:
                    while GLib.MainContext.default().pending():
                        GLib.MainContext.default().iteration(False)
                    assert process.poll() is None, logs()
                    if predicate():
                        return
                    time.sleep(0.02)
                raise AssertionError('Tray deadline expired: ' + logs())

            wait_for(lambda: 'waiting for desktop tray support' in logs())
            # Start the watcher only after the real app encountered its absence.
            watcher = Watcher()
            wait_for(lambda: len(watcher.items) == 1)
            item = watcher.items[0]
            title = watcher.bus.call_sync(item, '/StatusNotifierItem',
                'org.freedesktop.DBus.Properties', 'Get',
                GLib.Variant('(ss)', ('org.kde.StatusNotifierItem', 'Title')),
                None, Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
            assert title == 'Easy-Switch Link', title
            watcher.close()
            wait_for(lambda: logs().count('waiting for desktop tray support') >= 2)
            watcher = Watcher()
            wait_for(lambda: watcher.items == [item])
            print('Real tray: late watcher registration and watcher restart passed; same app and tray owner')
        finally:
            if watcher:
                watcher.close()
            process.terminate()
            process.wait(timeout=15)
