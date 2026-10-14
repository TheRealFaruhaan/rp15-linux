//! Per-mode settings: each performance mode (Office / Gaming / Beast) keeps its
//! own keyboard colour and brightness, Fn lock, Super key lock, CPU boost and
//! fan boost, like the vendor's Control Center.
//!
//! The settings are captured from the live hardware when a mode is left (so
//! changes made with F2, Fn+Esc or F6/F7 are kept too) and on every change made
//! through the daemon, and applied when a mode is entered. At boot the Super
//! key always starts unlocked.

use std::path::{Path, PathBuf};

use crate::{perf, read_bool, sysfs_write};

const MODES_DIR: &str = "/var/lib/rp15d/modes";
const LED_DIR: &str = "/sys/class/leds/uniwill::kbd_backlight";

/// Files from before per-mode settings, used once to seed the first mode.
const LEGACY_COLOR: &str = "/var/lib/rp15d/kbd_color";
const LEGACY_FN_LOCK: &str = "/var/lib/rp15d/fn_lock";
const LEGACY_CPU_BOOST: &str = "/var/lib/rp15d/cpu_boost";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub color: Option<(u8, u8, u8)>,
    pub brightness: Option<u8>,
    pub fn_lock: Option<bool>,
    pub super_lock: Option<bool>,
    pub cpu_boost: Option<bool>,
    pub fan_boost: Option<bool>,
}

fn led(attr: &str) -> PathBuf {
    Path::new(LED_DIR).join(attr)
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

fn parse_color(text: &str) -> Option<(u8, u8, u8)> {
    let mut it = text.split_whitespace().map(|v| v.parse::<u8>().ok());
    let c = (it.next()??, it.next()??, it.next()??);
    let max = crate::MAX_INTENSITY;
    (c.0 <= max && c.1 <= max && c.2 <= max).then_some(c)
}

fn flag(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

impl Settings {
    /// What the hardware is set to right now.
    pub fn live() -> Self {
        Settings {
            color: read_trimmed(&led("multi_intensity")).and_then(|t| parse_color(&t)),
            brightness: read_trimmed(&led("brightness")).and_then(|v| v.parse().ok()),
            fn_lock: read_bool(perf::inou("fn_lock").to_str().unwrap_or_default()),
            super_lock: read_bool(perf::inou("super_key_enable").to_str().unwrap_or_default())
                .map(|enabled| !enabled),
            cpu_boost: read_bool(perf::CPU_BOOST),
            fan_boost: read_bool(perf::inou("fan_boost").to_str().unwrap_or_default()),
        }
    }

    fn parse(text: &str) -> Self {
        let mut s = Settings::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let b = value.parse::<u8>().ok().map(|v| v != 0);
            match key.trim() {
                "color" => s.color = parse_color(value),
                "brightness" => s.brightness = value.parse().ok().filter(|&v| v <= 4),
                "fn_lock" => s.fn_lock = b,
                "super_lock" => s.super_lock = b,
                "cpu_boost" => s.cpu_boost = b,
                "fan_boost" => s.fan_boost = b,
                _ => {}
            }
        }
        s
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        if let Some((r, g, b)) = self.color {
            out += &format!("color={r} {g} {b}\n");
        }
        if let Some(v) = self.brightness {
            out += &format!("brightness={v}\n");
        }
        for (key, value) in [
            ("fn_lock", self.fn_lock),
            ("super_lock", self.super_lock),
            ("cpu_boost", self.cpu_boost),
            ("fan_boost", self.fan_boost),
        ] {
            if let Some(v) = value {
                out += &format!("{key}={}\n", flag(v));
            }
        }
        out
    }
}

fn path(profile: &str) -> PathBuf {
    Path::new(MODES_DIR).join(profile)
}

fn load(profile: &str) -> Option<Settings> {
    std::fs::read_to_string(path(profile))
        .ok()
        .map(|t| Settings::parse(&t))
}

async fn save(profile: &str, settings: &Settings) {
    let file = path(profile);
    let text = settings.serialize();
    let res = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(MODES_DIR)?;
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, file)
    })
    .await;
    if let Ok(Err(e)) | Err(e) = res.map_err(std::io::Error::other) {
        eprintln!("rp15d: could not save {profile} settings: {e}");
    }
}

pub fn current_profile() -> Option<String> {
    read_trimmed(Path::new(perf::PROFILE)).filter(|p| perf::PROFILES.contains(&p.as_str()))
}

/// Record the live hardware state as the settings of `profile`.
pub async fn remember(profile: &str) {
    save(profile, &Settings::live()).await;
}

/// Record the live state for whatever mode is active.
pub async fn remember_current() {
    if let Some(p) = current_profile() {
        remember(&p).await;
    }
}

async fn write_if<T: PartialEq + ToString>(path: PathBuf, want: Option<T>, have: Option<T>) {
    let Some(want) = want else { return };
    if have.as_ref() == Some(&want) {
        return;
    }
    if let Err(e) = sysfs_write(path.clone(), want.to_string()).await {
        eprintln!("rp15d: could not set {}: {e}", path.display());
    }
}

async fn apply(s: &Settings, at_boot: bool) {
    let live = Settings::live();
    // Colour before brightness, so the light never flashes the old colour.
    if s.color.is_some() && s.color != live.color {
        let (r, g, b) = s.color.unwrap_or_default();
        if let Err(e) = sysfs_write(led("multi_intensity"), format!("{r} {g} {b}")).await {
            eprintln!("rp15d: could not set keyboard colour: {e}");
        }
    }
    write_if(led("brightness"), s.brightness, live.brightness).await;
    write_if(perf::inou("fn_lock"), s.fn_lock.map(u8::from), live.fn_lock.map(u8::from)).await;
    let super_lock = if at_boot { Some(false) } else { s.super_lock };
    write_if(
        perf::inou("super_key_enable"),
        super_lock.map(|l| u8::from(!l)),
        live.super_lock.map(|l| u8::from(!l)),
    )
    .await;
    write_if(PathBuf::from(perf::CPU_BOOST), s.cpu_boost.map(u8::from), live.cpu_boost.map(u8::from))
        .await;
    write_if(perf::inou("fan_boost"), s.fan_boost.map(u8::from), live.fan_boost.map(u8::from))
        .await;
}

/// Seed the first mode from the files used before per-mode settings existed.
fn legacy_settings() -> Option<Settings> {
    let color = read_trimmed(Path::new(LEGACY_COLOR)).and_then(|t| parse_color(&t));
    let fn_lock = read_bool(LEGACY_FN_LOCK);
    let cpu_boost = read_bool(LEGACY_CPU_BOOST);
    if color.is_none() && fn_lock.is_none() && cpu_boost.is_none() {
        return None;
    }
    let live = Settings::live();
    Some(Settings {
        color: color.or(live.color),
        fn_lock: fn_lock.or(live.fn_lock),
        cpu_boost: cpu_boost.or(live.cpu_boost),
        ..live
    })
}

/// Switch to `profile`'s settings. A mode visited for the first time starts
/// from the current state, so nothing changes unexpectedly.
pub async fn enter(profile: &str, at_boot: bool) {
    let settings = match load(profile) {
        Some(s) => s,
        None => {
            let seed = if at_boot { legacy_settings() } else { None };
            let s = seed.unwrap_or_else(Settings::live);
            save(profile, &s).await;
            s
        }
    };
    crate::events::quiet_for(std::time::Duration::from_secs(2));
    apply(&settings, at_boot).await;
    if at_boot {
        // Every boot starts with the Super key unlocked; F2 or the app lock it
        // again, and that is remembered for the mode from then on.
        eprintln!("rp15d: restored {profile} settings (Super key unlocked)");
    } else {
        eprintln!("rp15d: switched to {profile} settings");
    }
}
