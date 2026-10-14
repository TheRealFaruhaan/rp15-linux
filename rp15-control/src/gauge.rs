//! Temperature ring with a spinning fan underneath.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, glib};

const RING_SIZE: i32 = 148;
const RING_WIDTH: f64 = 11.0;
const ARC_START: f64 = 0.75 * PI;
const ARC_SWEEP: f64 = 1.5 * PI;
const TEMP_MIN: f64 = 25.0;
const TEMP_MAX: f64 = 100.0;
/// Real fan speeds would be a blur, scale them down to something readable.
const FAN_VISUAL_SCALE: f64 = 0.06;

/// Green at idle, amber when warm, red when hot.
const STOPS: [(f64, (f64, f64, f64)); 3] = [
    (45.0, (0.20, 0.82, 0.48)),
    (70.0, (0.96, 0.76, 0.18)),
    (88.0, (0.88, 0.11, 0.14)),
];

pub fn temp_color(t: f64) -> (f64, f64, f64) {
    if t <= STOPS[0].0 {
        return STOPS[0].1;
    }
    for w in STOPS.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            let f = (t - t0) / (t1 - t0);
            return (
                c0.0 + (c1.0 - c0.0) * f,
                c0.1 + (c1.1 - c0.1) * f,
                c0.2 + (c1.2 - c0.2) * f,
            );
        }
    }
    STOPS[STOPS.len() - 1].1
}

#[derive(Default)]
struct State {
    target: Option<f64>,
    shown: f64,
    rpm: u32,
    fan_angle: f64,
    last_frame: Option<i64>,
}

#[derive(Clone)]
pub struct Gauge {
    pub widget: gtk::Box,
    state: Rc<RefCell<State>>,
    value: gtk::Label,
    rpm: gtk::Label,
}

impl Gauge {
    pub fn new(title: &str) -> Self {
        let state = Rc::new(RefCell::new(State::default()));

        let ring = gtk::DrawingArea::builder()
            .content_width(RING_SIZE)
            .content_height(RING_SIZE)
            .build();
        ring.set_draw_func(glib::clone!(
            #[strong]
            state,
            move |area, cr, w, h| draw_ring(area, cr, w, h, &state.borrow())
        ));

        let value = gtk::Label::builder().label("--").css_classes(["gauge-value"]).build();
        let caption = gtk::Label::builder()
            .label(title)
            .css_classes(["gauge-caption"])
            .build();
        let center = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        center.append(&value);
        center.append(&caption);

        let overlay = gtk::Overlay::builder().child(&ring).halign(gtk::Align::Center).build();
        overlay.add_overlay(&center);

        let fan = gtk::DrawingArea::builder()
            .content_width(20)
            .content_height(20)
            .valign(gtk::Align::Center)
            .build();
        fan.set_draw_func(glib::clone!(
            #[strong]
            state,
            move |area, cr, w, h| draw_fan(area, cr, w, h, &state.borrow())
        ));
        let rpm = gtk::Label::builder().label("—").css_classes(["gauge-rpm"]).build();
        let fan_row = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .build();
        fan_row.append(&fan);
        fan_row.append(&rpm);

        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .css_classes(["card", "gauge-card"])
            .build();
        widget.append(&overlay);
        widget.append(&fan_row);

        // One frame clock drives both the ring easing and the fan spin.
        widget.add_tick_callback(glib::clone!(
            #[strong]
            state,
            #[weak]
            ring,
            #[weak]
            fan,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move |_, clock| {
                let now = clock.frame_time();
                let mut s = state.borrow_mut();
                let dt = s.last_frame.map_or(0.0, |t| (now - t) as f64 / 1e6).min(0.1);
                s.last_frame = Some(now);
                if let Some(target) = s.target {
                    let diff = target - s.shown;
                    if diff.abs() > 0.01 {
                        s.shown += diff * (dt * 6.0).min(1.0);
                        ring.queue_draw();
                    }
                }
                if s.rpm > 0 {
                    s.fan_angle += f64::from(s.rpm) / 60.0 * 2.0 * PI * dt * FAN_VISUAL_SCALE;
                    s.fan_angle %= 2.0 * PI;
                    fan.queue_draw();
                }
                glib::ControlFlow::Continue
            }
        ));

        Self { widget, state, value, rpm }
    }

    pub fn set(&self, temp: Option<f64>, rpm: Option<u32>) {
        let mut s = self.state.borrow_mut();
        if s.target.is_none() {
            if let Some(t) = temp {
                s.shown = TEMP_MIN.max(t - 15.0);
            }
        }
        s.target = temp;
        s.rpm = rpm.unwrap_or(0);
        self.value.set_label(&temp.map_or("--".into(), |t| format!("{t:.0}°")));
        self.rpm.set_label(&match rpm {
            Some(0) => "Stopped".into(),
            Some(r) => format!("{r} rpm"),
            None => "—".into(),
        });
    }
}

fn draw_ring(area: &gtk::DrawingArea, cr: &cairo::Context, w: i32, h: i32, s: &State) {
    let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let r = f64::from(w.min(h)) / 2.0 - RING_WIDTH;
    let fg = area.color();
    cr.set_line_cap(cairo::LineCap::Round);

    cr.set_line_width(RING_WIDTH);
    cr.set_source_rgba(fg.red().into(), fg.green().into(), fg.blue().into(), 0.10);
    cr.arc(cx, cy, r, ARC_START, ARC_START + ARC_SWEEP);
    let _ = cr.stroke();

    if s.target.is_none() {
        return;
    }
    let frac = ((s.shown - TEMP_MIN) / (TEMP_MAX - TEMP_MIN)).clamp(0.0, 1.0);
    let end = ARC_START + ARC_SWEEP * frac.max(0.001);
    let (red, green, blue) = temp_color(s.shown);

    // Soft glow under the value arc.
    cr.set_line_width(RING_WIDTH * 2.2);
    cr.set_source_rgba(red, green, blue, 0.16);
    cr.arc(cx, cy, r, ARC_START, end);
    let _ = cr.stroke();

    cr.set_line_width(RING_WIDTH);
    cr.set_source_rgb(red, green, blue);
    cr.arc(cx, cy, r, ARC_START, end);
    let _ = cr.stroke();
}

fn draw_fan(area: &gtk::DrawingArea, cr: &cairo::Context, w: i32, h: i32, s: &State) {
    let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let r = f64::from(w.min(h)) / 2.0;
    let fg = area.color();
    let alpha = if s.rpm > 0 { 0.85 } else { 0.35 };
    cr.set_source_rgba(fg.red().into(), fg.green().into(), fg.blue().into(), alpha);
    cr.translate(cx, cy);
    cr.rotate(s.fan_angle);
    // Four broad, swept blades around a hub, inside a thin housing ring.
    for i in 0..4 {
        let _ = cr.save();
        cr.rotate(f64::from(i) * PI / 2.0);
        cr.move_to(r * 0.12, -r * 0.10);
        cr.curve_to(r * 0.30, -r * 0.75, r * 0.95, -r * 0.55, r * 0.78, -r * 0.05);
        cr.curve_to(r * 0.62, r * 0.12, r * 0.30, r * 0.10, r * 0.12, r * 0.10);
        cr.close_path();
        let _ = cr.fill();
        let _ = cr.restore();
    }
    cr.arc(0.0, 0.0, r * 0.2, 0.0, 2.0 * PI);
    let _ = cr.fill();
    cr.set_line_width(1.2);
    cr.arc(0.0, 0.0, r * 0.95, 0.0, 2.0 * PI);
    let _ = cr.stroke();
}
