// VibeConsole window control: GNOME Shell can minimize, close and raise any app's windows;
// a Wayland client cannot. Launcher pads ask through this session-bus service.
import Gio from 'gi://Gio';
import Shell from 'gi://Shell';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const IFACE = `<node>
  <interface name="org.vibeconsole.Windows">
    <method name="State">
      <arg type="s" direction="in" name="app"/>
      <arg type="s" direction="out" name="state"/>
    </method>
    <method name="Apply">
      <arg type="s" direction="in" name="app"/>
      <arg type="s" direction="in" name="action"/>
      <arg type="u" direction="out" name="windows"/>
    </method>
  </interface>
</node>`;

class Windows {
    // Windows the Shell attributes to the desktop entry `app` (e.g. org.gnome.Ptyxis.desktop).
    _windows(app) {
        const shellApp = Shell.AppSystem.get_default().lookup_app(app);
        if (!shellApp)
            throw new Error(`unknown application ${app}`);
        return [shellApp, shellApp.get_windows().filter(w => !w.skip_taskbar)];
    }

    // none | minimized | background | focused
    State(app) {
        const [, windows] = this._windows(app);
        if (windows.length === 0)
            return 'none';
        if (windows.every(w => w.minimized))
            return 'minimized';
        return windows.some(w => w.has_focus()) ? 'focused' : 'background';
    }

    // minimize | close | restore; returns how many windows it acted on.
    Apply(app, action) {
        const [, windows] = this._windows(app);
        const time = global.get_current_time();
        if (action === 'minimize')
            windows.forEach(w => w.minimize());
        else if (action === 'close')
            windows.forEach(w => w.delete(time));
        else if (action === 'restore' && windows.length > 0) {
            // Shell.App.activate() leaves minimized windows minimized (GNOME 50).
            windows.filter(w => w.minimized).forEach(w => w.unminimize());
            Main.activateWindow(windows[0], time);
        }
        else if (action !== 'restore')
            throw new Error(`unknown action ${action}`);
        return windows.length;
    }
}

export default class VibeConsoleWindows extends Extension {
    enable() {
        this._object = Gio.DBusExportedObject.wrapJSObject(IFACE, new Windows());
        this._object.export(Gio.DBus.session, '/org/vibeconsole/Windows');
        this._owner = Gio.bus_own_name(Gio.BusType.SESSION, 'org.vibeconsole.Windows',
            Gio.BusNameOwnerFlags.NONE, null, null, null);
    }

    disable() {
        Gio.bus_unown_name(this._owner);
        this._object.unexport();
        this._object = null;
    }
}
