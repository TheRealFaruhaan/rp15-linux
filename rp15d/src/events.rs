//! `Event(kind, value)` D-Bus signals for on-screen displays:
//! "profile" (low-power / balanced / performance), "fn_lock", "super_lock" and
//! "fan_boost" ("on" / "off"). Fn lock and Super lock are watched through
//! poll(), so changes from the keys, the app and the daemon itself all show.

use std::fs::File;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use zbus::object_server::SignalEmitter;

use crate::{perf, Daemon};

static EMITTER: OnceLock<SignalEmitter<'static>> = OnceLock::new();
/// Lock changes before this instant come from applying a mode, not the user.
static QUIET_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Applying a mode changes the locks too; the mode's own OSD covers that.
pub fn quiet_for(d: Duration) {
    if let Ok(mut q) = QUIET_UNTIL.lock() {
        *q = Some(Instant::now() + d);
    }
}

fn is_quiet() -> bool {
    QUIET_UNTIL
        .lock()
        .ok()
        .and_then(|q| *q)
        .is_some_and(|t| Instant::now() < t)
}

pub fn init(emitter: SignalEmitter<'static>) {
    let _ = EMITTER.set(emitter);
}

pub async fn emit(kind: &str, value: &str) {
    if let Some(e) = EMITTER.get() {
        if let Err(err) = Daemon::event(e, kind, value).await {
            eprintln!("rp15d: cannot emit {kind} event: {err}");
        }
    }
}

pub fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

fn read_flag(file: &File) -> Option<bool> {
    let mut buf = String::new();
    perf::reread(file, &mut buf).then(|| buf.trim() != "0")
}

/// Emit `kind` whenever the boolean sysfs attribute changes; `invert` for
/// attributes where 1 means "unlocked".
async fn watch_flag(path: PathBuf, kind: &'static str, invert: bool) {
    loop {
        let file = File::open(&path).ok();
        let last = file.as_ref().and_then(read_flag);
        // SAFETY: the AsyncFd owns the File, so the fd stays valid while registered.
        let afd = file.and_then(|f| unsafe { AsyncFd::register_with_interest(f, Interest::PRIORITY) }.ok());
        let (Some(afd), Some(mut last)) = (afd, last) else {
            // Gone while the driver reloads.
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        loop {
            let Ok(mut guard) = afd.ready(Interest::PRIORITY).await else { break };
            guard.clear_ready();
            let Some(value) = read_flag(afd.get_ref()) else { break };
            if value != last {
                last = value;
                if !is_quiet() {
                    emit(kind, on_off(value != invert)).await;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

pub async fn watch_locks() {
    tokio::join!(
        watch_flag(perf::inou("fn_lock"), "fn_lock", false),
        watch_flag(perf::inou("super_key_enable"), "super_lock", true),
    );
}
