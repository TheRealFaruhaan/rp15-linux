# RP-15 Linux

Full Linux support for the **Eluktronics RP-15** (Red Pill Gen 1, Tongfang/Uniwill,
Ryzen 7 4800H + RTX 2060), replacing the Windows Control Center.

## Install

From a release tarball (contains compiled programs, no Rust needed):

```sh
tar xf rp15-linux-1.0.0-x86_64.tar.gz
cd rp15-linux-1.0.0-x86_64
sudo ./install.sh
```

The installer:

- installs the required packages (DKMS, kernel headers, GTK4/libadwaita runtime);
- builds and installs the driver with DKMS, so it is rebuilt after kernel updates;
- with **Secure Boot** on, creates a signing key if needed and queues it for
  enrollment. Reboot, and on the blue *MOK management* screen choose
  Enroll MOK > Continue > Yes > password > Reboot;
- installs and starts the `rp15d` service, the app, the GNOME Shell extension and
  the F5 keyboard remap;
- is safe to run again (updates in place).

Log out and back in afterwards. Then open **RP-15 Control** from the app grid.

Uninstall: `sudo /usr/local/share/rp15-linux/install.sh --uninstall` (add `--purge`
to also delete saved settings).

Requires Ubuntu 25.04 or newer (libadwaita 1.7), GNOME Shell 50, x86_64.

## What you get

| | |
|---|---|
| Performance modes | Office / Gaming / Beast with the vendor power limits (45 or ECO 25 / 54 / 73 W), switched by the power-mode button (its LEDs follow), GNOME's power menu or the app |
| Per-mode settings | each mode remembers keyboard colour and brightness, Fn lock, Super key lock, CPU boost and fan boost |
| AC / battery | the last mode used on each power source is restored on plug / unplug |
| Fans | live RPM, Fan boost (both fans at full speed) |
| Thermals | CPU / GPU temperature, live CPU package power |
| Keyboard | RGB backlight colour and brightness (F6/F7), restored at boot |
| Fn lock, Super lock | Fn+Esc / F2, also from the app, with on-screen displays |
| Touchpad lock | F5, with the touchpad's corner LED, like on Windows |
| Sleep key | F1 asks for a second press before suspending |
| On-screen displays | coloured, animated OSDs for all of the above (GNOME Shell extension) |

## Layout

```
driver/           patched mainline uniwill-laptop (DKMS package uniwill-rp15)
  upstream/       the unmodified v7.0 sources and rp15.patch against them
rp15d/            root system service (Rust): the only part that writes hardware
rp15-control/     GTK4 + libadwaita app (Rust); --background shows prompts
shell-extension/  RP-15 OSD (GNOME Shell 50)
data/             D-Bus policy, systemd unit, hwdb, desktop entries, icon
install.sh        installer / uninstaller
Makefile          make (build), make dist (release tarball)
```

### rp15d

D-Bus `io.rp15.Daemon` / `io.rp15.Daemon1` on the system bus. Reads need nothing,
because sysfs is world-readable. It handles:

- range-checked writes: keyboard colour (0-50 per channel) and brightness (0-4),
  Fn lock, Super key, CPU boost, fan boost, performance mode, Office ECO, touchpad;
- power limits for each mode (driver `power_limit_pl1/2/4`). It re-applies them after resume,
  and publishes package power from RAPL (root-only);
- per-mode settings in `/var/lib/rp15d/modes/<profile>`. They are recorded when a mode is
  left (including changes made with the keys) and applied when one is entered. The
  Super key is always unlocked at boot;
- last mode on AC and on battery (`profile_ac`, `profile_battery`), restored on plug and unplug
  via UPower;
- grabs the ACPI sleep button (F1) and emits `SleepKey("armed")`. A second press
  within 3 s suspends. If rp15d is not running, F1 behaves as before;
- touchpad lock through the touchpad's own HID feature report 0x07, which also lights its LED;
- `Event(kind, value)` signals for the OSD extension.

### Driver changes (against mainline v7.0)

An RP-15 DMI entry with fan/temperature hwmon, Fn lock, Super key, an RGB keyboard
LED (`uniwill::kbd_backlight`), and performance modes as `platform_profile`
(power-mode button cycles them), `fan_boost` and `power_limit_pl1/2/4`. Mainline's
NVIDIA cTGP feature must stay off on this board: its registers are the manual fan
levels here. See `driver/upstream/rp15.patch`.

## Build from source

```sh
sudo apt install cargo rustc pkg-config libgtk-4-dev libadwaita-1-dev
make            # target/release/{rp15d,rp15-control}
make dist       # dist/rp15-linux-<version>-x86_64.tar.gz
sudo ./install.sh   # also works straight from the source tree after 'make'
```

Development screenshot of the app window (renders to a PNG and quits; uses a
separate app ID, so it does not touch an installed instance):

```sh
RP15_SCREENSHOT=/tmp/shot.png ./target/debug/rp15-control
```

The reverse-engineering notes this is based on are in `../FINDINGS.md`.
