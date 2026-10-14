//! Client side of rp15d's D-Bus API.

#[zbus::proxy(
    interface = "io.rp15.Daemon1",
    default_service = "io.rp15.Daemon",
    default_path = "/io/rp15/Daemon"
)]
pub trait Daemon {
    fn set_kbd_color(&self, red: u8, green: u8, blue: u8) -> zbus::Result<()>;
    fn set_kbd_brightness(&self, level: u8) -> zbus::Result<()>;
    fn set_fn_lock(&self, enabled: bool) -> zbus::Result<()>;
    fn set_super_key(&self, enabled: bool) -> zbus::Result<()>;
    fn set_profile(&self, profile: &str) -> zbus::Result<()>;
    fn set_office_eco(&self, enabled: bool) -> zbus::Result<()>;
    fn set_cpu_boost(&self, enabled: bool) -> zbus::Result<()>;
    fn set_fan_boost(&self, enabled: bool) -> zbus::Result<()>;
    fn set_touchpad(&self, enabled: bool) -> zbus::Result<()>;
    fn package_power(&self) -> zbus::Result<f64>;

    #[zbus(property)]
    fn available(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn sleep_guard(&self) -> zbus::Result<bool>;

    #[zbus(signal)]
    fn sleep_key(&self, state: &str, timeout_ms: u32) -> zbus::Result<()>;
}

pub async fn connect() -> zbus::Result<DaemonProxy<'static>> {
    let conn = zbus::Connection::system().await?;
    DaemonProxy::new(&conn).await
}
