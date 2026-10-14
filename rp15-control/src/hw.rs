//! Read-only hardware state. Everything here is world-readable sysfs.

use std::fs;
use std::path::{Path, PathBuf};

const LED_DIR: &str = "/sys/class/leds/uniwill::kbd_backlight";
pub const FN_LOCK: &str = "/sys/devices/platform/INOU0000:00/fn_lock";
pub const SUPER_KEY: &str = "/sys/devices/platform/INOU0000:00/super_key_enable";
const INOU: &str = "/sys/devices/platform/INOU0000:00";
const PROFILE: &str = "/sys/firmware/acpi/platform_profile";
const CPU_BOOST: &str = "/sys/devices/system/cpu/cpufreq/boost";
const OFFICE_ECO: &str = "/var/lib/rp15d/office_eco";

/// The EC's colour range per channel.
pub const MAX_INTENSITY: u8 = 50;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Thermals {
    pub cpu_temp: Option<f64>,
    pub gpu_temp: Option<f64>,
    pub cpu_fan_rpm: Option<u32>,
    pub gpu_fan_rpm: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Keyboard {
    pub brightness: u8,
    pub max_brightness: u8,
    /// Raw EC values, 0-50 each.
    pub color: (u8, u8, u8),
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

fn read_num<T: std::str::FromStr>(path: &Path) -> Option<T> {
    read_trimmed(path)?.parse().ok()
}

/// The uniwill-laptop hwmon device, found by name since the index can change.
fn uniwill_hwmon() -> Option<PathBuf> {
    fs::read_dir("/sys/class/hwmon")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| read_trimmed(&p.join("name")).as_deref() == Some("uniwill"))
}

pub fn thermals() -> Thermals {
    let Some(h) = uniwill_hwmon() else {
        return Thermals::default();
    };
    let temp = |n: &str| read_num::<f64>(&h.join(n)).map(|m| m / 1000.0);
    Thermals {
        cpu_temp: temp("temp1_input"),
        gpu_temp: temp("temp2_input"),
        cpu_fan_rpm: read_num(&h.join("fan1_input")),
        gpu_fan_rpm: read_num(&h.join("fan2_input")),
    }
}

pub fn keyboard() -> Option<Keyboard> {
    let dir = Path::new(LED_DIR);
    let text = read_trimmed(&dir.join("multi_intensity"))?;
    let mut it = text.split_whitespace().filter_map(|v| v.parse::<u8>().ok());
    let color = (it.next()?, it.next()?, it.next()?);
    Some(Keyboard {
        brightness: read_num(&dir.join("brightness"))?,
        max_brightness: read_num(&dir.join("max_brightness"))?,
        color,
    })
}

pub fn fn_lock() -> Option<bool> {
    read_num::<u8>(Path::new(FN_LOCK)).map(|v| v != 0)
}

/// `false` while the Super key is locked (F2).
pub fn super_key() -> Option<bool> {
    read_num::<u8>(Path::new(SUPER_KEY)).map(|v| v != 0)
}

/// 0-50 EC value to 0.0-1.0.
pub fn ec_to_unit(v: u8) -> f32 {
    f32::from(v.min(MAX_INTENSITY)) / f32::from(MAX_INTENSITY)
}

/// 0.0-1.0 to 0-50 EC value.
pub fn unit_to_ec(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * f32::from(MAX_INTENSITY)).round() as u8
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Perf {
    pub profile: Option<String>,
    pub cpu_boost: Option<bool>,
    pub fan_boost: Option<bool>,
    pub office_eco: bool,
    /// Active PL1 override in W (0 = firmware default).
    pub pl1: Option<u32>,
}

pub fn perf() -> Perf {
    let inou = Path::new(INOU);
    let flag = |p: &Path| read_num::<u8>(p).map(|v| v != 0);
    Perf {
        profile: read_trimmed(Path::new(PROFILE)),
        cpu_boost: flag(Path::new(CPU_BOOST)),
        fan_boost: flag(&inou.join("fan_boost")),
        office_eco: flag(Path::new(OFFICE_ECO)).unwrap_or(false),
        pl1: read_num(&inou.join("power_limit_pl1")),
    }
}
