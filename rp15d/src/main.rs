//! rp15d - privileged helper for the Eluktronics RP-15.
//!
//! Everything readable lives in sysfs and is world-readable, so clients read it
//! directly. This daemon only does what needs root:
//!
//! * keyboard backlight colour / brightness, Fn lock and Super key writes,
//!   range-checked,
//! * per-mode settings (keyboard colour and brightness, Fn lock, Super key
//!   lock, CPU and fan boost), restored at boot with the Super key unlocked
//!   (see modes.rs),
//! * holding the ACPI sleep button so that F1 needs a second press to suspend,
//! * performance modes: power limits per platform profile, Office ECO, CPU
//!   boost, fan boost and package power (see perf.rs).
//!
//! If the daemon is not running, the sleep button falls back to GNOME's default
//! behaviour (immediate suspend), so nothing ever gets stuck.

mod events;
mod modes;
mod perf;
mod touchpad;

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::Mutex;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface, Connection};

const BUS_NAME: &str = "io.rp15.Daemon";
const OBJECT_PATH: &str = "/io/rp15/Daemon";

const LED_DIR: &str = "/sys/class/leds/uniwill::kbd_backlight";
const FN_LOCK: &str = "/sys/devices/platform/INOU0000:00/fn_lock";
const SUPER_KEY: &str = "/sys/devices/platform/INOU0000:00/super_key_enable";
const STATE_DIR: &str = "/var/lib/rp15d";

/// The EC renders values above 50 unpredictably, see FINDINGS.md.
pub(crate) const MAX_INTENSITY: u8 = 50;
const MAX_BRIGHTNESS: u8 = 4;
/// Time window for the second sleep-key press.
const SLEEP_CONFIRM: Duration = Duration::from_secs(3);

fn led(attr: &str) -> PathBuf {
    Path::new(LED_DIR).join(attr)
}

/// sysfs writes go through the EC with ~6 ms per byte, keep them off the reactor.
pub(crate) async fn sysfs_write(path: PathBuf, value: String) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || std::fs::write(path, value))
        .await
        .map_err(std::io::Error::other)?
}

fn to_fdo(err: std::io::Error) -> fdo::Error {
    fdo::Error::Failed(err.to_string())
}

pub(crate) struct Daemon {
    /// Serialises writes, so two clients cannot interleave a colour update.
    lock: Mutex<()>,
}

#[interface(name = "io.rp15.Daemon1")]
impl Daemon {
    /// Set the keyboard colour, each channel 0-50. Remembered per mode.
    async fn set_kbd_color(&self, red: u8, green: u8, blue: u8) -> fdo::Result<()> {
        if red > MAX_INTENSITY || green > MAX_INTENSITY || blue > MAX_INTENSITY {
            return Err(fdo::Error::InvalidArgs(format!(
                "channels must be 0-{MAX_INTENSITY}"
            )));
        }
        let _guard = self.lock.lock().await;
        let value = format!("{red} {green} {blue}");
        sysfs_write(led("multi_intensity"), value)
            .await
            .map_err(to_fdo)?;
        modes::remember_current().await;
        Ok(())
    }

    /// Set the keyboard brightness level, 0-4.
    async fn set_kbd_brightness(&self, level: u8) -> fdo::Result<()> {
        if level > MAX_BRIGHTNESS {
            return Err(fdo::Error::InvalidArgs(format!(
                "level must be 0-{MAX_BRIGHTNESS}"
            )));
        }
        let _guard = self.lock.lock().await;
        sysfs_write(led("brightness"), level.to_string())
            .await
            .map_err(to_fdo)?;
        modes::remember_current().await;
        Ok(())
    }

    /// Enable or disable Fn lock (F1-F12 act as media keys when enabled).
    async fn set_fn_lock(&self, enabled: bool) -> fdo::Result<()> {
        let _guard = self.lock.lock().await;
        sysfs_write(PathBuf::from(FN_LOCK), u8::from(enabled).to_string())
            .await
            .map_err(to_fdo)?;
        modes::remember_current().await;
        Ok(())
    }

    /// Enable or disable the Super (Windows) key; disabled is what F2 toggles.
    async fn set_super_key(&self, enabled: bool) -> fdo::Result<()> {
        let _guard = self.lock.lock().await;
        sysfs_write(PathBuf::from(SUPER_KEY), u8::from(enabled).to_string())
            .await
            .map_err(to_fdo)?;
        modes::remember_current().await;
        Ok(())
    }

    /// Switch the performance mode: "low-power" (Office), "balanced" (Gaming)
    /// or "performance" (Beast). Same as the power-mode button.
    async fn set_profile(&self, profile: &str) -> fdo::Result<()> {
        if !perf::PROFILES.contains(&profile) {
            return Err(fdo::Error::InvalidArgs(format!(
                "profile must be one of {:?}",
                perf::PROFILES
            )));
        }
        let _guard = self.lock.lock().await;
        sysfs_write(PathBuf::from(perf::PROFILE), profile.to_owned())
            .await
            .map_err(to_fdo)
    }

    /// Office mode at 25 W instead of 45 W. Remembered across boots.
    async fn set_office_eco(&self, enabled: bool) -> fdo::Result<()> {
        save_state(perf::ECO_FILE, if enabled { "1" } else { "0" }).await;
        perf::REAPPLY.notify_one();
        Ok(())
    }

    /// Whether Office mode uses the ECO power limits.
    async fn office_eco(&self) -> bool {
        perf::office_eco()
    }

    /// CPU boost clocks (cpufreq boost). Remembered per mode.
    async fn set_cpu_boost(&self, enabled: bool) -> fdo::Result<()> {
        let _guard = self.lock.lock().await;
        sysfs_write(PathBuf::from(perf::CPU_BOOST), u8::from(enabled).to_string())
            .await
            .map_err(to_fdo)?;
        modes::remember_current().await;
        Ok(())
    }

    /// Run both fans at full speed, independent of the mode.
    async fn set_fan_boost(&self, enabled: bool) -> fdo::Result<()> {
        let _guard = self.lock.lock().await;
        sysfs_write(perf::inou("fan_boost"), u8::from(enabled).to_string())
            .await
            .map_err(to_fdo)?;
        modes::remember_current().await;
        events::emit("fan_boost", events::on_off(enabled)).await;
        Ok(())
    }

    /// CPU package power in W, averaged over the last second.
    async fn package_power(&self) -> f64 {
        f64::from(perf::PACKAGE_MW.load(std::sync::atomic::Ordering::Relaxed)) / 1000.0
    }

    /// Enable or disable the touchpad in its firmware; disabled lights the
    /// touchpad's corner LED. The app mirrors GNOME's touchpad setting here.
    async fn set_touchpad(&self, enabled: bool) -> fdo::Result<()> {
        touchpad::set(enabled).await.map_err(to_fdo)
    }

    /// Whether the uniwill driver with keyboard support is loaded.
    #[zbus(property)]
    async fn available(&self) -> bool {
        Path::new(LED_DIR).exists()
    }

    /// Whether this daemon currently holds the sleep button.
    #[zbus(property)]
    async fn sleep_guard(&self) -> bool {
        SLEEP_GUARD_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// For on-screen displays: kind "profile" (low-power / balanced /
    /// performance), "fn_lock", "super_lock" or "fan_boost" ("on" / "off").
    #[zbus(signal)]
    pub(crate) async fn event(emitter: &SignalEmitter<'_>, kind: &str, value: &str)
        -> zbus::Result<()>;

    /// Emitted for the sleep key: "armed" (press again within timeout_ms to
    /// suspend), "cancelled" (window expired) or "suspending".
    #[zbus(signal)]
    async fn sleep_key(emitter: &SignalEmitter<'_>, state: &str, timeout_ms: u32)
        -> zbus::Result<()>;
}

static SLEEP_GUARD_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) async fn save_state(file: &'static str, value: &str) {
    let value = format!("{value}\n");
    let res = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(STATE_DIR)?;
        let tmp = format!("{file}.tmp");
        std::fs::write(&tmp, value)?;
        std::fs::rename(tmp, file)
    })
    .await;
    if let Ok(Err(e)) | Err(e) = res.map_err(std::io::Error::other) {
        eprintln!("rp15d: could not save {file}: {e}");
    }
}

pub(crate) fn read_bool(path: &str) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse::<u8>().ok().map(|v| v != 0)
}

/// Settings can also change with F2, Fn+Esc or F6/F7, so record the live state
/// for the active mode at shutdown.
async fn save_on_shutdown() {
    let Ok(mut term) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        return;
    };
    term.recv().await;
    modes::remember_current().await;
    std::process::exit(0);
}

fn find_sleep_button() -> Option<evdev::Device> {
    evdev::enumerate().map(|(_, dev)| dev).find(|dev| {
        dev.physical_path()
            .is_some_and(|p| p.starts_with("PNP0C0E/"))
            && dev
                .supported_keys()
                .is_some_and(|k| k.contains(evdev::KeyCode::KEY_SLEEP))
    })
}

async fn suspend(conn: &Connection) {
    let res = conn
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Suspend",
            &(false,),
        )
        .await;
    if let Err(e) = res {
        eprintln!("rp15d: suspend failed: {e}");
    }
}

/// F1 on the RP-15 is the ACPI sleep button (PNP0C0E), not a keyboard key.
/// Grab it so GNOME never sees it, and require a second press to suspend.
async fn sleep_guard(conn: Connection) {
    let Some(mut dev) = find_sleep_button() else {
        eprintln!("rp15d: no ACPI sleep button found, sleep guard disabled");
        return;
    };
    if let Err(e) = dev.grab() {
        eprintln!("rp15d: cannot grab sleep button: {e}");
        return;
    }
    let mut events = match dev.into_event_stream() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rp15d: cannot read sleep button: {e}");
            return;
        }
    };
    SLEEP_GUARD_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!("rp15d: sleep guard active");

    let iface = match conn
        .object_server()
        .interface::<_, Daemon>(OBJECT_PATH)
        .await
    {
        Ok(i) => i,
        Err(e) => {
            eprintln!("rp15d: {e}");
            return;
        }
    };
    let emitter = iface.signal_emitter().to_owned();
    let timeout_ms = SLEEP_CONFIRM.as_millis() as u32;
    let mut armed_until: Option<tokio::time::Instant> = None;

    loop {
        let deadline = armed_until;
        let event = tokio::select! {
            ev = events.next_event() => ev,
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => {
                armed_until = None;
                let _ = Daemon::sleep_key(&emitter, "cancelled", 0).await;
                continue;
            }
        };
        let event = match event {
            Ok(ev) => ev,
            Err(e) => {
                eprintln!("rp15d: sleep button read error: {e}");
                break;
            }
        };
        let is_press = event.event_type() == evdev::EventType::KEY
            && event.code() == evdev::KeyCode::KEY_SLEEP.code()
            && event.value() == 1;
        if !is_press {
            continue;
        }
        if armed_until.take().is_some() {
            let _ = Daemon::sleep_key(&emitter, "suspending", 0).await;
            suspend(&conn).await;
        } else {
            armed_until = Some(tokio::time::Instant::now() + SLEEP_CONFIRM);
            let _ = Daemon::sleep_key(&emitter, "armed", timeout_ms).await;
        }
    }
    SLEEP_GUARD_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let daemon = Daemon {
        lock: Mutex::new(()),
    };
    let conn = zbus::connection::Builder::system()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, daemon)?
        .build()
        .await?;

    tokio::spawn(save_on_shutdown());
    tokio::spawn(sleep_guard(conn.clone()));
    let iface = conn
        .object_server()
        .interface::<_, Daemon>(OBJECT_PATH)
        .await?;
    events::init(iface.signal_emitter().to_owned());
    tokio::spawn(events::watch_locks());
    tokio::spawn(perf::watch_profile());
    tokio::spawn(perf::watch_resume(conn.clone()));
    tokio::spawn(perf::watch_power_source(conn.clone()));
    tokio::spawn(perf::sample_package_power());

    std::future::pending::<()>().await;
    Ok(())
}
