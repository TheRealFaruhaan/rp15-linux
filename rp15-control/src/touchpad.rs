//! Mirror GNOME's touchpad setting (toggled by F5 = Ctrl+Super+F21 after the
//! hwdb remap) into the touchpad firmware through rp15d, so that disabling the
//! touchpad also lights its corner LED, as on Windows.

use adw::prelude::*;
use gtk::{gio, glib};

use crate::window::SharedDaemon;

const SCHEMA: &str = "org.gnome.desktop.peripherals.touchpad";
const KEY: &str = "send-events";

fn enabled(settings: &gio::Settings) -> bool {
    // "disabled-on-external-mouse" is handled by GNOME itself.
    settings.string(KEY) != "disabled"
}

fn push(settings: &gio::Settings, daemon: &SharedDaemon) {
    let Some(proxy) = daemon.borrow().clone() else {
        return;
    };
    let on = enabled(settings);
    glib::spawn_future_local(async move {
        if let Err(e) = proxy.set_touchpad(on).await {
            eprintln!("rp15-control: cannot set touchpad: {e}");
        }
    });
}

/// Keep the touchpad firmware in sync with GNOME; call `sync_now` whenever
/// rp15d (re)appears.
pub struct TouchpadSync {
    settings: gio::Settings,
    daemon: SharedDaemon,
}

impl TouchpadSync {
    pub fn new(daemon: SharedDaemon) -> Option<Self> {
        let source = gio::SettingsSchemaSource::default()?;
        source.lookup(SCHEMA, true)?;
        let settings = gio::Settings::new(SCHEMA);
        settings.connect_changed(
            Some(KEY),
            glib::clone!(
                #[strong]
                daemon,
                move |s, _| push(s, &daemon)
            ),
        );
        Some(Self { settings, daemon })
    }

    pub fn sync_now(&self) {
        push(&self.settings, &self.daemon);
    }
}
