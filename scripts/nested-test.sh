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
# WPE_AUDIO=1 — audio spectrum test on a private PipeWire (own pipewire,
# pipewire-pulse and wireplumber with device monitors disabled, a null sink as
# the default output): the user's real sound devices and volume are untouched.
if [ -n "${WPE_AUDIO:-}" ]; then
    export PIPEWIRE_RUNTIME_DIR=$OUT/pw PULSE_RUNTIME_PATH=$OUT/pw/pulse WPE_DEBUG=1
    mkdir -p "$PIPEWIRE_RUNTIME_DIR" "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d"
    cat > "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d/90-wpe-test.conf" <<'C'
wireplumber.profiles = {
  main = {
    monitor.alsa = disabled
    monitor.alsa-midi = disabled
    monitor.bluez = disabled
    monitor.bluez.seat-monitoring = disabled
    monitor.v4l2 = disabled
    monitor.libcamera = disabled
  }
}
C
    pipewire > "$OUT/pipewire.log" 2>&1 &
    sleep 1
    wireplumber > "$OUT/wireplumber.log" 2>&1 &
    pipewire-pulse > "$OUT/pipewire-pulse.log" 2>&1 &
    sleep 1.5
    pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=wpe-null media.class=Audio/Sink audio.position=[FL FR] object.linger=true }' >/dev/null
    sleep 1
    # Test tone: 220 Hz left, 3 kHz right.
    python3 - "$OUT/tone.wav" <<'PY'
import math, struct, sys, wave
w = wave.open(sys.argv[1], "wb"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(48000)
w.writeframes(b"".join(struct.pack("<hh", int(12000 * math.sin(2 * math.pi * 220 * i / 48000)),
                                   int(12000 * math.sin(2 * math.pi * 3000 * i / 48000))) for i in range(48000 * 4)))
PY
fi
# Desktop Icons NG needs the real XDG user dirs (~/Desktop location).
[ -f "$HOME/.config/user-dirs.dirs" ] && mkdir -p "$XDG_CONFIG_HOME" && cp "$HOME/.config/user-dirs.dirs" "$XDG_CONFIG_HOME/"
[ "${WPE_SHELL_MODE:-}" = ubuntu ] && export XDG_CURRENT_DESKTOP=ubuntu:GNOME XDG_SESSION_DESKTOP=ubuntu
export WAYLAND_DISPLAY=${WPE_NESTED_DISPLAY:-wpe-nested-0}
unset DISPLAY
mkdir -p "$XDG_DATA_HOME/gnome-shell/extensions"
# WPE_EXT_SRC=gnome-extension/wpe@wpe.github.io tests the extension from the tree instead of the installed one.
cp -r "${WPE_EXT_SRC:-$REAL_DATA/gnome-shell/extensions/wpe@wpe.github.io}" "$XDG_DATA_HOME/gnome-shell/extensions/wpe@wpe.github.io"  # shell ignores symlinks

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

# WPE_SCENES="id1 id2 …" — scene wallpapers (linux-wallpaperengine) instead of
# the web/video run: screenshot, on-screen FPS, CPU/GPU of the scene process,
# pointer (parallax) via the control channel; for the first scene also
# pause on a maximized window, property change (restart) and FPS limit.
renderer_pid() {
    gdbus call --session -d io.github.wpe.Daemon -o /io/github/wpe/Daemon -m io.github.wpe.Daemon1.GetRendererPids \
        | grep -oE "uint32 [0-9]+" | head -1 | cut -d' ' -f2
}
if [ -n "${WPE_SCENES:-}" ]; then
    first=1
    for ID in $WPE_SCENES; do
        echo "=== scene $ID"
        $G set '*' "$ID"
        sleep "${WPE_SCENE_WAIT:-12}"
        PID=$(renderer_pid)
        if [ -z "$PID" ] || ! kill -0 "$PID" 2>/dev/null; then
            echo "scene $ID: NOT RUNNING"; continue
        fi
        python3 scripts/rd-probe.py "$OUT/scene-$ID-" sleep:1 click:800,450 sleep:1 click:1300,750 sleep:1.5 shot:1 fps:4 move:100,100 sleep:1.5 shot:2-mouse-tl move:1500,800 sleep:1.5 shot:3-mouse-br
        python3 scripts/proc-usage.py "$PID" 5
        if [ $first = 1 ]; then
            first=0
            GDK_BACKEND=wayland python3 scripts/maximized-window.py 9 &
            MAXW=$!
            sleep 3
            echo "paused (maximized window):"; python3 scripts/proc-usage.py "$PID" 4
            python3 scripts/rd-probe.py "$OUT/scene-$ID-" shot:4-maximized fps:2
            wait $MAXW; sleep 1
            echo "resumed:"; python3 scripts/rd-probe.py "$OUT/scene-$ID-" fps:2
            $G fps 15; sleep "${WPE_SCENE_WAIT:-12}"
            echo "fps limit 15 (new pid $(renderer_pid)):"; python3 scripts/rd-probe.py "$OUT/scene-$ID-" fps:4
            python3 scripts/proc-usage.py "$(renderer_pid)" 4
            $G fps 30
# WPE_GUI=1 — gallery GUI in the nested session: screenshots $OUT/gui-*.png.
# WPE_GUI_ACTIONS — extra rd-probe actions (e.g. "click:300,300 sleep:1 shot:sel").
if [ -n "${WPE_GUI:-}" ]; then
    GDK_BACKEND=wayland ./target/release/wpe-gallery > "$OUT/gui.log" 2>&1 &
    GUI_PID=$!
    sleep 4
    python3 scripts/rd-probe.py "$OUT/gui-" shot:1-grid ${WPE_GUI_ACTIONS:-}
    kill $GUI_PID; kill $DAEMON_PID; sleep 1; kill $SHELL_PID; wait
    exit 0
fi

            KEY=$($G props "$ID" | python3 -c 'import json,sys; p=[x for x in json.load(sys.stdin) if x["kind"]["type"]=="color"]; print(p[0]["key"] if p else "")')
            if [ -n "$KEY" ]; then
                $G prop "$ID" "$KEY" '"1 0 0"'; sleep "${WPE_SCENE_WAIT:-12}"
                echo "property $KEY=1 0 0 -> pid $(renderer_pid)"
                python3 scripts/rd-probe.py "$OUT/scene-$ID-" shot:5-prop-red
            fi
        fi
    done
    grep -E "^\[scene\]" "$OUT/daemon.log"
    echo "--- not ok (ListWallpapers status):"
    gdbus call --session -d io.github.wpe.Daemon -o /io/github/wpe/Daemon -m io.github.wpe.Daemon1.ListWallpapers \
        | python3 -c 'import ast,json,sys; [print(w["id"], w["status"], w["reason"]) for w in json.loads(ast.literal_eval(sys.stdin.read().strip()[1:-2])) if w["status"] != "ok"]'
    kill $DAEMON_PID; sleep 1; kill $SHELL_PID; wait
    exit 0
fi

# WPE_PAUSE=1 — pause decisions on two monitors: maximized window (that monitor
# only), fullscreen window / borderless window over 90% (all monitors, setting
# pause_on_fullscreen_any; off -> that monitor only). Needs WPE_MONITORS with two.
if [ -n "${WPE_PAUSE:-}" ]; then
    $G set '*' "${WPE_ID:-web-interactive}"; sleep 5
    paused_now() { sleep 2.5; grep -oE '^\[pause\] [^:]+: [a-z]+' "$OUT/daemon.log" | awk '{s[$2]=$3} END {for (m in s) printf "%s%s ", m, s[m]; print ""}'; }
    win() { GDK_BACKEND=wayland python3 scripts/maximized-window.py 4 "$1" "${2:-0}" & sleep 0.1; }
    echo "pause: start:                $(paused_now)"
    win maximized 0;  echo "pause: maximized on 0:       $(paused_now)"; wait $!; echo "pause: closed:               $(paused_now)"
    win fullscreen 1; echo "pause: fullscreen on 1:      $(paused_now)"; wait $!; echo "pause: closed:               $(paused_now)"
    win big 0;        echo "pause: window 95%x93% on 0:  $(paused_now)"; wait $!; echo "pause: closed:               $(paused_now)"
    win borderless 0; echo "pause: borderless on 0:      $(paused_now)"; wait $!; echo "pause: closed:               $(paused_now)"
    gdbus call --session -d io.github.wpe.Daemon -o /io/github/wpe/Daemon -m io.github.wpe.Daemon1.SetPauseOnFullscreenAny false >/dev/null
    win fullscreen 1; echo "pause: fullscreen on 1, any=off: $(paused_now)"; wait $!; echo "pause: closed:               $(paused_now)"
    kill $DAEMON_PID; sleep 1; kill $SHELL_PID; wait
    exit 0
fi

# WPE_CPU="id1 id2" — CPU of the daemon tree (per process) for each wallpaper.
if [ -n "${WPE_CPU:-}" ]; then
    for ID in $WPE_CPU; do
        $G set '*' "$ID"; sleep "${WPE_CPU_WAIT:-8}"
        echo "cpu $ID:"; WPE_CPU_DETAIL=1 python3 scripts/proc-usage.py $DAEMON_PID 5
        python3 scripts/rd-probe.py "$OUT/cpu-$ID-" shot:1 > /dev/null
    done
    kill $DAEMON_PID; sleep 1; kill $SHELL_PID; wait
    exit 0
fi

if [ -n "${WPE_AUDIO:-}" ]; then
    spectrum_pids() { pgrep -f "node.name=wpe-audio-spectrum" | tr '\n' ' '; }
    # Recording streams in the private PipeWire graph: "name:state".
    capture_streams() {
        pw-dump | python3 -c 'import json,sys; print(" ".join(o["info"]["props"].get("application.name","?")+":"+o["info"]["state"] for o in json.load(sys.stdin) if o.get("type","").endswith(":Node") and o["info"]["props"].get("media.class")=="Stream/Input/Audio") or "none")'
    }
    tone() { ( for _ in $(seq "$1"); do pw-play "$OUT/tone.wav"; done ) & }
    $G set '*' web-audio; sleep 5
    echo "audio: capture before any sound: pids [$(spectrum_pids)]"
    tone 3; TONE=$!; sleep 4
    python3 scripts/rd-probe.py "$OUT/audio-" shot:1-tone
    echo "audio: capture streams: $(capture_streams)"
    echo "audio: CPU while playing (daemon with WebKit, then pw-record alone):"
    python3 scripts/proc-usage.py $DAEMON_PID 4; python3 scripts/proc-usage.py "$(spectrum_pids | cut -d' ' -f1)" 4
    wait $TONE; sleep 3
    python3 scripts/rd-probe.py "$OUT/audio-" shot:2-silence
    echo "audio: CPU in silence (no spectrum sent, page still animating):"; python3 scripts/proc-usage.py $DAEMON_PID 4
    GDK_BACKEND=wayland python3 scripts/maximized-window.py 5 &
    MAXW=$!; sleep 2.5; echo "audio: paused (maximized window): capture pids [$(spectrum_pids)]"
    wait $MAXW; sleep 1.5; echo "audio: resumed: capture pids [$(spectrum_pids)]"
    $G set '*' video-test; sleep 4
    echo "audio: video-test (no listener): capture pids [$(spectrum_pids)]"
    for ID in ${WPE_AUDIO_SCENES:-}; do
        $G set '*' "$ID"; sleep "${WPE_SCENE_WAIT:-12}"
        python3 scripts/rd-probe.py "$OUT/audio-scene-$ID-" shot:1-silence
        tone 3; TONE=$!; sleep 3
        python3 scripts/rd-probe.py "$OUT/audio-scene-$ID-" shot:2-tone sleep:0.7 shot:3-tone
        wait $TONE
        echo "audio: scene $ID capture streams: $(capture_streams)"
        GDK_BACKEND=wayland python3 scripts/maximized-window.py 5 &
        MAXW=$!; sleep 2.5; echo "audio: scene $ID paused: capture streams: $(capture_streams)"
        wait $MAXW; sleep 1.5; echo "audio: scene $ID resumed: capture streams: $(capture_streams)"
    done
    echo "--- log:"; grep -E "^\[audio\]|\[wpe-audio\]|^\[scene\]" "$OUT/daemon.log" | sed 's/^.*\[wpe-audio\]/[wpe-audio]/'
    kill $DAEMON_PID; sleep 1; kill $SHELL_PID; pkill -P $$; wait
    exit 0
fi

ID=${WPE_ID:-web-interactive}
$G set '*' "$ID"
sleep 6
python3 scripts/rd-probe.py "$OUT/$ID-" shot:1 move:800,450 sleep:0.5 move:810,460 sleep:0.3 click:800,450 sleep:1.5 shot:1b move:300,250 sleep:0.05 move:400,300 sleep:0.05 move:500,350 sleep:0.05 move:600,400 sleep:0.05 move:700,450 click:800,450 shot:2-input
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
