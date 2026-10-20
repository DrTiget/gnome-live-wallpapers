// GNOME Shell 50 extension (ESM). Skeleton — stage 1 implementation plan:
//  1. Watch global.display 'window-created'; windows titled "@wpe!<connector>"
//     belong to wpe-daemon.
//  2. Reparent their WindowActor into Main.layoutManager._backgroundGroup
//     (Hanabi approach, override BackgroundManager), hide them from
//     alt-tab/overview/taskbar, make them sticky/below.
//  3. Track pointer over empty desktop (stage.get_actor_at_pos -> background
//     actor) and call io.github.wpe.Daemon1.PointerEvent.
//  4. Per monitor: if a fullscreen or maximized window covers it -> SetPaused.
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import Gio from 'gi://Gio';

const IFACE = `<node><interface name="io.github.wpe.Daemon1">
  <method name="PointerEvent"><arg type="s"/><arg type="d"/><arg type="d"/><arg type="s"/><arg type="u"/></method>
  <method name="SetPaused"><arg type="s"/><arg type="b"/></method>
  <method name="SetMonitors"><arg type="as"/></method>
</interface></node>`;
const DaemonProxy = Gio.DBusProxy.makeProxyWrapper(IFACE);

export default class WpeExtension extends Extension {
    enable() {
        this._proxy = new DaemonProxy(Gio.DBus.session,
            'io.github.wpe.Daemon', '/io/github/wpe/Daemon');
        this._signals = [];
        // TODO(stage 1): window tracking, background placement, input, pause.
    }

    disable() {
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = null;
        this._proxy = null;
    }
}
