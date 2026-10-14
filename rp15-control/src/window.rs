//! Main window: thermals, keyboard backlight, Fn lock.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{cairo, gdk, glib};

use crate::daemon::DaemonProxy;
use crate::gauge::Gauge;
use crate::hw::{self, Keyboard};
use crate::perf_ui::PerfWidgets;

pub type SharedDaemon = Rc<RefCell<Option<DaemonProxy<'static>>>>;

/// Preset colours, in EC units (0-50 per channel) so they round-trip exactly.
const SWATCHES: [(&str, (u8, u8, u8)); 9] = [
    ("Red", (50, 0, 0)),
    ("Orange", (50, 14, 0)),
    ("Yellow", (50, 38, 0)),
    ("Green", (0, 50, 0)),
    ("Cyan", (0, 50, 50)),
    ("Blue", (0, 0, 50)),
    ("Purple", (24, 0, 50)),
    ("Pink", (50, 0, 22)),
    ("White", (50, 50, 50)),
];

fn ec_rgba((r, g, b): (u8, u8, u8)) -> gdk::RGBA {
    gdk::RGBA::new(hw::ec_to_unit(r), hw::ec_to_unit(g), hw::ec_to_unit(b), 1.0)
}

struct Ui {
    daemon: SharedDaemon,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
    cpu: Gauge,
    gpu: Gauge,
    perf: PerfWidgets,
    preview: gtk::DrawingArea,
    preview_label: gtk::Label,
    swatches: Vec<(gtk::Button, (u8, u8, u8))>,
    custom: gtk::ColorDialogButton,
    brightness: gtk::Scale,
    fn_lock: adw::SwitchRow,
    super_lock: adw::SwitchRow,
    sleep_status: gtk::Label,
    keyboard: Cell<Option<Keyboard>>,
    /// Set while the UI is updated from hardware, so handlers don't write back.
    updating: Cell<bool>,
}

impl Ui {
    /// Call the daemon and refresh afterwards; errors become toasts.
    fn call<F, Fut>(self: &Rc<Self>, f: F)
    where
        F: FnOnce(DaemonProxy<'static>) -> Fut + 'static,
        Fut: Future<Output = zbus::Result<()>> + 'static,
    {
        let Some(proxy) = self.daemon.borrow().clone() else {
            self.toasts.add_toast(adw::Toast::new("The rp15d helper is not running"));
            self.refresh();
            return;
        };
        let ui = self.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = f(proxy).await {
                let msg = match &e {
                    zbus::Error::MethodError(_, Some(m), _) => m.clone(),
                    _ => e.to_string(),
                };
                ui.toasts.add_toast(adw::Toast::new(&format!("Could not apply: {msg}")));
            }
            ui.refresh();
        });
    }

    fn set_color(self: &Rc<Self>, c: (u8, u8, u8)) {
        self.call(move |p| async move { p.set_kbd_color(c.0, c.1, c.2).await });
    }

    fn refresh(&self) {
        self.updating.set(true);

        let t = hw::thermals();
        self.cpu.set(t.cpu_temp, t.cpu_fan_rpm);
        self.gpu.set(t.gpu_temp, t.gpu_fan_rpm);

        let kbd = hw::keyboard();
        if kbd != self.keyboard.get() {
            self.keyboard.set(kbd);
            if let Some(k) = kbd {
                self.brightness.set_range(0.0, f64::from(k.max_brightness));
                self.brightness.set_value(f64::from(k.brightness));
                for (button, c) in &self.swatches {
                    if *c == k.color {
                        button.add_css_class("selected");
                    } else {
                        button.remove_css_class("selected");
                    }
                }
                self.custom.set_rgba(&ec_rgba(k.color));
                self.preview_label.set_label(&if k.brightness == 0 {
                    "Off".to_owned()
                } else {
                    format!("Level {} of {}", k.brightness, k.max_brightness)
                });
            }
            self.preview.queue_draw();
        }

        self.perf.update(&hw::perf());
        let proxy = self.daemon.borrow().clone();
        let perf_power = self.perf.power_handle();
        glib::spawn_future_local(async move {
            let watts = match proxy {
                Some(p) => p.package_power().await.ok(),
                None => None,
            };
            perf_power.set_power(watts);
        });

        if let Some(on) = hw::fn_lock() {
            self.fn_lock.set_active(on);
        }
        if let Some(enabled) = hw::super_key() {
            self.super_lock.set_active(!enabled);
        }

        let has_daemon = self.daemon.borrow().is_some();
        self.banner.set_revealed(!has_daemon);
        for w in [
            self.brightness.upcast_ref::<gtk::Widget>(),
            self.custom.upcast_ref(),
            self.fn_lock.upcast_ref(),
            self.super_lock.upcast_ref(),
        ] {
            w.set_sensitive(has_daemon);
        }
        for (b, _) in &self.swatches {
            b.set_sensitive(has_daemon);
        }
        self.perf.set_sensitive(has_daemon);

        self.updating.set(false);
    }

    fn set_sleep_guard(&self, active: Option<bool>) {
        let (text, class) = match active {
            Some(true) => ("Active", "success"),
            Some(false) => ("Inactive", "warning"),
            None => ("Unknown", "dim-label"),
        };
        self.sleep_status.set_label(text);
        for c in ["success", "warning", "dim-label"] {
            self.sleep_status.remove_css_class(c);
        }
        self.sleep_status.add_css_class(class);
    }
}

fn draw_preview(area: &gtk::DrawingArea, cr: &cairo::Context, w: i32, h: i32, kbd: Option<Keyboard>) {
    let (w, h) = (f64::from(w), f64::from(h));
    let radius = 14.0;
    let fg = area.color();
    let rounded = |cr: &cairo::Context| {
        cr.new_sub_path();
        cr.arc(w - radius, radius, radius, -std::f64::consts::FRAC_PI_2, 0.0);
        cr.arc(w - radius, h - radius, radius, 0.0, std::f64::consts::FRAC_PI_2);
        cr.arc(radius, h - radius, radius, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
        cr.arc(radius, radius, radius, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.close_path();
    };

    rounded(cr);
    cr.set_source_rgba(fg.red().into(), fg.green().into(), fg.blue().into(), 0.06);
    let _ = cr.fill_preserve();
    cr.clip();

    let Some(k) = kbd else { return };
    if k.brightness == 0 {
        return;
    }
    let level = f64::from(k.brightness) / f64::from(k.max_brightness.max(1));
    let c = ec_rgba(k.color);
    let (r, g, b) = (f64::from(c.red()), f64::from(c.green()), f64::from(c.blue()));

    // A soft backlight glow: bright in the middle, fading to the edges.
    let glow = cairo::RadialGradient::new(w / 2.0, h * 1.1, 0.0, w / 2.0, h * 1.1, w * 0.62);
    glow.add_color_stop_rgba(0.0, r, g, b, 0.95 * level);
    glow.add_color_stop_rgba(0.55, r, g, b, 0.45 * level);
    glow.add_color_stop_rgba(1.0, r, g, b, 0.0);
    let _ = cr.set_source(&glow);
    let _ = cr.paint();

    // Keycaps: a simplified laptop layout, widths in key units per row.
    const ROWS: [&[f64]; 4] = [
        &[1.0; 14],
        &[1.5, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.5],
        &[1.8, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.2],
        &[1.3, 1.0, 1.3, 6.0, 1.3, 1.0, 1.0, 1.0],
    ];
    let pad = 10.0;
    let gap = 4.0;
    let kh = (h - 2.0 * pad - gap * 3.0) / 4.0;
    for (row, keys) in ROWS.iter().enumerate() {
        let units: f64 = keys.iter().sum();
        let unit = (w - 2.0 * pad - gap * (keys.len() as f64 - 1.0)) / units;
        let y = pad + row as f64 * (kh + gap);
        let mut x = pad;
        for &k in keys.iter() {
            let kw = k * unit;
            let rr = 3.5;
            cr.new_sub_path();
            cr.arc(x + kw - rr, y + rr, rr, -std::f64::consts::FRAC_PI_2, 0.0);
            cr.arc(x + kw - rr, y + kh - rr, rr, 0.0, std::f64::consts::FRAC_PI_2);
            cr.arc(x + rr, y + kh - rr, rr, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
            cr.arc(x + rr, y + rr, rr, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
            cr.close_path();
            // Dark cap with the backlight bleeding around its edge.
            cr.set_source_rgba(0.08, 0.08, 0.09, 0.92);
            let _ = cr.fill_preserve();
            cr.set_source_rgba(r, g, b, 0.30 + 0.60 * level);
            cr.set_line_width(1.4);
            let _ = cr.stroke();
            x += kw + gap;
        }
    }
}

pub fn build(app: &adw::Application, daemon: SharedDaemon) -> adw::ApplicationWindow {
    // Thermals.
    let cpu = Gauge::new("CPU");
    let gpu = Gauge::new("GPU");
    let thermals = gtk::Box::builder().spacing(12).homogeneous(true).build();
    thermals.append(&cpu.widget);
    thermals.append(&gpu.widget);

    // Keyboard preview + colours.
    let preview = gtk::DrawingArea::builder().content_height(118).hexpand(true).build();
    let preview_label = gtk::Label::builder()
        .css_classes(["preview-label"])
        .halign(gtk::Align::End)
        .hexpand(true)
        .build();
    let preview_caption = gtk::Box::builder().margin_start(4).margin_end(4).build();
    preview_caption.append(
        &gtk::Label::builder()
            .label("BACKLIGHT")
            .css_classes(["preview-label"])
            .halign(gtk::Align::Start)
            .build(),
    );
    preview_caption.append(&preview_label);

    let swatch_box = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .max_children_per_line(10)
        .min_children_per_line(5)
        .column_spacing(6)
        .row_spacing(6)
        .build();
    let mut css = String::new();
    let mut swatches = Vec::new();
    for (i, (name, c)) in SWATCHES.iter().enumerate() {
        let rgba = ec_rgba(*c);
        css.push_str(&format!(
            ".swatch-{i} {{ background: rgb({}, {}, {}); }}\n",
            (rgba.red() * 255.0) as u8,
            (rgba.green() * 255.0) as u8,
            (rgba.blue() * 255.0) as u8
        ));
        let button = gtk::Button::builder()
            .tooltip_text(*name)
            .css_classes(["swatch", &format!("swatch-{i}")])
            .halign(gtk::Align::Center)
            .build();
        swatch_box.insert(&button, -1);
        swatches.push((button, *c));
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let custom = gtk::ColorDialogButton::builder()
        .dialog(&gtk::ColorDialog::builder().with_alpha(false).title("Keyboard colour").build())
        .valign(gtk::Align::Center)
        .build();
    let custom_row = adw::ActionRow::builder()
        .title("Custom colour")
        .activatable_widget(&custom)
        .build();
    custom_row.add_suffix(&custom);

    let brightness = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(0.0, 0.0, 4.0, 1.0, 1.0, 0.0))
        .digits(0)
        .round_digits(0)
        .draw_value(false)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .width_request(180)
        .build();
    for i in 0..=4 {
        brightness.add_mark(f64::from(i), gtk::PositionType::Bottom, None);
    }
    let brightness_row = adw::ActionRow::builder()
        .title("Brightness")
        .subtitle("F6 / F7")
        .build();
    brightness_row.add_suffix(&brightness);

    let keyboard_card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["card", "keyboard-card"])
        .build();
    keyboard_card.append(&preview_caption);
    keyboard_card.append(&preview);
    keyboard_card.append(&swatch_box);

    let keyboard_group = adw::PreferencesGroup::builder().title("Keyboard").build();
    keyboard_group.add(&keyboard_card);
    let keyboard_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(12)
        .build();
    keyboard_list.append(&custom_row);
    keyboard_list.append(&brightness_row);
    keyboard_group.add(&keyboard_list);

    // System.
    let fn_lock = adw::SwitchRow::builder()
        .title("Fn lock")
        .subtitle("Use the media functions of F1–F12 without holding Fn")
        .build();
    let super_lock = adw::SwitchRow::builder()
        .title("Super key lock")
        .subtitle("Disable the Windows key, for example while gaming (F2)")
        .build();
    let sleep_status = gtk::Label::builder().valign(gtk::Align::Center).build();
    let sleep_row = adw::ActionRow::builder()
        .title("Sleep key guard")
        .subtitle("Press F1 twice within 3 seconds to suspend")
        .build();
    sleep_row.add_suffix(&sleep_status);
    let system_group = adw::PreferencesGroup::builder().title("System").build();
    system_group.add(&fn_lock);
    system_group.add(&super_lock);
    system_group.add(&sleep_row);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .margin_top(18)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .build();
    content.append(&thermals);
    let perf = PerfWidgets::new();
    content.append(&perf.group);
    content.append(&keyboard_group);
    content.append(&system_group);

    let clamp = adw::Clamp::builder().maximum_size(560).child(&content).build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build();

    let banner = adw::Banner::new("The rp15d helper service is not running — settings are read-only");
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    body.append(&banner);
    body.append(&scrolled);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new("RP-15 Control", "Eluktronics Red Pill 15"))
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("RP-15 Control")
        .default_width(500)
        .default_height(880)
        .content(&toolbar)
        .build();

    let ui = Rc::new(Ui {
        daemon,
        toasts,
        banner,
        cpu,
        gpu,
        perf,
        preview: preview.clone(),
        preview_label,
        swatches,
        custom: custom.clone(),
        brightness: brightness.clone(),
        fn_lock: fn_lock.clone(),
        super_lock: super_lock.clone(),
        sleep_status,
        keyboard: Cell::new(None),
        updating: Cell::new(false),
    });

    preview.set_draw_func(glib::clone!(
        #[weak]
        ui,
        move |area, cr, w, h| draw_preview(area, cr, w, h, ui.keyboard.get())
    ));

    for (button, c) in &ui.swatches {
        let c = *c;
        button.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| ui.set_color(c)
        ));
    }

    custom.connect_rgba_notify(glib::clone!(
        #[weak]
        ui,
        move |b| {
            if ui.updating.get() {
                return;
            }
            let c = b.rgba();
            ui.set_color((
                hw::unit_to_ec(c.red()),
                hw::unit_to_ec(c.green()),
                hw::unit_to_ec(c.blue()),
            ));
        }
    ));

    brightness.connect_value_changed(glib::clone!(
        #[weak]
        ui,
        move |s| {
            if ui.updating.get() {
                return;
            }
            let level = s.value().round() as u8;
            if ui.keyboard.get().is_some_and(|k| k.brightness == level) {
                return;
            }
            ui.call(move |p| async move { p.set_kbd_brightness(level).await });
        }
    ));

    fn_lock.connect_active_notify(glib::clone!(
        #[weak]
        ui,
        move |row| {
            if ui.updating.get() {
                return;
            }
            let on = row.is_active();
            ui.call(move |p| async move { p.set_fn_lock(on).await });
        }
    ));

    for (button, profile) in &ui.perf.modes {
        let profile = *profile;
        button.connect_toggled(glib::clone!(
            #[weak]
            ui,
            move |b| {
                if ui.updating.get() || !b.is_active() {
                    return;
                }
                ui.call(move |p| async move { p.set_profile(profile).await });
            }
        ));
    }
    type Setter = fn(
        crate::daemon::DaemonProxy<'static>,
        bool,
    ) -> std::pin::Pin<Box<dyn Future<Output = zbus::Result<()>>>>;
    let switches: [(&adw::SwitchRow, Setter); 3] = [
        (&ui.perf.eco, |p, on| Box::pin(async move { p.set_office_eco(on).await })),
        (&ui.perf.cpu_boost, |p, on| Box::pin(async move { p.set_cpu_boost(on).await })),
        (&ui.perf.fan_boost, |p, on| Box::pin(async move { p.set_fan_boost(on).await })),
    ];
    for (row, setter) in switches {
        row.connect_active_notify(glib::clone!(
            #[weak]
            ui,
            move |row| {
                if ui.updating.get() {
                    return;
                }
                let on = row.is_active();
                ui.call(move |p| setter(p, on));
            }
        ));
    }

    super_lock.connect_active_notify(glib::clone!(
        #[weak]
        ui,
        move |row| {
            if ui.updating.get() {
                return;
            }
            let enable = !row.is_active();
            ui.call(move |p| async move { p.set_super_key(enable).await });
        }
    ));

    ui.refresh();
    glib::timeout_add_seconds_local(
        1,
        glib::clone!(
            #[weak]
            ui,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                ui.refresh();
                glib::ControlFlow::Continue
            }
        ),
    );

    // Sleep guard status changes rarely; ask once and whenever the window shows.
    let check_guard = glib::clone!(
        #[weak]
        ui,
        move || {
            let proxy = ui.daemon.borrow().clone();
            glib::spawn_future_local(async move {
                let active = match proxy {
                    Some(p) => p.sleep_guard().await.ok(),
                    None => None,
                };
                ui.set_sleep_guard(active);
            });
        }
    );
    check_guard();
    window.connect_is_active_notify(move |w| {
        if w.is_active() {
            check_guard();
        }
    });

    // Every handler above only holds a weak reference; the window owns the UI state.
    window.connect_destroy(move |_| {
        let _keep_alive = &ui;
    });

    window
}
