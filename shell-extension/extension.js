// RP-15 OSD: coloured, animated on-screen displays for rp15d events.
//
// Listens to io.rp15.Daemon1 on the system bus:
//   Event(kind, value)  kind = profile | fn_lock | super_lock | fan_boost
//   SleepKey(state, timeout_ms)  state = armed | cancelled | suspending
// and owns io.rp15.Osd on the session bus so rp15-control knows not to send
// plain notifications for the same events.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const DAEMON = 'io.rp15.Daemon';
const DAEMON_PATH = '/io/rp15/Daemon';
const DAEMON_IFACE = 'io.rp15.Daemon1';
const OSD_NAME = 'io.rp15.Osd';

const SHOW_MS = 1600;
const IN_MS = 260;
const OUT_MS = 220;

// Same colours as the app: mode cards, temperature rings.
const PROFILES = {
    'low-power': {
        title: 'Office mode', subtitle: 'Quiet and cool', color: '#33d17a',
        icon: 'power-profile-power-saver-symbolic', level: 1,
    },
    'balanced': {
        title: 'Gaming mode', subtitle: 'Balanced performance', color: '#f5c211',
        icon: 'power-profile-balanced-symbolic', level: 2,
    },
    'performance': {
        title: 'Beast mode', subtitle: 'Maximum performance', color: '#e01b24',
        icon: 'power-profile-performance-symbolic', level: 3,
    },
};

const TOGGLES = {
    fn_lock: {
        on: {title: 'Fn lock on', subtitle: 'F1–F12 act as media keys', color: '#3584e4', icon: 'input-keyboard-symbolic'},
        off: {title: 'Fn lock off', subtitle: 'F1–F12 act as function keys', color: '#5e5c64', icon: 'input-keyboard-symbolic'},
    },
    super_lock: {
        on: {title: 'Super key locked', subtitle: 'Press F2 to unlock', color: '#9141ac', icon: 'changes-prevent-symbolic'},
        off: {title: 'Super key unlocked', subtitle: 'The Windows key works again', color: '#5e5c64', icon: 'changes-allow-symbolic'},
    },
    fan_boost: {
        on: {title: 'Fan boost on', subtitle: 'Both fans at full speed', color: '#62a0ea', icon: 'weather-windy-symbolic'},
        off: {title: 'Fan boost off', subtitle: 'Fans back to automatic', color: '#5e5c64', icon: 'weather-windy-symbolic'},
    },
};

const SLEEP = {
    title: 'Press sleep again', subtitle: 'to put the laptop to sleep',
    color: '#613583', icon: 'weather-clear-night-symbolic',
};

class Osd {
    constructor() {
        this._hideId = 0;

        this._badgeIcon = new St.Icon({style_class: 'rp15-osd-icon', icon_size: 28});
        this._badge = new St.Bin({
            style_class: 'rp15-osd-badge',
            child: this._badgeIcon,
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._badge.set_pivot_point(0.5, 0.5);

        this._title = new St.Label({style_class: 'rp15-osd-title'});
        this._subtitle = new St.Label({style_class: 'rp15-osd-subtitle'});

        this._segments = new St.BoxLayout({style_class: 'rp15-osd-segments'});
        this._segs = [0, 1, 2].map(() => {
            const seg = new St.Widget({style_class: 'rp15-osd-seg'});
            this._segments.add_child(seg);
            return seg;
        });

        this._fill = new St.Widget({style_class: 'rp15-osd-fill', x_expand: true});
        this._fill.set_pivot_point(0, 0.5);
        this._track = new St.BoxLayout({style_class: 'rp15-osd-track', x_expand: true});
        this._track.add_child(this._fill);

        const text = new St.BoxLayout({
            style_class: 'rp15-osd-text',
            orientation: Clutter.Orientation.VERTICAL,
            y_align: Clutter.ActorAlign.CENTER,
            x_expand: true,
        });
        text.add_child(this._title);
        text.add_child(this._subtitle);
        text.add_child(this._segments);
        text.add_child(this._track);

        this._box = new St.BoxLayout({
            style_class: 'rp15-osd',
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.END,
        });
        this._box.add_child(this._badge);
        this._box.add_child(text);
        this._box.set_pivot_point(0.5, 0.5);

        // Full-monitor holder that places the card at the bottom centre.
        this._holder = new St.Bin({
            child: this._box,
            reactive: false,
            visible: false,
        });
        Main.layoutManager.uiGroup.add_child(this._holder);
    }

    _place() {
        const m = Main.layoutManager.primaryMonitor;
        if (!m)
            return;
        this._holder.set_position(m.x, m.y);
        this._holder.set_size(m.width, m.height);
        this._holder.style = `padding-bottom: ${Math.round(m.height * 0.12)}px;`;
        Main.layoutManager.uiGroup.set_child_above_sibling(this._holder, null);
    }

    show({title, subtitle, color, icon, level = 0, countdownMs = 0}) {
        this._place();
        this._title.text = title;
        this._subtitle.text = subtitle;
        this._badgeIcon.icon_name = icon;
        this._badge.style = `background-color: ${color}; box-shadow: 0 0 22px ${color}99;`;

        this._segments.visible = level > 0;
        this._segs.forEach((seg, i) => {
            seg.style = i < level ? `background-color: ${color};` : '';
        });

        this._track.visible = countdownMs > 0;
        this._fill.remove_all_transitions();
        if (countdownMs > 0) {
            this._fill.style = `background-color: ${color};`;
            this._fill.scale_x = 1;
            this._fill.ease({
                scale_x: 0,
                duration: countdownMs,
                mode: Clutter.AnimationMode.LINEAR,
            });
        }

        const wasVisible = this._holder.visible;
        this._holder.show();
        this._box.remove_all_transitions();
        if (!wasVisible) {
            this._box.opacity = 0;
            this._box.scale_x = this._box.scale_y = 0.85;
            this._box.translation_y = 24;
        }
        // Opacity must not use an overshooting curve: it is an 8-bit value and
        // would wrap past 255, which shows as a flicker.
        this._box.ease({
            opacity: 255,
            duration: IN_MS,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });
        this._box.ease({
            scale_x: 1,
            scale_y: 1,
            translation_y: 0,
            duration: IN_MS,
            mode: Clutter.AnimationMode.EASE_OUT_BACK,
        });

        // A little pop on the badge, also when an OSD replaces another.
        this._badge.remove_all_transitions();
        this._badge.scale_x = this._badge.scale_y = 1.3;
        this._badge.ease({
            scale_x: 1,
            scale_y: 1,
            duration: 420,
            mode: Clutter.AnimationMode.EASE_OUT_ELASTIC,
        });

        this._scheduleHide(countdownMs > 0 ? countdownMs + 150 : SHOW_MS);
    }

    _scheduleHide(ms) {
        if (this._hideId)
            GLib.source_remove(this._hideId);
        this._hideId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            this._hideId = 0;
            this.hide();
            return GLib.SOURCE_REMOVE;
        });
    }

    hide() {
        if (this._hideId) {
            GLib.source_remove(this._hideId);
            this._hideId = 0;
        }
        if (!this._holder.visible)
            return;
        this._box.remove_all_transitions();
        this._box.ease({
            opacity: 0,
            scale_x: 0.94,
            scale_y: 0.94,
            translation_y: 12,
            duration: OUT_MS,
            mode: Clutter.AnimationMode.EASE_IN_QUAD,
            onStopped: isFinished => {
                if (isFinished)
                    this._holder.hide();
            },
        });
    }

    destroy() {
        if (this._hideId)
            GLib.source_remove(this._hideId);
        this._hideId = 0;
        this._holder.destroy();
    }
}

export default class Rp15OsdExtension extends Extension {
    enable() {
        this._osd = new Osd();
        this._subs = [
            Gio.DBus.system.signal_subscribe(DAEMON, DAEMON_IFACE, 'Event', DAEMON_PATH,
                null, Gio.DBusSignalFlags.NONE,
                (_c, _s, _p, _i, _n, params) => this._onEvent(...params.deep_unpack())),
            Gio.DBus.system.signal_subscribe(DAEMON, DAEMON_IFACE, 'SleepKey', DAEMON_PATH,
                null, Gio.DBusSignalFlags.NONE,
                (_c, _s, _p, _i, _n, params) => this._onSleepKey(...params.deep_unpack())),
        ];
        this._nameId = Gio.bus_own_name(Gio.BusType.SESSION, OSD_NAME,
            Gio.BusNameOwnerFlags.NONE, null, null, null);
    }

    disable() {
        for (const id of this._subs ?? [])
            Gio.DBus.system.signal_unsubscribe(id);
        this._subs = null;
        if (this._nameId)
            Gio.bus_unown_name(this._nameId);
        this._nameId = 0;
        this._osd?.destroy();
        this._osd = null;
    }

    _onEvent(kind, value) {
        if (kind === 'profile') {
            const p = PROFILES[value];
            if (p)
                this._osd.show(p);
            return;
        }
        const t = TOGGLES[kind]?.[value];
        if (t)
            this._osd.show(t);
    }

    _onSleepKey(state, timeoutMs) {
        if (state === 'armed')
            this._osd.show({...SLEEP, countdownMs: timeoutMs});
        else
            this._osd.hide();
    }
}
