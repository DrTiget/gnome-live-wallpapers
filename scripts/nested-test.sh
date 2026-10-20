#!/bin/bash
# Headless GNOME Shell test, fully isolated from the user's real session:
# own D-Bus session bus, GSettings in a keyfile under $OUT (never touches the
# real dconf), own XDG config/data/state/cache dirs.
#   usage: scripts/nested-test.sh        (re-execs itself under dbus-run-session)
# Needs: extension installed in ~/.local/share/gnome-shell/extensions,
# target/release/wpe-daemon and wpe-gallery built with --features gtk.
set -u
cd "$(dirname "$0")/.."
OUT=${OUT:-/tmp/wpe-nested}
if [ -z "${WPE_NESTED_INNER:-}" ]; then
    REAL_DATA=${XDG_DATA_HOME:-$HOME/.local/share}
    rm -rf "$OUT"; mkdir -p "$OUT"
    export WPE_NESTED_INNER=1 REAL_DATA OUT
    # Set BEFORE the private bus starts so dconf/gsettings never see the real config.
    export GSETTINGS_BACKEND=keyfile
    export XDG_CONFIG_HOME=$OUT/config XDG_STATE_HOME=$OUT/state XDG_CACHE_HOME=$OUT/cache XDG_DATA_HOME=$OUT/data
    unset DBUS_SESSION_BUS_ADDRESS
    exec dbus-run-session -- "$0" "$@"
fi
# Desktop Icons NG needs the real XDG user dirs (~/Desktop location).
[ -f "$HOME/.config/user-dirs.dirs" ] && mkdir -p "$XDG_CONFIG_HOME" && cp "$HOME/.config/user-dirs.dirs" "$XDG_CONFIG_HOME/"
[ "${WPE_SHELL_MODE:-}" = ubuntu ] && export XDG_CURRENT_DESKTOP=ubuntu:GNOME XDG_SESSION_DESKTOP=ubuntu
export WAYLAND_DISPLAY=wpe-nested-0
unset DISPLAY
mkdir -p "$XDG_DATA_HOME/gnome-shell/extensions"
cp -r "$REAL_DATA/gnome-shell/extensions/wpe@wpe.github.io" "$XDG_DATA_HOME/gnome-shell/extensions/"  # shell ignores symlinks

gsettings set org.gnome.shell disable-user-extensions false
gsettings set org.gnome.shell enabled-extensions "['wpe@wpe.github.io']"
gsettings set org.gnome.desktop.interface enable-animations false

# WPE_SHELL_MODE=ubuntu also loads ubuntu-dock and Desktop Icons NG like the real session.
gnome-shell ${WPE_SHELL_MODE:+--mode=$WPE_SHELL_MODE} --headless --wayland --no-x11 $(for m in ${WPE_MONITORS:-1600x900}; do printf -- '--virtual-monitor %s ' "$m"; done) \
    --wayland-display "$WAYLAND_DISPLAY" > "$OUT/shell.log" 2>&1 &
SHELL_PID=$!
for _ in $(seq 50); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; sleep 0.2; done
sleep 4
LC_ALL=C gnome-extensions info wpe@wpe.github.io > "$OUT/ext-state.txt" 2>&1
cat "$OUT/ext-state.txt"

GDK_BACKEND=wayland ./target/release/wpe-daemon > "$OUT/daemon.log" 2>&1 &
DAEMON_PID=$!
sleep 3
G=./target/release/wpe-gallery
$G add-dir "${WPE_TEST_DIR:-$HOME/wpe-test}"
$G list
ID=${WPE_ID:-web-interactive}
$G set '*' "$ID"
sleep 6
python3 scripts/rd-probe.py "$OUT/$ID-" shot:1 click:800,450 sleep:1.5 shot:1b move:300,250 sleep:0.05 move:400,300 sleep:0.05 move:500,350 sleep:0.05 move:600,400 sleep:0.05 move:700,450 click:800,450 shot:2-input
$G prop "$ID" message '"Changed over D-Bus"' 2>/dev/null
$G prop "$ID" schemecolor '"1 0.2 0.2"' 2>/dev/null
sleep 1
python3 scripts/rd-probe.py "$OUT/$ID-" shot:3-props
# Auto-pause: a maximized window must pause the wallpaper, closing it resumes.
GDK_BACKEND=wayland python3 scripts/maximized-window.py 4 &
sleep 2
python3 scripts/rd-probe.py "$OUT/$ID-" shot:4-maximized
wait $!
sleep 1
echo "--- pause events:"; grep -E "SetPaused|Pause \{" "$OUT/daemon.log" || echo "(run with WPE_DEBUG=1 to log)"

ID2=${WPE_ID2:-video-test}
$G set '*' "$ID2"; sleep 4
python3 scripts/rd-probe.py "$OUT/$ID2-" shot:1 sleep:1 shot:2
if [ "$(echo ${WPE_MONITORS:-x} | wc -w)" -gt 1 ]; then
    $G set '*' "$ID"; sleep 4
    for i in $(seq 0 $(( $(echo $WPE_MONITORS | wc -w) - 1 ))); do
        WPE_PROBE_MONITOR=$i python3 scripts/rd-probe.py "$OUT/multi-$i-" shot:all
    done
fi
kill $DAEMON_PID; sleep 1; kill $SHELL_PID; wait
