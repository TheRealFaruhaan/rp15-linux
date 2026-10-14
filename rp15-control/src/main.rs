//! RP-15 Control: thermals, keyboard backlight and Fn lock for the
//! Eluktronics RP-15, plus the "press sleep again" prompt for rp15d.

mod daemon;
mod gauge;
mod hw;
mod perf_ui;
mod toggles;
mod touchpad;
mod window;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use futures_util::StreamExt;
use gtk::{gdk, gio, glib};

use window::SharedDaemon;

const APP_ID: &str = "io.rp15.Control";
const SLEEP_NOTIFICATION: &str = "sleep-confirm";
/// Owned by the RP-15 OSD GNOME Shell extension while it shows these events.
const OSD_BUS_NAME: &str = "io.rp15.Osd";

thread_local! {
    static OSD_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Whether the shell extension shows on-screen displays, so plain
/// notifications would be duplicates.
pub fn osd_active() -> bool {
    OSD_ACTIVE.with(Cell::get)
}

fn watch_osd() {
    // The watch lives as long as the process.
    let id = gio::bus_watch_name(
        gio::BusType::Session,
        OSD_BUS_NAME,
        gio::BusNameWatcherFlags::NONE,
        |_, _, _| OSD_ACTIVE.with(|a| a.set(true)),
        |_, _| OSD_ACTIVE.with(|a| a.set(false)),
    );
    std::mem::forget(id);
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Keep a daemon proxy around (rp15d may start after us) and turn its sleep-key
/// signals into a notification.
fn watch_daemon(app: &adw::Application, daemon: SharedDaemon) {
    let app = app.downgrade();
    let touchpad = touchpad::TouchpadSync::new(daemon.clone());
    glib::spawn_future_local(async move {
        loop {
            let proxy = match daemon::connect().await {
                Ok(p) => p,
                Err(_) => {
                    glib::timeout_future(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let mut signals = match proxy.receive_sleep_key().await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("rp15-control: cannot subscribe to rp15d: {e}");
                    glib::timeout_future(Duration::from_secs(5)).await;
                    continue;
                }
            };
            daemon.replace(Some(proxy));
            if let Some(t) = &touchpad {
                t.sync_now();
            }

            while let Some(signal) = signals.next().await {
                let Some(app) = app.upgrade() else { return };
                let Ok(args) = signal.args() else { continue };
                if osd_active() {
                    continue;
                }
                if *args.state() == "armed" {
                    let secs = (args.timeout_ms() + 999) / 1000;
                    let n = gio::Notification::new("Press sleep again to suspend");
                    n.set_body(Some(&format!(
                        "Press F1 again within {secs} seconds to put the laptop to sleep."
                    )));
                    n.set_priority(gio::NotificationPriority::Urgent);
                    n.set_icon(&gio::ThemedIcon::new("weather-clear-night-symbolic"));
                    app.send_notification(Some(SLEEP_NOTIFICATION), &n);
                } else {
                    app.withdraw_notification(SLEEP_NOTIFICATION);
                }
            }
            // Stream ended (rp15d went away): reconnect.
            daemon.replace(None);
        }
    });
}

/// Development aid: render the window to a PNG, then quit.
fn screenshot_later(window: &gtk::Window, path: String) {
    let window = window.clone();
    glib::timeout_add_local_once(Duration::from_millis(2500), move || {
        let (w, h) = (window.width(), window.height());
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
        if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
            let texture = renderer.render_texture(node, None);
            if let Err(e) = texture.save_to_png(&path) {
                eprintln!("screenshot failed: {e}");
            }
        }
        window.close();
    });
}

fn main() -> glib::ExitCode {
    // Development screenshots must not activate an installed instance.
    let app_id = if std::env::var_os("RP15_SCREENSHOT").is_some() {
        "io.rp15.Control.Devel"
    } else {
        APP_ID
    };
    let app = adw::Application::builder().application_id(app_id).build();
    app.add_main_option(
        "background",
        glib::Char::from(b'b'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start without a window, only show sleep-key prompts",
        None,
    );

    let daemon: SharedDaemon = Rc::new(RefCell::new(None));
    // Set when started with --background: skip the first activation's window.
    let start_hidden = Rc::new(Cell::new(false));
    let hold = Rc::new(RefCell::new(None));

    app.connect_handle_local_options(glib::clone!(
        #[strong]
        start_hidden,
        move |_, opts| {
            if opts.contains("background") {
                start_hidden.set(true);
            }
            std::ops::ControlFlow::Continue(())
        }
    ));

    app.connect_startup(glib::clone!(
        #[strong]
        daemon,
        #[strong]
        start_hidden,
        #[strong]
        hold,
        move |app| {
            load_css();
            watch_osd();
            watch_daemon(app, daemon.clone());
            toggles::watch(app);
            if start_hidden.get() {
                // Stay alive without a window to relay sleep-key prompts.
                hold.replace(Some(app.hold()));
            }
        }
    ));

    app.connect_activate(glib::clone!(
        #[strong]
        daemon,
        move |app| {
            if start_hidden.replace(false) {
                return;
            }
            let window = app
                .active_window()
                .unwrap_or_else(|| window::build(app, daemon.clone()).upcast());
            if std::env::var_os("RP15_SCREENSHOT").is_some() {
                window.set_default_size(500, 1560);
            }
            window.present();
            if let Ok(path) = std::env::var("RP15_SCREENSHOT") {
                screenshot_later(&window, path);
            }
        }
    ));

    app.run()
}
