#!/usr/bin/env python3
"""Drive a (nested/headless) GNOME Shell through Mutter's RemoteDesktop +
ScreenCast D-Bus APIs: take screenshots and inject pointer input.

usage: rd-probe.py OUT_PREFIX [action ...]
actions: shot:NAME  move:X,Y  click:X,Y  rclick:X,Y  sleep:SEC  esc  fps:SEC (screencast frames/s =
         how often the monitor actually repaints)
Talks to the session bus in $DBUS_SESSION_BUS_ADDRESS (the nested one).
"""
import os
import sys
import time
import gi
gi.require_version("Gst", "1.0")
from gi.repository import Gio, GLib, Gst

BTN_LEFT = 0x110
BTN_RIGHT = 0x111
bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(name, path, iface, method, args=None, rtype=None):
    return bus.call_sync(name, path, iface, method, args, rtype, Gio.DBusCallFlags.NONE, -1, None).unpack()


def main():
    Gst.init(None)
    prefix, actions = sys.argv[1], sys.argv[2:]
    state = call("org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig",
                 "org.gnome.Mutter.DisplayConfig", "GetCurrentState")
    # $WPE_PROBE_MONITOR selects the monitor (index in DisplayConfig order).
    connector = state[1][int(os.environ.get("WPE_PROBE_MONITOR", "0"))][0][0]
    print("monitor", connector, flush=True)

    rd_path = call("org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop",
                   "org.gnome.Mutter.RemoteDesktop", "CreateSession")[0]
    rd_id = call("org.gnome.Mutter.RemoteDesktop", rd_path, "org.freedesktop.DBus.Properties", "Get",
                 GLib.Variant("(ss)", ("org.gnome.Mutter.RemoteDesktop.Session", "SessionId")))[0]
    sc_path = call("org.gnome.Mutter.ScreenCast", "/org/gnome/Mutter/ScreenCast",
                   "org.gnome.Mutter.ScreenCast", "CreateSession",
                   GLib.Variant("(a{sv})", ({"remote-desktop-session-id": GLib.Variant("s", rd_id)},)))[0]
    stream = call("org.gnome.Mutter.ScreenCast", sc_path, "org.gnome.Mutter.ScreenCast.Session",
                  "RecordMonitor", GLib.Variant("(sa{sv})", (connector, {"cursor-mode": GLib.Variant("u", 1)})))[0]

    node = {}
    loop = GLib.MainLoop()

    def on_signal(_c, _s, _p, _i, sig, params):
        if sig == "PipeWireStreamAdded":
            node["id"] = params.unpack()[0]
            loop.quit()

    bus.signal_subscribe("org.gnome.Mutter.ScreenCast", "org.gnome.Mutter.ScreenCast.Stream",
                         "PipeWireStreamAdded", stream, None, Gio.DBusSignalFlags.NONE, on_signal)
    call("org.gnome.Mutter.RemoteDesktop", rd_path, "org.gnome.Mutter.RemoteDesktop.Session", "Start")
    GLib.timeout_add_seconds(10, loop.quit)
    loop.run()
    if "id" not in node:
        sys.exit("no pipewire stream")

    pipe = Gst.parse_launch(f"pipewiresrc path={node['id']} always-copy=true ! videoconvert ! "
                            f"videoscale ! video/x-raw,{os.environ.get('WPE_PROBE_SIZE', 'width=960')} ! appsink name=sink max-buffers=1 drop=true sync=false")
    sink = pipe.get_by_name("sink")
    pipe.set_state(Gst.State.PLAYING)

    def rd(method, sig, args):
        call("org.gnome.Mutter.RemoteDesktop", rd_path, "org.gnome.Mutter.RemoteDesktop.Session",
             method, GLib.Variant(sig, args))

    def shot(name):
        # Pull a fresh frame (drop stale ones), encode to PNG.
        sample = None
        end = time.time() + 0.4
        while time.time() < end:
            s = sink.emit("try-pull-sample", 200 * Gst.MSECOND)
            if s:
                sample = s
        if not sample:
            print("no frame for", name, flush=True)
            return
        enc = Gst.parse_launch("appsrc name=src ! videoconvert ! pngenc ! filesink name=out")
        src = enc.get_by_name("src")
        enc.get_by_name("out").set_property("location", f"{prefix}{name}.png")
        src.set_property("caps", sample.get_caps())
        enc.set_state(Gst.State.PLAYING)
        src.emit("push-buffer", sample.get_buffer())
        src.emit("end-of-stream")
        enc.get_bus().timed_pop_filtered(5 * Gst.SECOND, Gst.MessageType.EOS | Gst.MessageType.ERROR)
        enc.set_state(Gst.State.NULL)
        print("shot", name, flush=True)

    for a in actions:
        kind, _, arg = a.partition(":")
        if kind == "shot":
            shot(arg)
        elif kind == "esc":
            rd("NotifyKeyboardKeysym", "(ub)", (0xff1b, True))
            rd("NotifyKeyboardKeysym", "(ub)", (0xff1b, False))
            print("esc", flush=True)
        elif kind == "fps":
            # The stream only gets frames on damage, so this is the on-screen rate.
            sec, n = float(arg), 0
            while sink.emit("try-pull-sample", 0):
                pass
            end = time.time() + sec
            while time.time() < end:
                if sink.emit("try-pull-sample", 100 * Gst.MSECOND):
                    n += 1
            print(f"fps {n / sec:.1f}", flush=True)
        elif kind == "sleep":
            time.sleep(float(arg))
        elif kind in ("move", "click", "rclick"):
            x, y = map(float, arg.split(","))
            rd("NotifyPointerMotionAbsolute", "(sdd)", (stream, x, y))
            if kind != "move":
                # the first absolute motion of a session is applied late
                time.sleep(0.3)
                btn = BTN_LEFT if kind == "click" else BTN_RIGHT
                rd("NotifyPointerButton", "(ib)", (btn, True))
                time.sleep(0.05)
                rd("NotifyPointerButton", "(ib)", (btn, False))
            print(kind, x, y, flush=True)
    pipe.set_state(Gst.State.NULL)
    call("org.gnome.Mutter.RemoteDesktop", rd_path, "org.gnome.Mutter.RemoteDesktop.Session", "Stop")


main()
