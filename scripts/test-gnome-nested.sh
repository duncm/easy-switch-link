#!/usr/bin/env bash
# Isolated extension-load smoke test. Never connects to the user's session bus.
set -euo pipefail
project=$(cd "$(dirname "$0")/.." && pwd)
temporary=$(mktemp -d /tmp/esl-gnome-test.XXXXXX)
trap 'rm -rf -- "$temporary"' EXIT
mkdir -p "$temporary"/{runtime,config,data/gnome-shell/extensions}
chmod 700 "$temporary/runtime"
cp -r "$project/packaging/gnome/easy-switch-link@duncm.github.com" "$temporary/data/gnome-shell/extensions/"
if [ "${ESL_TEST_NO_CONNECTION:-0}" = 1 ]; then
    sed -i 's/this\._connect();/\/\* isolated startup control: no socket \*\//g' "$temporary/data/gnome-shell/extensions/easy-switch-link@duncm.github.com/extension.js"
fi
if [ "${ESL_TEST_MINIMAL:-0}" = 1 ]; then
    printf '%s\n' "import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js'; export default class extends Extension { enable() {} disable() {} }" > "$temporary/data/gnome-shell/extensions/easy-switch-link@duncm.github.com/extension.js"
fi
export XDG_RUNTIME_DIR="$temporary/runtime" XDG_CONFIG_HOME="$temporary/config" XDG_DATA_HOME="$temporary/data"
export ESL_GNOME_TEST_DIR="$temporary"
dbus-run-session -- bash <<'SH'
set -euo pipefail
unset WAYLAND_DISPLAY XDG_SESSION_ID
Xvfb -displayfd 3 -screen 0 1280x720x24 -nolisten tcp 3>"$ESL_GNOME_TEST_DIR/display" >"$ESL_GNOME_TEST_DIR/xvfb.log" 2>&1 &
xvfb=$!
shell=
trap 'if [ -n "$shell" ]; then kill -KILL "$shell" 2>/dev/null || true; fi; kill "$xvfb" 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do test -s "$ESL_GNOME_TEST_DIR/display" && break; sleep 0.1; done
export DISPLAY=":$(cat "$ESL_GNOME_TEST_DIR/display")" XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME GNOME_SHELL_SESSION_MODE=user
if [ "${ESL_TEST_SKIP_EXTENSION:-0}" = 1 ]; then
    gsettings set org.gnome.shell enabled-extensions '[]'
else
    gsettings set org.gnome.shell enabled-extensions "['easy-switch-link@duncm.github.com']"
fi
gsettings set org.gnome.shell disable-user-extensions false
gnome-shell --nested --wayland --sm-disable --wayland-display=esl-test >"$ESL_GNOME_TEST_DIR/shell.log" 2>&1 &
shell=$!
for _ in $(seq 1 15); do
    if timeout -k 1 2 gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell --method org.gnome.Shell.Extensions.GetExtensionInfo easy-switch-link@duncm.github.com >"$ESL_GNOME_TEST_DIR/info" 2>"$ESL_GNOME_TEST_DIR/query.log"; then
        if [ "${ESL_TEST_SKIP_EXTENSION:-0}" = 1 ]; then cat "$ESL_GNOME_TEST_DIR/info"; exit 0; fi
        if grep -q "'state': <1.0>\|'state': <uint32 1>\|'state': <1>" "$ESL_GNOME_TEST_DIR/info"; then
            cat "$ESL_GNOME_TEST_DIR/info"
            exit 0
        fi
    fi
    kill -0 "$shell" 2>/dev/null || break
    sleep 0.5
done
gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.ListNames
cat "$ESL_GNOME_TEST_DIR/info" "$ESL_GNOME_TEST_DIR/query.log" "$ESL_GNOME_TEST_DIR/shell.log"
exit 1
SH
