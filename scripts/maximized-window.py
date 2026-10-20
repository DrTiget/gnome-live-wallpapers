#!/usr/bin/env python3
"""Test helper: open a GTK4 window for N seconds (default 4).
usage: maximized-window.py [SECONDS] [maximized|fullscreen|borderless|big] [MONITOR_INDEX]
  maximized (default) — maximized window;
  fullscreen          — fullscreen window (a game);
  borderless          — undecorated window of the monitor's size ("windowed
                        fullscreen" game; mutter may maximize it);
  big                 — undecorated window of 95% x 93% of the monitor."""
import sys
import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gtk, GLib

seconds = int(sys.argv[1]) if len(sys.argv) > 1 else 4
mode = sys.argv[2] if len(sys.argv) > 2 else "maximized"
index = int(sys.argv[3]) if len(sys.argv) > 3 else 0
app = Gtk.Application(application_id="io.github.wpe.TestMaximized")


def on_activate(a):
    w = Gtk.ApplicationWindow(application=a, title="WPE pause test")
    w.set_child(Gtk.Label(label=f"{mode} window: wallpaper should be paused"))
    mon = Gdk.Display.get_default().get_monitors().get_item(index)
    if mode == "fullscreen":
        w.fullscreen_on_monitor(mon)
    elif mode in ("borderless", "big"):
        g = mon.get_geometry()
        k = (1, 1) if mode == "borderless" else (0.95, 0.93)
        w.set_decorated(False)
        w.set_default_size(int(g.width * k[0]), int(g.height * k[1]))
    else:
        w.maximize()
    w.present()
    GLib.timeout_add_seconds(seconds, a.quit)


app.connect("activate", on_activate)
app.run([])
