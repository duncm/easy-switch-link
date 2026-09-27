#!/usr/bin/env bash
# Run only inside a disposable build root, with its own /etc and /var/lib.
set -euo pipefail
test "${ESL_DISPOSABLE_ROOT:-}" = 1 || { echo 'Set ESL_DISPOSABLE_ROOT=1 in an isolated test root' >&2; exit 1; }
cd /work
export DEBIAN_FRONTEND=noninteractive
package=dist/easy-switch-link_0.3.4_amd64.deb
if test -f dist/previous.deb; then apt-get install --allow-downgrades -y ./dist/previous.deb; fi
apt-get install -y "./$package"
apt-get install --reinstall -y "./$package"
easy-switch-link --version | grep '0.3.4'
test ! -f /etc/easy-switch-link/policy.json
test ! -L /etc/systemd/system/multi-user.target.wants/easy-switch-link-hardware.service
easy-switch-link --config-dir /tmp/esl-user-smoke run --headless > /tmp/esl-user-smoke.log 2>&1 &
user_monitor=$!
trap 'kill "$user_monitor" 2>/dev/null || true' EXIT
sleep 1
easy-switch-link --config-dir /tmp/esl-user-smoke status --json > /tmp/esl-user-smoke-status.json
easy-switch-link --config-dir /tmp/esl-user-smoke quit
wait "$user_monitor"
trap - EXIT
systemd-analyze verify /usr/lib/systemd/system/easy-switch-link-hardware.service /usr/lib/systemd/system/easy-switch-link-peer.service
getent passwd esl-smoke >/dev/null || useradd -m -u 21000 esl-smoke
mkdir -p /var/lib/easy-switch-link/{hardware,peer}
chown easy-switch-link:easy-switch-link /var/lib/easy-switch-link/hardware
chown easy-switch-link-peer:easy-switch-link-peer /var/lib/easy-switch-link/peer
chmod 700 /var/lib/easy-switch-link/{hardware,peer}
printf 'canary\n' > /tmp/esl-private-canary
install -o easy-switch-link -g easy-switch-link -m 600 /tmp/esl-private-canary /var/lib/easy-switch-link/hardware/isolation-test
install -o easy-switch-link-peer -g easy-switch-link-peer -m 600 /tmp/esl-private-canary /var/lib/easy-switch-link/peer/isolation-test
if runuser -u easy-switch-link-peer -- cat /var/lib/easy-switch-link/hardware/isolation-test; then exit 1; fi
if runuser -u easy-switch-link -- cat /var/lib/easy-switch-link/peer/isolation-test; then exit 1; fi
mkdir -p /home/esl-smoke/.config/easy-switch-link /tmp/esl-admin-bin
chown -R esl-smoke:esl-smoke /home/esl-smoke/.config
printf 'version = 2\n' > /tmp/esl-empty-config
install -o esl-smoke -g esl-smoke -m 600 /tmp/esl-empty-config /home/esl-smoke/.config/easy-switch-link/config.toml
cat > /tmp/esl-admin-bin/systemctl <<'SH'
#!/bin/sh
# Simulate failed service activation; never operate the host's systemd manager.
case "$*" in *is-enabled*) exit 1;; *enable\ --now*) test -f /tmp/esl-admin-succeed;; *) exit 0;; esac
SH
printf '#!/bin/sh\nexit 0\n' > /tmp/esl-admin-bin/udevadm
chmod 755 /tmp/esl-admin-bin/*
if PATH="/tmp/esl-admin-bin:$PATH" /usr/lib/easy-switch-link/service-admin enable --user esl-smoke > /tmp/esl-rollback.log 2>&1; then
    echo 'Expected simulated activation failure' >&2; exit 1
fi
grep -q "enable.*--now.*easy-switch-link-hardware.service" /tmp/esl-rollback.log
test ! -f /etc/easy-switch-link/policy.json
test ! -f /etc/udev/rules.d/71-easy-switch-link-system.rules
cmp /tmp/esl-empty-config /home/esl-smoke/.config/easy-switch-link/config.toml
# Successful configuration round-trip with service management stubbed; actual
# process startup and credential checks are exercised separately below.
touch /tmp/esl-admin-succeed
PATH="/tmp/esl-admin-bin:$PATH" /usr/lib/easy-switch-link/service-admin enable --user esl-smoke
test -f /etc/easy-switch-link/policy.json
printf 'version = 2\nstart_at_login = false\n' > /tmp/esl-updated-config
install -o easy-switch-link -g easy-switch-link -m 600 /tmp/esl-updated-config /var/lib/easy-switch-link/hardware/config.toml
PATH="/tmp/esl-admin-bin:$PATH" /usr/lib/easy-switch-link/service-admin disable
cmp /tmp/esl-updated-config /home/esl-smoke/.config/easy-switch-link/config.toml
rm -f /tmp/esl-admin-succeed
mkdir -p /etc/easy-switch-link /run/easy-switch-link/hardware /var/lib/easy-switch-link/hardware
chown easy-switch-link:easy-switch-link /run/easy-switch-link/hardware /var/lib/easy-switch-link/hardware
chmod 700 /var/lib/easy-switch-link/hardware
python3 - <<'PY'
import json,pwd
from pathlib import Path
p=dict(version=1,owner_uid=pwd.getpwnam('esl-smoke').pw_uid,receiver_uid=pwd.getpwnam('easy-switch-link').pw_uid,
       peer_uid=pwd.getpwnam('easy-switch-link-peer').pw_uid,greeter_uid=120,seat='seat0',login_handoffs=True)
Path('/etc/easy-switch-link/policy.json').write_text(json.dumps(p))
PY
runuser -u easy-switch-link -- easy-switch-link service hardware > /tmp/esl-system-smoke.log 2>&1 &
hardware=$!
trap 'kill "$hardware" 2>/dev/null || true; rm -f /etc/easy-switch-link/policy.json' EXIT
sleep 2
runuser -u esl-smoke -- easy-switch-link service status > /tmp/esl-system-status.json
cat > /tmp/esl-denied.py <<'PY'
import socket,struct,json
s=socket.socket(socket.AF_UNIX);s.connect('/run/easy-switch-link/hardware/control.sock')
body=json.dumps(dict(version=1,control=dict(operation='user',request=dict(command='status')))).encode()
try:
 s.sendall(struct.pack('<I',len(body))+body)
 header=s.recv(4)
 if header:
  n=struct.unpack('<I',header)[0];data=b''
  while len(data)<n:data+=s.recv(n-len(data))
  assert not json.loads(data)['response']['ok']
except (ConnectionResetError,BrokenPipeError):pass
print('Unauthorized service client rejected')
PY
runuser -u nobody -- python3 /tmp/esl-denied.py
runuser -u easy-switch-link-peer -- python3 /tmp/esl-denied.py
kill "$hardware"
wait "$hardware" || true
rm -f /etc/easy-switch-link/policy.json
trap - EXIT
apt-get remove -y easy-switch-link
test ! -x /usr/bin/easy-switch-link
echo 'Package install, reinstall/upgrade, portable IPC, account isolation and removal passed'
