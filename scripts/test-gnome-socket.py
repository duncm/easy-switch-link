#!/usr/bin/env python3
"""Exercise production GJS framing and credentials with real Gio Unix sockets.

Only compositor objects are substituted; no live desktop or receiver is used.
"""
import json
import os
from pathlib import Path
import re
import socket
import struct
import subprocess
import tempfile
import time

root = Path(__file__).resolve().parent.parent
source = (root/'packaging/gnome/easy-switch-link@duncm.github.com/extension.js').read_text()
source = re.sub(r'^import .*;\n', '', source, flags=re.M).replace('export default class EasySwitchLink', 'class EasySwitchLink')
prefix = '''import Gio from 'gi://Gio'; import GLib from 'gi://GLib';
class Extension {}
const Main={sessionMode:{isGreeter:false,isLocked:false}};
const Clutter={},Meta={};
'''
suffix = '''
let finish;
const done=new Promise(resolve=>{finish=resolve;});
const c=new EasySwitchLink();
c._enabled=true;c._queue=[];c._delay=1;c._lastInput=0;c._serial=0;c._barriers=[];
c._tracker={set_pointer_visible(_visible){}};
c._idle={get_idletime(){return 10000;},add_user_active_watch(){return 1;},remove_watch(){}};
c._layout=async()=>c._send({event:'layout',version:1,displays:[],cursor:true});
c._endpoint=()=>[GLib.get_user_runtime_dir()+'/easy-switch-link-gnome.sock',Gio.Credentials.new().get_unix_user()];
c._schedule=()=>finish();
c._connect();
GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT,5,()=>{finish();return GLib.SOURCE_REMOVE;});
await done;
c._enabled=false;c._disconnect();
'''
with tempfile.TemporaryDirectory(prefix='esl-gjs-socket-') as tmp:
    script = Path(tmp)/'client.js'
    script.write_text(prefix+source+suffix)
    server = socket.socket(socket.AF_UNIX)
    server.bind(str(Path(tmp)/'easy-switch-link-gnome.sock'))
    server.listen(1)
    server.settimeout(5)
    process = subprocess.Popen(['gjs','-m',str(script)],env={**os.environ,'XDG_RUNTIME_DIR':tmp},stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    try:
        conn, _ = server.accept()
        conn.settimeout(5)
        def send(value):
            data=json.dumps(value).encode()
            conn.sendall(struct.pack('<I',len(data))+data)
        def exact(n):
            result=b''
            while len(result)<n:
                data=conn.recv(n-len(result))
                if not data: raise RuntimeError('GJS disconnected before replying')
                result+=data
            return result
        def receive():
            n=struct.unpack('<I',exact(4))[0]
            assert 0<n<=65536
            return json.loads(exact(n))
        send(dict(command='hello',version=1))
        assert receive()['event']=='layout'
        send(dict(command='hide',written_ns=str(time.monotonic_ns()-50_000_000)))
        assert receive()==dict(event='cursor',hidden=True)
        send(dict(command='restore'))
        assert receive()==dict(event='cursor',hidden=False)
        conn.close()
        out,err=process.communicate(timeout=8)
        assert process.returncode==0,err.decode()
        print('Native GJS: credential check, framed hello, hide, restore and disconnect passed')
    finally:
        if process.poll() is None:
            process.kill()
        out,err=process.communicate()
        if err: print(err.decode())
        server.close()
