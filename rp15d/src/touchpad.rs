//! Touchpad lock with its LED.
//!
//! The RP-15 touchpad (UNIW0001, I2C HID Precision Touchpad) lights its corner
//! LED and stops reporting when its Selective Reporting feature report (0x07:
//! bit0 SurfaceSwitch, bit1 ButtonSwitch) is cleared, which is how Windows
//! disables it. hid-multitouch sets both bits on init and resume, so the
//! wanted state is re-applied after resume.

use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

const NAME_PREFIX: &str = "HID_NAME=UNIW0001:";
const REPORT_ID: u8 = 0x07;
const BOTH_SWITCHES: u8 = 0x03;

/// The state last asked for; the touchpad starts enabled.
static ENABLED: AtomicBool = AtomicBool::new(true);

fn find_hidraw() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/hidraw")
        .ok()?
        .flatten()
        .find(|e| {
            std::fs::read_to_string(e.path().join("device/uevent"))
                .is_ok_and(|u| u.lines().any(|l| l.starts_with(NAME_PREFIX)))
        })
        .map(|e| PathBuf::from("/dev").join(e.file_name()))
}

/// HIDIOCSFEATURE(len): _IOC(_IOC_WRITE | _IOC_READ, 'H', 0x06, len)
fn hidiocsfeature(len: usize) -> libc::c_ulong {
    (3 << 30) | ((len as libc::c_ulong) << 16) | ((b'H' as libc::c_ulong) << 8) | 0x06
}

fn write_switches(enabled: bool) -> std::io::Result<()> {
    let path = find_hidraw()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "touchpad not found"))?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut buf = [REPORT_ID, if enabled { BOTH_SWITCHES } else { 0 }];
    // SAFETY: buf is valid for the length encoded in the request.
    let ret = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            hidiocsfeature(buf.len()) as _,
            buf.as_mut_ptr(),
        )
    };
    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub async fn set(enabled: bool) -> std::io::Result<()> {
    ENABLED.store(enabled, Ordering::Relaxed);
    tokio::task::spawn_blocking(move || write_switches(enabled))
        .await
        .map_err(std::io::Error::other)?
}

/// After resume hid-multitouch has turned the switches back on.
pub async fn reapply() {
    if !ENABLED.load(Ordering::Relaxed) {
        // Give hid-multitouch time to finish its own resume first.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if let Err(e) = set(false).await {
            eprintln!("rp15d: could not re-apply touchpad lock: {e}");
        }
    }
}
