//! Performance section: mode cards, live CPU power, ECO / CPU boost / fan boost.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{cairo, glib};

use crate::hw::Perf;

/// (platform_profile, vendor name, icon, CSS class, PL1 in W, ECO PL1 in W)
pub const MODES: [(&str, &str, &str, &str, u32, u32); 3] = [
    ("low-power", "Office", "power-profile-power-saver-symbolic", "office", 45, 25),
    ("balanced", "Gaming", "power-profile-balanced-symbolic", "gaming", 54, 54),
    ("performance", "Beast", "power-profile-performance-symbolic", "beast", 73, 73),
];

/// The highest power limit the vendor software uses (Beast PL4).
const POWER_SCALE_W: f64 = 90.0;

/// Bar state: current watts and the active PL1 marker.
#[derive(Default)]
struct BarState {
    watts: Cell<f64>,
    limit: Cell<Option<f64>>,
}

/// The parts of the power card updated asynchronously.
#[derive(Clone)]
pub struct PowerHandle {
    value: gtk::Label,
    bar: gtk::DrawingArea,
    state: Rc<BarState>,
}

impl PowerHandle {
    pub fn set_power(&self, watts: Option<f64>) {
        self.value.set_label(&watts.map_or("-- W".into(), |w| format!("{w:.1} W")));
        self.state.watts.set(watts.unwrap_or(0.0));
        self.bar.queue_draw();
    }
}

fn draw_bar(area: &gtk::DrawingArea, cr: &cairo::Context, w: i32, h: i32, s: &BarState) {
    let (w, h) = (f64::from(w), f64::from(h));
    let r = h / 2.0;
    let pill = |cr: &cairo::Context, width: f64| {
        cr.new_sub_path();
        cr.arc(width - r, r, r, -std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2);
        cr.arc(r, r, r, std::f64::consts::FRAC_PI_2, 1.5 * std::f64::consts::PI);
        cr.close_path();
    };
    let fg = area.color();
    pill(cr, w);
    cr.set_source_rgba(fg.red().into(), fg.green().into(), fg.blue().into(), 0.10);
    let _ = cr.fill();

    let frac = (s.watts.get() / POWER_SCALE_W).clamp(0.0, 1.0);
    if frac > 0.0 {
        let (red, green, blue) = crate::gauge::temp_color(25.0 + frac * 70.0);
        pill(cr, (w * frac).max(h));
        cr.set_source_rgb(red, green, blue);
        let _ = cr.fill();
    }

    if let Some(limit) = s.limit.get() {
        let x = (w * (limit / POWER_SCALE_W).clamp(0.0, 1.0)).round() - 1.0;
        cr.set_source_rgba(fg.red().into(), fg.green().into(), fg.blue().into(), 0.8);
        cr.rectangle(x, -2.0, 2.0, h + 4.0);
        let _ = cr.fill();
    }
}

pub struct PerfWidgets {
    pub group: adw::PreferencesGroup,
    pub modes: Vec<(gtk::ToggleButton, &'static str)>,
    mode_watts: Vec<gtk::Label>,
    pub eco: adw::SwitchRow,
    pub cpu_boost: adw::SwitchRow,
    pub fan_boost: adw::SwitchRow,
    power: PowerHandle,
    power_limit: gtk::Label,
    /// ECO only applies to Office mode; tracked for sensitivity.
    office_active: Cell<bool>,
    daemon_present: Cell<bool>,
}

impl PerfWidgets {
    pub fn new() -> Self {
        let mode_box = gtk::Box::builder().spacing(10).homogeneous(true).build();
        let mut modes = Vec::new();
        let mut mode_watts = Vec::new();
        let mut first: Option<gtk::ToggleButton> = None;
        for (profile, name, icon, class, _, _) in MODES {
            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .build();
            content.append(&gtk::Image::builder().icon_name(icon).pixel_size(24).build());
            content.append(&gtk::Label::builder().label(name).css_classes(["mode-name"]).build());
            let watts = gtk::Label::builder().css_classes(["mode-watts"]).build();
            content.append(&watts);
            let button = gtk::ToggleButton::builder()
                .child(&content)
                .css_classes(["mode-card", class])
                .build();
            if let Some(f) = &first {
                button.set_group(Some(f));
            } else {
                first = Some(button.clone());
            }
            mode_box.append(&button);
            modes.push((button, profile));
            mode_watts.push(watts);
        }

        let value = gtk::Label::builder()
            .label("-- W")
            .css_classes(["power-value"])
            .halign(gtk::Align::Start)
            .build();
        let power_limit = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .halign(gtk::Align::End)
            .hexpand(true)
            .build();
        let power_head = gtk::Box::builder().spacing(8).build();
        power_head.append(
            &gtk::Label::builder()
                .label("CPU POWER")
                .css_classes(["preview-label"])
                .build(),
        );
        power_head.append(&power_limit);
        let state = Rc::new(BarState::default());
        let bar = gtk::DrawingArea::builder()
            .content_height(10)
            .hexpand(true)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        bar.set_draw_func(glib::clone!(
            #[strong]
            state,
            move |area, cr, w, h| draw_bar(area, cr, w, h, &state)
        ));
        let power_card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["card", "power-card"])
            .margin_top(12)
            .build();
        power_card.append(&power_head);
        power_card.append(&value);
        power_card.append(&bar);

        let eco = adw::SwitchRow::builder()
            .title("Office ECO")
            .subtitle("Office mode only: 25 W instead of 45 W, quieter and cooler")
            .build();
        let cpu_boost = adw::SwitchRow::builder()
            .title("CPU boost")
            .subtitle("Allow clocks above the base frequency")
            .build();
        let fan_boost = adw::SwitchRow::builder()
            .title("Fan boost")
            .subtitle("Run both fans at full speed")
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .margin_top(12)
            .build();
        list.append(&eco);
        list.append(&cpu_boost);
        list.append(&fan_boost);

        let group = adw::PreferencesGroup::builder()
            .title("Performance")
            .description("Also switched by the power-mode button")
            .build();
        group.add(&mode_box);
        group.add(&power_card);
        group.add(&list);

        Self {
            group,
            modes,
            mode_watts,
            eco,
            cpu_boost,
            fan_boost,
            power: PowerHandle { value, bar, state },
            power_limit,
            office_active: Cell::new(false),
            daemon_present: Cell::new(false),
        }
    }

    /// Update from hardware; the caller suppresses its own change handlers.
    pub fn update(&self, p: &Perf) {
        for (i, (button, profile)) in self.modes.iter().enumerate() {
            button.set_active(p.profile.as_deref() == Some(*profile));
            let (_, _, _, _, pl1, eco_pl1) = MODES[i];
            let w = if i == 0 && p.office_eco { eco_pl1 } else { pl1 };
            self.mode_watts[i].set_label(&format!("{w} W"));
        }
        if let Some(on) = p.cpu_boost {
            self.cpu_boost.set_active(on);
        }
        if let Some(on) = p.fan_boost {
            self.fan_boost.set_active(on);
        }
        // ECO belongs to Office mode: shown off and unavailable elsewhere.
        let office = p.profile.as_deref() == Some(MODES[0].0);
        self.office_active.set(office);
        self.eco.set_active(office && p.office_eco);
        self.eco.set_sensitive(office && self.daemon_present.get());
        self.power_limit.set_label(&match p.pl1 {
            Some(0) | None => "firmware default limit".to_owned(),
            Some(w) => format!("limit {w} W"),
        });
        let limit = p.pl1.filter(|&w| w > 0).map(f64::from);
        if self.power.state.limit.replace(limit) != limit {
            self.power.bar.queue_draw();
        }
    }

    pub fn power_handle(&self) -> PowerHandle {
        self.power.clone()
    }

    pub fn set_sensitive(&self, on: bool) {
        self.daemon_present.set(on);
        for (b, _) in &self.modes {
            b.set_sensitive(on);
        }
        for r in [&self.cpu_boost, &self.fan_boost] {
            r.set_sensitive(on);
        }
        self.eco.set_sensitive(on && self.office_active.get());
    }
}
