// SPDX-License-Identifier: GPL-3.0-or-later
//
// WPE live wallpaper — GNOME Shell 50 extension.
//
// wpe-daemon opens one window per monitor titled "@wpe!<connector>". This
// extension (approach of jeffshee/gnome-ext-hanabi):
//  * keeps those windows minimized and hides them from the overview, alt-tab,
//    dock/app tracking;
//  * puts a Clutter.Clone of the window into the desktop background actor of
//    the matching monitor (so it is below all windows and also shows in the
//    overview workspaces);
//  * forwards pointer motion/clicks on the empty desktop to the daemon
//    (io.github.wpe.Daemon1.PointerEvent);
//  * pauses a monitor's wallpaper when a maximized/fullscreen window covers it
//    or the screen is locked (SetPaused).

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import GObject from 'gi://GObject';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Background from 'resource:///org/gnome/shell/ui/background.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Workspace from 'resource:///org/gnome/shell/ui/workspace.js';
import * as WorkspaceThumbnail from 'resource:///org/gnome/shell/ui/workspaceThumbnail.js';

const TITLE_PREFIX = '@wpe!';
const BUS_NAME = 'io.github.wpe.Daemon';
const OBJECT_PATH = '/io/github/wpe/Daemon';
const IFACE = 'io.github.wpe.Daemon1';
const RENDERER_POLL_MS = 1000;
const MOTION_INTERVAL_US = 16000; // ~60 Hz
const FADE_MS = 600;

const DEBUG = !!GLib.getenv('WPE_EXT_DEBUG');
const isRenderer = w => !!w?.title?.startsWith(TITLE_PREFIX);
const connectorOf = w => w.title.slice(TITLE_PREFIX.length);

function log(msg) {
    console.log(`[wpe] ${msg}`);
}

// Desktop Icons NG (ding@rastersoft.com, enabled on Ubuntu) puts a
// transparent full-monitor window below all windows; pointer events on it are
// events on the desktop. Its title is "@!x,y;BDHF" until DING's extension
// renames it to "Desktop Icons N".
const DING_APP_ID = 'com.rastersoft.ding';
const isDingWindow = w => !!w && (
    w.get_gtk_application_id?.() === DING_APP_ID ||
    w.get_wm_class?.() === DING_APP_ID ||
    !!w.title?.startsWith('@!') ||
    /^Desktop Icons( \d+)?$/.test(w.title ?? ''));

function windowOfActor(actor) {
    for (let a = actor; a; a = a.get_parent()) {
        if (a instanceof Meta.WindowActor)
            return a.get_meta_window();
    }
    return null;
}

function monitorIndexForConnector(connector) {
    try {
        return global.backend.get_monitor_manager().get_monitor_for_connector(connector);
    } catch (e) {
        logError(e, '[wpe] get_monitor_for_connector');
        return -1;
    }
}

function callDaemon(method, signature, args) {
    Gio.DBus.session.call(
        BUS_NAME, OBJECT_PATH, IFACE, method,
        new GLib.Variant(signature, args), null,
        Gio.DBusCallFlags.NO_AUTO_START, -1, null,
        (conn, res) => {
            try {
                conn.call_finish(res);
            } catch (e) {
                // Daemon not running: ignore silently.
            }
        });
}

// ---------------------------------------------------------------------------
// Clone of the renderer window inside a monitor's background actor.
// ---------------------------------------------------------------------------

const LiveWallpaper = GObject.registerClass(
class WpeLiveWallpaper extends St.Widget {
    _init(backgroundActor, findRenderer) {
        super._init({
            layout_manager: new Clutter.BinLayout(),
            width: backgroundActor.width,
            height: backgroundActor.height,
            x_expand: true,
            y_expand: true,
            opacity: 0,
        });
        this._bg = backgroundActor;
        this._findRenderer = findRenderer;
        this._clone = null;
        this._sourceDestroyId = 0;
        this._pollId = 0;
        this._disposed = false;

        backgroundActor.layout_manager = new Clutter.BinLayout();
        backgroundActor.add_child(this);
        this.connect('destroy', () => this._onDestroy());
        this._attach();
    }

    _attach() {
        if (this._disposed)
            return;
        const tryAttach = () => {
            if (this._disposed)
                return GLib.SOURCE_REMOVE;
            const source = this._findRenderer(this._bg.monitor);
            if (!source)
                return GLib.SOURCE_CONTINUE;
            log(`attaching renderer clone to background of monitor ${this._bg.monitor}`);
            this._clone = new Clutter.Clone({source, x_expand: true, y_expand: true});
            this._sourceDestroyId = source.connect('destroy', () => {
                this._sourceDestroyId = 0;
                this._clone?.destroy();
                this._clone = null;
                this.opacity = 0;
                this._attach();
            });
            this.add_child(this._clone);
            this.ease({opacity: 255, duration: FADE_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
            this._pollId = 0;
            return GLib.SOURCE_REMOVE;
        };
        if (tryAttach() === GLib.SOURCE_CONTINUE)
            this._pollId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, RENDERER_POLL_MS, tryAttach);
    }

    _onDestroy() {
        this._disposed = true;
        if (this._pollId) {
            GLib.source_remove(this._pollId);
            this._pollId = 0;
        }
        if (this._clone) {
            if (this._sourceDestroyId)
                this._clone.source?.disconnect(this._sourceDestroyId);
            this._clone.set_source(null);
            this._clone = null;
        }
    }
});

// ---------------------------------------------------------------------------
// Keeps renderer windows minimized, positioned and out of the way.
// ---------------------------------------------------------------------------

class RendererWindows {
    constructor(onChanged) {
        this._windows = new Map(); // Meta.Window -> signal ids
        this._onChanged = onChanged;
        this._mapId = 0;
        this._createdId = 0;
    }

    enable() {
        this._mapId = global.window_manager.connect_after('map', (_wm, actor) => {
            this._maybeManage(actor.get_meta_window());
        });
        // The renderer window is mapped (just invisible); never let it keep focus.
        this._focusId = global.display.connect('notify::focus-window', () => {
            const w = global.display.focus_window;
            if (isRenderer(w))
                this._dropFocus(w);
        });
        this._createdId = global.display.connect('window-created', (_d, w) => {
            // Title may arrive after creation on Wayland.
            const id = w.connect('notify::title', () => {
                if (isRenderer(w)) {
                    w.disconnect(id);
                    this._maybeManage(w);
                }
            });
            this._maybeManage(w);
        });
        for (const w of global.display.list_all_windows())
            this._maybeManage(w);
        this._monitorsId = Main.layoutManager.connect('monitors-changed', () => {
            for (const w of this._windows.keys())
                this._place(w);
        });
    }

    disable() {
        if (this._mapId)
            global.window_manager.disconnect(this._mapId);
        if (this._createdId)
            global.display.disconnect(this._createdId);
        if (this._focusId)
            global.display.disconnect(this._focusId);
        if (this._monitorsId)
            Main.layoutManager.disconnect(this._monitorsId);
        this._monitorsId = 0;
        if (this._dropFocusId)
            GLib.source_remove(this._dropFocusId);
        this._dropFocusId = 0;
        this._mapId = this._createdId = this._focusId = 0;
        for (const [w, ids] of this._windows) {
            ids.forEach(id => w.disconnect(id));
            this._release(w);
        }
        this._windows.clear();
    }

    // Meta.WindowActor of the renderer window for a monitor index
    actorForMonitor(index) {
        for (const w of this._windows.keys()) {
            if (monitorIndexForConnector(connectorOf(w)) === index) {
                const actor = w.get_compositor_private();
                if (actor)
                    return actor;
            }
        }
        return null;
    }

    connectorForMonitor(index) {
        for (const w of this._windows.keys()) {
            if (monitorIndexForConnector(connectorOf(w)) === index)
                return connectorOf(w);
        }
        return null;
    }

    connectors() {
        return [...this._windows.keys()].map(connectorOf);
    }

    _maybeManage(w) {
        if (!w || !isRenderer(w) || this._windows.has(w))
            return;
        log(`managing renderer window for ${connectorOf(w)} (monitor ${monitorIndexForConnector(connectorOf(w))})`);
        const ids = [];
        ids.push(w.connect('notify::minimized', () => this._enforce(w)));
        ids.push(w.connect_after('shown', () => this._enforce(w)));
        ids.push(w.connect_after('raised', () => w.lower()));
        ids.push(w.connect('workspace-changed', () => this._enforce(w)));
        ids.push(w.connect('position-changed', () => this._place(w)));
        ids.push(w.connect('size-changed', () => this._place(w)));
        ids.push(w.connect('unmanaged', () => {
            ids.forEach(id => w.disconnect(id));
            this._windows.delete(w);
            this._onChanged();
        }));
        this._windows.set(w, ids);
        this._place(w);
        this._enforce(w);
        this._dropFocus(w);
        this._onChanged();
    }

    _place(w) {
        const index = monitorIndexForConnector(connectorOf(w));
        if (index < 0)
            return;
        const geo = global.display.get_monitor_geometry(index);
        const rect = w.get_frame_rect();
        if (DEBUG)
            log(`place ${connectorOf(w)}: frame ${rect.x},${rect.y} ${rect.width}x${rect.height} monitor ${geo.x},${geo.y} ${geo.width}x${geo.height}`);
        if (rect.x === geo.x && rect.y === geo.y && rect.width === geo.width && rect.height === geo.height)
            return;
        // Mutter auto-maximizes / clamps windows as big as the work area
        // (dock, top bar); undo that so the clone maps 1:1 onto the monitor.
        if (w.is_maximized())
            w.unmaximize();
        w.move_resize_frame(true, geo.x, geo.y, geo.width, geo.height);
    }

    // Hide the real window without minimizing it: a minimized window gets the
    // xdg-toplevel "suspended" state and GTK/WebKit stop rendering, which
    // would freeze the clone. Instead the window stays mapped (sticky, at the
    // bottom) and only its actor is hidden; Clutter clones still paint hidden
    // sources, and mutter keeps sending frame callbacks because the surface
    // is painted through the clone.
    _enforce(w) {
        if (w.minimized)
            w.unminimize();
        if (!w.is_on_all_workspaces())
            w.stick();
        if (w.above)
            w.unmake_above();
        w.lower();
        const actor = w.get_compositor_private();
        if (!actor)
            return;
        if (!actor._wpeVisibleId) {
            actor._wpeVisibleId = actor.connect('notify::visible', () => {
                if (actor.visible && !GLib.getenv('WPE_EXT_SHOW_WINDOWS'))
                    actor.hide();
            });
        }
        if (!GLib.getenv('WPE_EXT_SHOW_WINDOWS'))
            actor.hide();
    }

    _dropFocus(w) {
        // Deferred: focus changes also happen while windows are unmanaging.
        if (this._dropFocusId)
            return;
        this._dropFocusId = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
            this._dropFocusId = 0;
            if (this._windows.has(w) && w.has_focus())
                this._focusOther(w);
            return GLib.SOURCE_REMOVE;
        });
    }

    _focusOther(w) {
        const time = global.get_current_time();
        const ws = global.workspace_manager.get_active_workspace();
        // get_tab_list is filtered by our override, so renderers are excluded.
        const next = global.display.get_tab_list(Meta.TabList.NORMAL, ws).find(x => x !== w && !x.minimized);
        if (next)
            next.activate(time);
        else
            global.display.unset_input_focus(time);
    }

    _release(w) {
        const actor = w.get_compositor_private();
        if (actor?._wpeVisibleId) {
            actor.disconnect(actor._wpeVisibleId);
            delete actor._wpeVisibleId;
            actor.show();
        }
    }
}

// ---------------------------------------------------------------------------
// Pointer forwarding: events that land on the bare desktop.
// ---------------------------------------------------------------------------

class PointerForwarder {
    constructor(renderers) {
        this._renderers = renderers;
        this._id = 0;
        this._lastMotion = 0;
        this._pendingMotion = null;
        this._motionTimeoutId = 0;
        this._pressedOnDesktop = false;
    }

    enable() {
        this._id = global.stage.connect('captured-event', (_stage, event) => {
            try {
                this._handle(event);
            } catch (e) {
                logError(e, '[wpe] pointer');
            }
            return Clutter.EVENT_PROPAGATE;
        });
    }

    disable() {
        if (this._id)
            global.stage.disconnect(this._id);
        this._id = 0;
        if (this._motionTimeoutId)
            GLib.source_remove(this._motionTimeoutId);
        this._motionTimeoutId = 0;
        this._pendingMotion = null;
    }

    _handle(event) {
        const type = event.type();
        let kind;
        if (type === Clutter.EventType.MOTION)
            kind = 'motion';
        else if (type === Clutter.EventType.BUTTON_PRESS)
            kind = 'press';
        else if (type === Clutter.EventType.BUTTON_RELEASE)
            kind = 'release';
        else
            return;

        if (DEBUG && kind !== 'motion')
            log(`pointer ${kind} overview=${Main.overview.visible} modal=${Main.modalCount}`);
        if (Main.overview.visible || Main.modalCount > 0 || Main.sessionMode.isLocked)
            return;

        const [x, y] = event.get_coords();
        const target = global.stage.get_actor_at_pos(Clutter.PickMode.REACTIVE, x, y);
        // The background actor itself is reactive (right-click menu), and our
        // clone lives inside it.
        const onDesktop = target === global.stage ||
            Main.layoutManager._backgroundGroup.contains(target) ||
            isDingWindow(windowOfActor(target));
        if (DEBUG && kind !== 'motion')
            log(`pointer ${kind} at ${x},${y} target=${target} window=${windowOfActor(target)?.title} desktop=${onDesktop}`);
        // Deliver a release if the press started on the desktop.
        if (kind === 'release' && this._pressedOnDesktop) {
            this._pressedOnDesktop = false;
        } else if (!onDesktop) {
            return;
        }
        if (kind === 'press')
            this._pressedOnDesktop = true;

        const index = global.display.get_current_monitor();
        const connector = this._renderers.connectorForMonitor(index);
        if (!connector)
            return;
        const geo = global.display.get_monitor_geometry(index);
        const args = [connector, x - geo.x, y - geo.y, kind, kind === 'motion' ? 0 : event.get_button()];

        if (kind !== 'motion') {
            this._flushMotion();
            callDaemon('PointerEvent', '(sddsu)', args);
            return;
        }
        // Rate-limit motion to ~60 Hz but always deliver the last position.
        this._pendingMotion = args;
        const now = GLib.get_monotonic_time();
        if (now - this._lastMotion >= MOTION_INTERVAL_US) {
            this._flushMotion();
        } else if (!this._motionTimeoutId) {
            this._motionTimeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, MOTION_INTERVAL_US / 1000, () => {
                this._motionTimeoutId = 0;
                this._flushMotion();
                return GLib.SOURCE_REMOVE;
            });
        }
    }

    _flushMotion() {
        if (!this._pendingMotion)
            return;
        this._lastMotion = GLib.get_monotonic_time();
        callDaemon('PointerEvent', '(sddsu)', this._pendingMotion);
        this._pendingMotion = null;
    }
}

// ---------------------------------------------------------------------------
// Pause when covered by maximized/fullscreen windows or when locked.
// ---------------------------------------------------------------------------

class AutoPause {
    constructor(renderers) {
        this._renderers = renderers;
        this._paused = new Map(); // connector -> bool (last sent)
        this._signals = []; // [obj, id]
        this._windowSignals = new Map();
        this._idleId = 0;
        this._watchId = 0;
    }

    enable() {
        const on = (obj, sig, cb = () => this.queue()) => this._signals.push([obj, obj.connect(sig, cb)]);
        on(global.display, 'window-created', (_d, w) => {
            this._track(w);
            this.queue();
        });
        on(global.display, 'restacked');
        on(global.display, 'window-entered-monitor');
        on(global.display, 'window-left-monitor');
        on(global.workspace_manager, 'active-workspace-changed');
        on(global.workspace_manager, 'showing-desktop-changed');
        on(Main.sessionMode, 'updated');
        on(Main.layoutManager, 'monitors-changed');
        for (const w of global.display.list_all_windows())
            this._track(w);
        // Daemon (re)started: resend state.
        this._watchId = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME, Gio.BusNameWatcherFlags.NONE,
            () => {
                this._paused.clear();
                this.queue();
            }, null);
        this.queue();
    }

    disable() {
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        for (const [w, ids] of this._windowSignals)
            ids.forEach(id => w.disconnect(id));
        this._windowSignals.clear();
        if (this._idleId)
            GLib.source_remove(this._idleId);
        this._idleId = 0;
        if (this._watchId)
            Gio.bus_unwatch_name(this._watchId);
        this._watchId = 0;
        // Leave everything playing.
        for (const [connector, paused] of this._paused) {
            if (paused)
                callDaemon('SetPaused', '(sb)', [connector, false]);
        }
        this._paused.clear();
    }

    _track(w) {
        if (!w || this._windowSignals.has(w))
            return;
        const ids = ['notify::maximized-horizontally', 'notify::maximized-vertically',
            'notify::fullscreen', 'notify::minimized'].map(s => w.connect(s, () => this.queue()));
        ids.push(w.connect('unmanaged', () => {
            ids.forEach(id => w.disconnect(id));
            this._windowSignals.delete(w);
            this.queue();
        }));
        this._windowSignals.set(w, ids);
    }

    queue() {
        if (this._idleId)
            return;
        this._idleId = GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            this._idleId = 0;
            this._update();
            return GLib.SOURCE_REMOVE;
        });
    }

    _update() {
        const covered = new Set();
        const locked = Main.sessionMode.isLocked;
        if (!locked) {
            const ws = global.workspace_manager.get_active_workspace();
            for (const w of ws.list_windows()) {
                if (isRenderer(w) || isDingWindow(w) || w.skip_taskbar ||
                    w.get_window_type() === Meta.WindowType.DESKTOP || !w.showing_on_its_workspace())
                    continue;
                if (w.fullscreen || w.is_maximized())
                    covered.add(w.get_monitor());
            }
        }
        for (const connector of this._renderers.connectors()) {
            const index = monitorIndexForConnector(connector);
            const paused = locked || covered.has(index);
            if (this._paused.get(connector) !== paused) {
                this._paused.set(connector, paused);
                callDaemon('SetPaused', '(sb)', [connector, paused]);
            }
        }
    }
}

// ---------------------------------------------------------------------------

export default class WpeExtension extends Extension {
    enable() {
        this._injections = new InjectionManager();
        this._wallpapers = new Set();
        this._autoPause = null;
        this._renderers = new RendererWindows(() => this._autoPause?.queue());
        this._autoPause = new AutoPause(this._renderers);
        this._pointer = new PointerForwarder(this._renderers);
        this._installOverrides();
        this._filterAttention(true);
        this._renderers.enable();
        this._pointer.enable();
        this._autoPause.enable();
        this._reloadBackgrounds();
    }

    disable() {
        this._autoPause.disable();
        this._pointer.disable();
        this._renderers.disable();
        this._injections.clear();
        this._injections = null;
        this._filterAttention(false);
        this._reloadBackgrounds();
        this._autoPause = this._pointer = this._renderers = null;
    }

    _installOverrides() {
        const self = this;
        const inj = this._injections;
        inj.overrideMethod(Background.BackgroundManager.prototype, '_createBackgroundActor', original => {
            return function () {
                const backgroundActor = original.call(this);
                const isLockScreen = this._container?.style_class?.includes('screen-shield-background') ?? false;
                if (isLockScreen)
                    return backgroundActor;
                const wp = new LiveWallpaper(backgroundActor,
                    index => self._renderers?.actorForMonitor(index) ?? null);
                self._wallpapers.add(wp);
                wp.connect('destroy', () => self._wallpapers?.delete(wp));
                return backgroundActor;
            };
        });

        inj.overrideMethod(Shell.Global.prototype, 'get_window_actors', original => {
            return function (...args) {
                return original.apply(this, args).filter(a => !isRenderer(a.meta_window));
            };
        });

        for (const proto of [Workspace.Workspace.prototype, WorkspaceThumbnail.WorkspaceThumbnail.prototype]) {
            inj.overrideMethod(proto, '_isOverviewWindow', original => {
                return function (window) {
                    return isRenderer(window) ? false : original.call(this, window);
                };
            });
        }

        inj.overrideMethod(Meta.Display.prototype, 'get_tab_list', original => {
            return function (type, workspace) {
                return original.call(this, type, workspace).filter(w => !isRenderer(w));
            };
        });

        inj.overrideMethod(Shell.WindowTracker.prototype, 'get_window_app', original => {
            return function (window) {
                return isRenderer(window) ? null : original.call(this, window);
            };
        });

        inj.overrideMethod(Shell.App.prototype, 'get_windows', original => {
            return function () {
                return original.call(this).filter(w => !isRenderer(w));
            };
        });

        inj.overrideMethod(Shell.App.prototype, 'get_n_windows', () => {
            return function () {
                return this.get_windows().length;
            };
        });

        inj.overrideMethod(Shell.AppSystem.prototype, 'get_running', original => {
            return function () {
                return original.call(this).filter(app => app.get_n_windows() > 0);
            };
        });
    }

    // Shell's WindowAttentionHandler ("… is ready" notifications) connects
    // bound methods, so it can't be patched on the prototype: reconnect it
    // through a filter that ignores renderer windows.
    _filterAttention(enable) {
        const wah = Main.windowAttentionHandler;
        if (!wah?._onWindowDemandsAttention)
            return;
        global.display.disconnectObject(wah);
        const handler = enable
            ? (display, window) => {
                if (!isRenderer(window))
                    wah._onWindowDemandsAttention(display, window);
            }
            : wah._onWindowDemandsAttention.bind(wah);
        global.display.connectObject(
            'window-demands-attention', handler,
            'window-marked-urgent', handler, wah);
    }

    _reloadBackgrounds() {
        for (const wp of [...this._wallpapers ?? []])
            wp.destroy();
        this._wallpapers?.clear();
        global.compositor.get_laters().add(Meta.LaterType.BEFORE_REDRAW, () => {
            Main.layoutManager._updateBackgrounds();
            try {
                Main.overview._overview._controls._workspacesDisplay._updateWorkspacesViews();
            } catch (e) {
                // Overview not built yet or changed by another extension.
            }
            return GLib.SOURCE_REMOVE;
        });
    }
}
