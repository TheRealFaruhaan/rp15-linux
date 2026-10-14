//! Performance modes: power limits per platform profile, CPU boost, fan boost
//! and package power.
//!
//! The driver exposes the vendor modes as platform_profile (low-power = Office,
//! balanced = Gaming, performance = Beast); the power-mode button, GNOME and
//! the app all switch it there. On this board the mode byte mostly selects the
//! mode LEDs, so the performance itself comes from the power limits we apply
//! whenever the profile changes. Without them the firmware holds the CPU at
//! about 30 W sustained (see FINDINGS.md).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tokio::sync::Notify;
use zbus::Connection;

use crate::{modes, read_bool, save_state, sysfs_write};

pub const PROFILE: &str = "/sys/firmware/acpi/platform_profile";
pub const PROFILES: [&str; 3] = ["low-power", "balanced", "performance"];
const INOU: &str = "/sys/devices/platform/INOU0000:00";
pub const CPU_BOOST: &str = "/sys/devices/system/cpu/cpufreq/boost";
const RAPL: &str = "/sys/class/powercap/intel-rapl:0";

pub const ECO_FILE: &str = "/var/lib/rp15d/office_eco";
/// The last mode used on AC power and on battery.
const PROFILE_AC_FILE: &str = "/var/lib/rp15d/profile_ac";
const PROFILE_BATTERY_FILE: &str = "/var/lib/rp15d/profile_battery";

/// Wakes the profile watcher to apply the limits again (ECO toggled, resume).
pub static REAPPLY: Notify = Notify::const_new();
/// Latest package power in mW, sampled from RAPL.
pub static PACKAGE_MW: AtomicU32 = AtomicU32::new(0);

pub fn inou(attr: &str) -> PathBuf {
    Path::new(INOU).join(attr)
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

/// PL1 / PL2 / PL4 in W, the values the vendor software uses for each mode.
pub fn limits(profile: &str, office_eco: bool) -> Option<[u8; 3]> {
    match profile {
        "low-power" if office_eco => Some([25, 25, 65]),
        "low-power" => Some([45, 45, 65]),
        "balanced" => Some([54, 54, 65]),
        "performance" => Some([73, 73, 90]),
        _ => None,
    }
}

pub fn office_eco() -> bool {
    read_bool(ECO_FILE).unwrap_or(false)
}

async fn apply_limits() {
    let Some(profile) = read_trimmed(Path::new(PROFILE)) else {
        return;
    };
    let Some(pl) = limits(&profile, office_eco()) else {
        return;
    };
    let mut changed = false;
    for (name, value) in ["power_limit_pl1", "power_limit_pl2", "power_limit_pl4"]
        .into_iter()
        .zip(pl)
    {
        let path = inou(name);
        if read_trimmed(&path).and_then(|v| v.parse::<u8>().ok()) == Some(value) {
            continue;
        }
        if let Err(e) = sysfs_write(path, value.to_string()).await {
            eprintln!("rp15d: could not set {name}: {e}");
            return;
        }
        changed = true;
    }
    if changed {
        eprintln!(
            "rp15d: {profile}: power limits {}/{}/{} W",
            pl[0], pl[1], pl[2]
        );
    }
}

/// Re-read through the same descriptor: kernfs tracks poll events per open file.
pub fn reread(file: &File, buf: &mut String) -> bool {
    let mut f = file;
    buf.clear();
    f.seek(SeekFrom::Start(0)).is_ok() && f.read_to_string(buf).is_ok()
}

fn rearm(file: &File) -> bool {
    reread(file, &mut String::new())
}

/// Apply the power limits for the current profile now and on every change.
pub async fn watch_profile() {
    // The mode whose settings are on the hardware; None until the first entry.
    let mut active: Option<String> = None;
    loop {
        // The file only exists while the driver is loaded.
        let afd = File::open(PROFILE)
            .ok()
            .filter(rearm)
            // SAFETY: the AsyncFd owns the File, so the fd stays valid while registered.
            .and_then(|f| unsafe { AsyncFd::register_with_interest(f, Interest::PRIORITY) }.ok());
        let Some(afd) = afd else {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };
        loop {
            // Leaving a mode records its live settings, entering applies the new
            // mode's. The first entry after the daemon starts is the boot restore.
            if let Some(profile) = modes::current_profile() {
                if active.as_deref() != Some(profile.as_str()) {
                    let at_boot = active.is_none();
                    if let Some(old) = active.take() {
                        modes::remember(&old).await;
                    }
                    modes::enter(&profile, at_boot).await;
                    remember_for_power_source(&profile).await;
                    if !at_boot {
                        crate::events::emit("profile", &profile).await;
                    }
                    active = Some(profile);
                }
            }
            apply_limits().await;

            tokio::select! {
                ready = afd.ready(Interest::PRIORITY) => {
                    let Ok(mut guard) = ready else { break };
                    guard.clear_ready();
                    if !rearm(afd.get_ref()) {
                        break;
                    }
                }
                _ = REAPPLY.notified() => {}
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The EC may drop the power limits over suspend; apply them again on resume.
pub async fn watch_resume(conn: Connection) {
    let proxy = match zbus::Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rp15d: cannot watch logind: {e}");
            return;
        }
    };
    let Ok(mut signals) = proxy.receive_signal("PrepareForSleep").await else {
        return;
    };
    while let Some(msg) = signals.next().await {
        if let Ok(false) = msg.body().deserialize::<bool>() {
            REAPPLY.notify_one();
            crate::touchpad::reapply().await;
        }
    }
}

/// Package power from RAPL, which is root-only, published for clients.
pub async fn sample_package_power() {
    let rapl = Path::new(RAPL);
    let max = read_trimmed(&rapl.join("max_energy_range_uj"))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(u64::from(u32::MAX));
    let read = || read_trimmed(&rapl.join("energy_uj")).and_then(|v| v.parse::<u64>().ok());
    let mut last = read().map(|e| (e, tokio::time::Instant::now()));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let Some(e) = read() else { continue };
        let now = tokio::time::Instant::now();
        if let Some((le, lt)) = last {
            let dt = now.duration_since(lt).as_secs_f64();
            if dt > 0.0 {
                let uj = (e + max - le) % max;
                let mw = (uj as f64 / dt / 1000.0) as u32;
                PACKAGE_MW.store(mw, Ordering::Relaxed);
            }
        }
        last = Some((e, now));
    }
}

/// Whether the laptop runs on battery: no "Mains" supply reports online.
fn on_battery() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    let mut mains = dir.flatten().map(|e| e.path()).filter(|p| {
        read_trimmed(&p.join("type")).as_deref() == Some("Mains")
    });
    // No AC adapter device at all: treat as AC so nothing ever switches.
    let mut any = false;
    let online = mains.any(|p| {
        any = true;
        read_trimmed(&p.join("online")).as_deref() == Some("1")
    });
    any && !online
}

fn power_source_file(battery: bool) -> &'static str {
    if battery {
        PROFILE_BATTERY_FILE
    } else {
        PROFILE_AC_FILE
    }
}

/// Remember `profile` as the last mode used on the current power source.
async fn remember_for_power_source(profile: &str) {
    save_state(power_source_file(on_battery()), profile).await;
}

/// On plug / unplug, go back to the mode last used on that power source.
pub async fn watch_power_source(conn: Connection) {
    let proxy = match zbus::Proxy::new(
        &conn,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower",
        "org.freedesktop.UPower",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rp15d: cannot watch UPower: {e}");
            return;
        }
    };
    let mut changes = proxy.receive_property_changed::<bool>("OnBattery").await;
    let mut last = on_battery();
    while let Some(change) = changes.next().await {
        let Ok(battery) = change.get().await else { continue };
        if battery == last {
            continue;
        }
        last = battery;
        let Some(saved) = read_trimmed(Path::new(power_source_file(battery)))
            .filter(|p| PROFILES.contains(&p.as_str()))
        else {
            continue;
        };
        if modes::current_profile().as_deref() == Some(saved.as_str()) {
            continue;
        }
        let source = if battery { "battery" } else { "AC" };
        match sysfs_write(PathBuf::from(PROFILE), saved.clone()).await {
            Ok(()) => eprintln!("rp15d: on {source}: back to {saved}"),
            Err(e) => eprintln!("rp15d: could not switch to {saved}: {e}"),
        }
    }
}
