//! Short notifications when Fn lock (Fn+Esc) or the Super key lock (F2) is
//! toggled on the keyboard. The driver calls sysfs_notify() on both files, so
//! a POLLPRI watch wakes us immediately without polling.

use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::hw;

const NOTIFICATION_ID: &str = "toggle";

#[derive(Clone, Copy)]
enum Toggle {
    FnLock,
    SuperKey,
}

impl Toggle {
    fn path(self) -> &'static str {
        match self {
            Toggle::FnLock => hw::FN_LOCK,
            Toggle::SuperKey => hw::SUPER_KEY,
        }
    }

    fn message(self, value: bool) -> (&'static str, &'static str) {
        match (self, value) {
            (Toggle::FnLock, true) => ("Fn lock on", "F1–F12 act as media keys"),
            (Toggle::FnLock, false) => ("Fn lock off", "F1–F12 act as function keys"),
            (Toggle::SuperKey, true) => ("Super key unlocked", "The Windows key works again"),
            (Toggle::SuperKey, false) => ("Super key locked", "Press F2 to unlock"),
        }
    }
}

fn read_value(file: &mut File) -> Option<bool> {
    let mut buf = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut buf).ok()?;
    buf.trim().parse::<u8>().ok().map(|v| v != 0)
}

pub fn watch(app: &adw::Application) {
    let pending_withdraw: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    for toggle in [Toggle::FnLock, Toggle::SuperKey] {
        watch_one(app.downgrade(), toggle, pending_withdraw.clone());
    }
}

fn watch_one(
    app: glib::WeakRef<adw::Application>,
    toggle: Toggle,
    pending_withdraw: Rc<RefCell<Option<glib::SourceId>>>,
) {
    let retry = {
        let (app, pending_withdraw) = (app.clone(), pending_withdraw.clone());
        move || {
            let (app, pending_withdraw) = (app.clone(), pending_withdraw.clone());
            glib::timeout_add_local_once(Duration::from_secs(5), move || {
                watch_one(app, toggle, pending_withdraw)
            });
        }
    };

    // The files go away while the driver is reloaded; try again later.
    let Ok(mut file) = File::open(toggle.path()) else {
        retry();
        return;
    };
    let Some(mut last) = read_value(&mut file) else {
        retry();
        return;
    };
    let fd = file.as_raw_fd();

    glib_unix::unix_fd_add_local(fd, glib::IOCondition::PRI | glib::IOCondition::ERR, move |_, _| {
        let Some(value) = read_value(&mut file) else {
            retry();
            return glib::ControlFlow::Break;
        };
        if value == last {
            return glib::ControlFlow::Continue;
        }
        last = value;
        let Some(app) = app.upgrade() else {
            return glib::ControlFlow::Break;
        };

        if crate::osd_active() {
            return glib::ControlFlow::Continue;
        }
        let (title, body) = toggle.message(value);
        let n = gio::Notification::new(title);
        n.set_body(Some(body));
        n.set_icon(&gio::ThemedIcon::new("input-keyboard-symbolic"));
        app.send_notification(Some(NOTIFICATION_ID), &n);

        // Behave like an OSD: go away on its own.
        if let Some(id) = pending_withdraw.borrow_mut().take() {
            id.remove();
        }
        let weak = app.downgrade();
        let slot = pending_withdraw.clone();
        *pending_withdraw.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(2500),
            move || {
                slot.borrow_mut().take();
                if let Some(app) = weak.upgrade() {
                    app.withdraw_notification(NOTIFICATION_ID);
                }
            },
        ));
        glib::ControlFlow::Continue
    });
}
