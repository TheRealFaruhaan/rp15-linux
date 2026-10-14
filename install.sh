#!/bin/bash
# RP-15 Linux installer: everything for the Eluktronics RP-15 on Ubuntu.
#
#   sudo ./install.sh                 install or update
#   sudo ./install.sh --uninstall     remove everything (keeps saved settings)
#   sudo ./install.sh --uninstall --purge   also remove saved settings
#
# Options:
#   --force   install on a machine that is not an Eluktronics RP-15
#
# Installs the patched uniwill-laptop driver (DKMS, rebuilt on kernel updates,
# signed for Secure Boot), the rp15d system service, the RP-15 Control app, the
# RP-15 OSD GNOME Shell extension and the keyboard remap. Safe to run again.

set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
VERSION=$(cat "$HERE/VERSION")
DRIVER=uniwill-rp15
MODULE=uniwill_laptop
UUID=rp15-osd@rp15.io
PREFIX=/usr/local
STATE_DIR=/var/lib/rp15d
SHARE=$PREFIX/share/rp15-linux
MOK_DIR=/var/lib/shim-signed/mok

# Everything installed outside of DKMS, for --uninstall.
FILES=(
    "$PREFIX/libexec/rp15d"
    "$PREFIX/bin/rp15-control"
    /usr/share/dbus-1/system.d/io.rp15.Daemon.conf
    /etc/systemd/system/rp15d.service
    /etc/udev/hwdb.d/61-rp15-keyboard.hwdb
    /etc/dkms/framework.conf.d/rp15-linux.conf
    "$PREFIX/share/applications/io.rp15.Control.desktop"
    /etc/xdg/autostart/io.rp15.Control-background.desktop
    "$PREFIX/share/icons/hicolor/scalable/apps/io.rp15.Control.svg"
)

REBOOT_REASONS=()
RELOGIN=0
WARNINGS=()

if [ -t 1 ]; then
    B=$'\e[1m' G=$'\e[32m' Y=$'\e[33m' R=$'\e[31m' N=$'\e[0m'
else
    B='' G='' Y='' R='' N=''
fi
step() { echo "${B}==>${N} ${B}$*${N}"; }
info() { echo "    $*"; }
warn() { echo "${Y}warning:${N} $*" >&2; WARNINGS+=("$*"); }
die() { echo "${R}error:${N} $*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }
systemd_running() { [ -d /run/systemd/system ]; }

# ---------------------------------------------------------------- options

MODE=install
PURGE=0
FORCE=0
for arg in "$@"; do
    case "$arg" in
        --uninstall) MODE=uninstall ;;
        --purge) PURGE=1 ;;
        --force) FORCE=1 ;;
        -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option: $arg (see --help)" ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    exec sudo -- "$0" "$@"
fi

# The desktop user the per-user parts (Shell extension) are set up for.
TARGET_USER=${SUDO_USER:-}
[ "$TARGET_USER" = root ] && TARGET_USER=
if [ -n "$TARGET_USER" ]; then
    TARGET_UID=$(id -u "$TARGET_USER")
    TARGET_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
fi

# Run a command as the desktop user, on their session bus when they are logged
# in, otherwise on a private bus (gsettings then writes their dconf database).
as_user() {
    local bus=/run/user/$TARGET_UID/bus
    if [ -S "$bus" ]; then
        runuser -u "$TARGET_USER" -- env DBUS_SESSION_BUS_ADDRESS="unix:path=$bus" "$@"
    else
        runuser -u "$TARGET_USER" -- dbus-run-session -- "$@"
    fi
}

# ------------------------------------------------------------- shell extension

set_extension_enabled() {
    local enable=$1 current new
    [ -n "$TARGET_USER" ] || return 0
    have gsettings || return 1
    current=$(as_user gsettings get org.gnome.shell enabled-extensions 2>/dev/null) || return 1
    # current looks like ['a@b', 'c@d'] or @as []
    new=$(python3 -I - "$current" "$UUID" "$enable" <<'EOF'
import ast, sys
cur, uuid, enable = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
items = [] if cur.strip().startswith("@as") else ast.literal_eval(cur)
items = [i for i in items if i != uuid] + ([uuid] if enable else [])
print("[" + ", ".join(repr(i) for i in items) + "]")
EOF
    )
    [ "$new" = "$current" ] || as_user gsettings set org.gnome.shell enabled-extensions "$new"
    if [ "$enable" = 1 ]; then
        # Make sure it is not also listed as disabled, and that user
        # extensions are allowed.
        current=$(as_user gsettings get org.gnome.shell disabled-extensions 2>/dev/null || echo '@as []')
        new=$(python3 -I - "$current" "$UUID" 0 <<'EOF'
import ast, sys
cur, uuid = sys.argv[1], sys.argv[2]
items = [] if cur.strip().startswith("@as") else ast.literal_eval(cur)
print("[" + ", ".join(repr(i) for i in items if i != uuid) + "]")
EOF
        )
        [ "$new" = "$current" ] || as_user gsettings set org.gnome.shell disabled-extensions "$new"
        as_user gsettings set org.gnome.shell disable-user-extensions false 2>/dev/null || true
    fi
}

# ===================================================================== uninstall

if [ "$MODE" = uninstall ]; then
    step "Uninstalling RP-15 Linux"
    if systemd_running; then
        systemctl disable --now rp15d.service 2>/dev/null || true
    else
        systemctl disable rp15d.service 2>/dev/null || true
    fi
    pkill -x rp15-control 2>/dev/null || true

    for f in "${FILES[@]}"; do rm -f "$f"; done
    rm -rf "$PREFIX/share/gnome-shell/extensions/$UUID" "$SHARE"
    set_extension_enabled 0 || true

    if have systemd-hwdb; then
        systemd-hwdb update
        udevadm trigger --subsystem-match=input --action=change 2>/dev/null || true
    fi
    gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    update-desktop-database -q "$PREFIX/share/applications" 2>/dev/null || true

    if have dkms; then
        for v in $(dkms status -m "$DRIVER" 2>/dev/null | sed -n "s|^$DRIVER/\([^,:]*\).*|\1|p" | sort -u); do
            dkms remove -m "$DRIVER" -v "$v" --all >/dev/null 2>&1 || true
            rm -rf "/usr/src/$DRIVER-$v"
        done
    fi
    # Back to the in-kernel driver (which does not bind on the RP-15).
    modprobe -r "$MODULE" 2>/dev/null || true
    depmod -a 2>/dev/null || true
    if systemd_running; then
        systemctl daemon-reload
        systemctl try-restart power-profiles-daemon.service 2>/dev/null || true
    fi

    if [ "$PURGE" = 1 ]; then
        rm -rf "$STATE_DIR"
        info "removed saved settings ($STATE_DIR)"
    else
        info "kept saved settings in $STATE_DIR (use --purge to remove them)"
    fi
    info "The Secure Boot key stays enrolled; it is harmless and used by other DKMS modules."
    echo "${G}RP-15 Linux has been removed.${N} Log out and back in to unload the Shell extension."
    exit 0
fi

# ======================================================================= install

echo "${B}RP-15 Linux $VERSION${N}"

# --------------------------------------------------------------- preflight

step "Checking the system"

vendor=$(cat /sys/class/dmi/id/sys_vendor 2>/dev/null || true)
product=$(cat /sys/class/dmi/id/product_name 2>/dev/null || true)
if [ "$vendor" != "Eluktronics Inc." ] || [ "$product" != "RP-15" ]; then
    [ "$FORCE" = 1 ] || die "this is a '$vendor $product', not an Eluktronics RP-15 (use --force to install anyway)"
    warn "installing on '$vendor $product' because of --force; the driver will not bind here"
fi
info "machine: $vendor $product"

have apt-get || die "this installer needs an apt-based system (Ubuntu)"
# In a subshell: os-release defines its own VERSION.
info "system: $(. /etc/os-release && echo "${PRETTY_NAME:-unknown}"), kernel $(uname -r)"

BIN=
for d in "$HERE/bin" "$HERE/target/release"; do
    if [ -x "$d/rp15d" ] && [ -x "$d/rp15-control" ]; then BIN=$d; break; fi
done
[ -n "$BIN" ] || die "no compiled programs found: use a release tarball, or run 'make' in the source tree first"
info "programs: $BIN"

# --------------------------------------------------------------- packages

step "Installing required packages"

KERNEL=$(uname -r)
PKGS=(dkms build-essential "linux-headers-$KERNEL" mokutil openssl
      libgtk-4-1 libadwaita-1-0 power-profiles-daemon python3)
# Headers for future kernels, so DKMS can rebuild the driver after updates.
while read -r meta; do
    [ -n "$meta" ] || continue
    hdr=${meta/linux-image-/linux-headers-}
    apt-cache show "$hdr" >/dev/null 2>&1 && PKGS+=("$hdr")
done < <(dpkg-query -W -f='${Package} ${Status}\n' 'linux-image-generic*' 2>/dev/null \
         | awk '$NF == "installed" {print $1}')

missing=()
for p in "${PKGS[@]}"; do
    dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed' || missing+=("$p")
done
if [ ${#missing[@]} -gt 0 ]; then
    info "installing: ${missing[*]}"
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends "${missing[@]}" >/dev/null \
        || die "could not install: ${missing[*]}"
else
    info "all present"
fi

# The app needs libadwaita 1.7 or newer.
adw=$(dpkg-query -W -f='${Version}' libadwaita-1-0 2>/dev/null | cut -d- -f1)
if dpkg --compare-versions "${adw:-0}" lt 1.7; then
    die "libadwaita $adw is too old (needs 1.7, Ubuntu 25.04 or newer)"
fi

# ------------------------------------------------------------ secure boot

step "Secure Boot"

SB=off
if [ -d /sys/firmware/efi ] && have mokutil && mokutil --sb-state 2>/dev/null | grep -q 'SecureBoot enabled'; then
    SB=on
fi
if [ "$SB" = off ]; then
    info "disabled, no module signing needed"
else
    mkdir -p "$MOK_DIR"
    if [ ! -s "$MOK_DIR/MOK.der" ] || [ ! -s "$MOK_DIR/MOK.priv" ]; then
        info "creating a module signing key in $MOK_DIR"
        openssl req -new -x509 -newkey rsa:2048 -nodes -days 36500 -outform DER \
            -keyout "$MOK_DIR/MOK.priv" -out "$MOK_DIR/MOK.der" \
            -subj "/CN=$(hostname) Secure Boot Module Signature key/" \
            -addext "extendedKeyUsage=codeSigning,1.3.6.1.4.1.2312.16.1.2" \
            -addext "keyUsage=digitalSignature" >/dev/null 2>&1 \
            || die "could not create a signing key"
        chmod 600 "$MOK_DIR/MOK.priv"
    fi
    # Tell DKMS to sign with this key (Ubuntu already does; Debian does not).
    mkdir -p /etc/dkms/framework.conf.d
    printf 'mok_signing_key=%s\nmok_certificate=%s\n' "$MOK_DIR/MOK.priv" "$MOK_DIR/MOK.der" \
        > /etc/dkms/framework.conf.d/rp15-linux.conf

    # mokutil --test-key exits non-zero in both cases, so read its text.
    status=$(mokutil --test-key "$MOK_DIR/MOK.der" 2>&1 || true)
    if echo "$status" | grep -q 'already enrolled'; then
        info "enabled, signing key already enrolled"
    elif echo "$status" | grep -q 'enrollment request'; then
        info "enabled, signing key enrollment already pending"
        REBOOT_REASONS+=("enroll the Secure Boot key (MOK Manager, see below)")
    else
        echo
        echo "    Secure Boot is on, so the driver must be signed with a key the firmware"
        echo "    trusts. Choose a one-time password now; after a reboot, a blue"
        echo "    'MOK management' screen asks for it (press a key within 10 seconds,"
        echo "    then Enroll MOK > Continue > Yes > password > Reboot)."
        echo
        if ! mokutil --import "$MOK_DIR/MOK.der"; then
            die "key enrollment was not queued; run the installer again"
        fi
        REBOOT_REASONS+=("enroll the Secure Boot key (MOK Manager, see below)")
    fi
fi

# ----------------------------------------------------------------- driver

step "Kernel driver ($DRIVER $VERSION, DKMS)"

for v in $(dkms status -m "$DRIVER" 2>/dev/null | sed -n "s|^$DRIVER/\([^,:]*\).*|\1|p" | sort -u); do
    info "removing previous version $v"
    dkms remove -m "$DRIVER" -v "$v" --all >/dev/null 2>&1 || true
    rm -rf "/usr/src/$DRIVER-$v"
done
SRC=/usr/src/$DRIVER-$VERSION
rm -rf "$SRC"
install -d "$SRC"
install -m644 "$HERE"/driver/Makefile "$HERE"/driver/dkms.conf "$HERE"/driver/*.c "$HERE"/driver/*.h "$SRC/"

dkms add -m "$DRIVER" -v "$VERSION" >/dev/null
if ! dkms build -m "$DRIVER" -v "$VERSION" -k "$KERNEL" >/tmp/rp15-dkms-build.log 2>&1; then
    tail -20 /tmp/rp15-dkms-build.log >&2
    die "the driver failed to build (log: /tmp/rp15-dkms-build.log)"
fi
dkms install -m "$DRIVER" -v "$VERSION" -k "$KERNEL" --force >/dev/null
info "built and installed for $KERNEL"

DRIVER_LOADED=0
if [ -e /sys/devices/platform/INOU0000:00 ] || [ "$FORCE" = 0 ]; then
    modprobe -r "$MODULE" 2>/dev/null || true
    if modprobe "$MODULE" 2>/tmp/rp15-modprobe.log; then
        DRIVER_LOADED=1
        info "loaded"
    elif grep -qi 'key was rejected\|required key not available' /tmp/rp15-modprobe.log; then
        info "not loaded yet: Secure Boot needs the key enrolled first"
        [ ${#REBOOT_REASONS[@]} -gt 0 ] || REBOOT_REASONS+=("load the signed driver")
    else
        warn "the driver did not load now: $(cat /tmp/rp15-modprobe.log)"
        REBOOT_REASONS+=("load the driver")
    fi
fi

# --------------------------------------------------------------- programs

step "Programs and system files"

install -Dm755 "$BIN/rp15d" "$PREFIX/libexec/rp15d"
install -Dm755 "$BIN/rp15-control" "$PREFIX/bin/rp15-control"
install -Dm644 "$HERE/data/io.rp15.Daemon.conf" /usr/share/dbus-1/system.d/io.rp15.Daemon.conf
install -Dm644 "$HERE/data/rp15d.service" /etc/systemd/system/rp15d.service
install -Dm644 "$HERE/data/61-rp15-keyboard.hwdb" /etc/udev/hwdb.d/61-rp15-keyboard.hwdb
install -Dm644 "$HERE/data/io.rp15.Control.desktop" "$PREFIX/share/applications/io.rp15.Control.desktop"
install -Dm644 "$HERE/data/io.rp15.Control-background.desktop" /etc/xdg/autostart/io.rp15.Control-background.desktop
install -Dm644 "$HERE/data/io.rp15.Control.svg" "$PREFIX/share/icons/hicolor/scalable/apps/io.rp15.Control.svg"
info "rp15d, RP-15 Control, D-Bus policy, systemd unit, desktop entries, icon"

if have systemd-hwdb; then
    systemd-hwdb update
    udevadm trigger --subsystem-match=input --action=change 2>/dev/null || true
    info "keyboard remap (F5 touchpad lock)"
else
    warn "systemd-hwdb not found; the F5 remap applies after it is available and 'systemd-hwdb update' runs"
fi

gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$PREFIX/share/applications" 2>/dev/null || true

# Keep a copy of the installer for uninstalling later.
install -d "$SHARE"
install -m755 "$HERE/install.sh" "$SHARE/install.sh"
install -m644 "$HERE/VERSION" "$SHARE/VERSION"
info "uninstall later with: sudo $SHARE/install.sh --uninstall"

# ---------------------------------------------------------- shell extension

step "GNOME Shell extension (RP-15 OSD)"

EXT=$PREFIX/share/gnome-shell/extensions/$UUID
rm -rf "$EXT"
install -Dm644 -t "$EXT" "$HERE"/shell-extension/metadata.json \
    "$HERE"/shell-extension/extension.js "$HERE"/shell-extension/stylesheet.css
info "installed for all users in $EXT"

if [ -n "$TARGET_USER" ]; then
    # A per-user copy would shadow the system-wide one with an old version.
    rm -rf "$TARGET_HOME/.local/share/gnome-shell/extensions/$UUID"
    if set_extension_enabled 1; then
        info "enabled for $TARGET_USER"
        RELOGIN=1
    else
        warn "could not enable the extension for $TARGET_USER; enable 'RP-15 OSD' in the Extensions app"
    fi
else
    info "enable it per user with: gnome-extensions enable $UUID"
fi

# --------------------------------------------------------------- services

step "Services"

if systemd_running; then
    systemctl daemon-reload
    systemctl enable rp15d.service >/dev/null 2>&1
    systemctl restart rp15d.service
    sleep 1
    if systemctl is-active --quiet rp15d.service; then
        info "rp15d running"
    else
        warn "rp15d did not start: see 'journalctl -u rp15d'"
    fi
    if [ "$DRIVER_LOADED" = 1 ]; then
        # Let power-profiles-daemon pick up the driver's performance modes.
        systemctl try-restart power-profiles-daemon.service 2>/dev/null || true
    fi
else
    systemctl enable rp15d.service >/dev/null 2>&1 || true
    info "systemd is not running here; rp15d starts at the next boot"
fi

if [ -n "$TARGET_USER" ] && [ -S "/run/user/$TARGET_UID/bus" ]; then
    # Restart a running background instance so it uses the new program.
    pkill -x rp15-control 2>/dev/null || true
    RELOGIN=1
fi

# ----------------------------------------------------------------- summary

echo
echo "${G}${B}RP-15 Linux $VERSION is installed.${N}"
if [ ${#REBOOT_REASONS[@]} -gt 0 ]; then
    echo
    echo "${B}Reboot now${N} to $(IFS=';'; echo "${REBOOT_REASONS[*]}" | sed 's/;/ and /g')."
    if [ "$SB" = on ] && printf '%s\n' "${REBOOT_REASONS[@]}" | grep -q 'Secure Boot'; then
        echo "  On the blue 'Perform MOK management' screen: press a key, then"
        echo "  Enroll MOK > Continue > Yes > enter the password > Reboot."
        echo "  If it times out, run this installer again and reboot again."
    fi
elif [ "$RELOGIN" = 1 ]; then
    echo
    echo "${B}Log out and back in${N} to start the on-screen displays and the background helper."
fi
echo
echo "Open ${B}RP-15 Control${N} from the app grid."
if [ ${#WARNINGS[@]} -gt 0 ]; then
    echo
    echo "${Y}Warnings:${N}"
    printf '  - %s\n' "${WARNINGS[@]}"
fi
