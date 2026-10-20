#!/usr/bin/env python3
"""Test helper: open a maximized GTK4 window for N seconds (default 4)."""
import sys
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, GLib

app = Gtk.Application(application_id="io.github.wpe.TestMaximized")


def on_activate(a):
    w = Gtk.ApplicationWindow(application=a, title="WPE pause test")
    w.set_child(Gtk.Label(label="maximized window: wallpaper should be paused"))
    w.maximize()
    w.present()
    GLib.timeout_add_seconds(int(sys.argv[1]) if len(sys.argv) > 1 else 4, a.quit)


app.connect("activate", on_activate)
app.run([])
