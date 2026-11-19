#!/bin/sh
set -eu

# Builds Tiro from this checkout and installs it for the current user, under
# its own identity so it never meets an installed Hex: the app id
# dev.axl47.tiro (desktop and autostart entries, icon, window class), the
# `tiro` command, tiro.service, and ~/.local/share/tiro. The executable keeps
# its internal name, hex, inside the version layout Hex's own installer
# uses. Tiro never updates itself; run this again to update.

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
app_id=dev.axl47.tiro
unit_name=tiro.service
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
support_dir="$data_home/tiro"
install_dir="$HOME/.local/bin"
applications="$data_home/applications"
autostart="$config_home/autostart"
units="$config_home/systemd/user"
binary="$install_dir/tiro"
unit="$units/$unit_name"

fail() {
  echo "$1" >&2
  exit 1
}

# Tiro's own files carry the same marks as Hex's, which the app checks
# before it replaces or removes anything.
managed() {
  grep -Fxq 'X-Hex-Managed=true' "$1" || grep -Fxq '# X-Hex-Managed=true' "$1" \
    || grep -Fxq "X-XDP-Autostart=$app_id" "$1"
}

refuse_unmanaged() {
  if [ -L "$1" ] || { [ -e "$1" ] && ! managed "$1"; }; then
    fail "Refusing to replace a file Tiro didn't write: $1"
  fi
}

uninstall() {
  if [ -f "$unit" ] && managed "$unit"; then
    systemctl --user disable --now "$unit_name" >/dev/null 2>&1 || true
    rm -f "$unit"
    systemctl --user daemon-reload >/dev/null 2>&1 || true
  fi
  if [ -L "$binary" ] && [ "$(readlink "$binary")" = "$support_dir/current/hex" ]; then
    rm -f "$binary"
  fi
  for desktop in "$applications/$app_id.desktop" "$autostart/$app_id.desktop"; do
    if [ -f "$desktop" ] && [ ! -L "$desktop" ] && managed "$desktop"; then
      rm -f "$desktop"
    fi
  done
  # The icons the app writes into the user's theme, marked by a PNG text chunk.
  for icon in "$data_home"/icons/hicolor/*/apps/"$app_id".png; do
    if [ -f "$icon" ] && [ ! -L "$icon" ] && LC_ALL=C grep -Faq 'X-Hex-Managed=true' "$icon"; then
      rm -f "$icon"
    fi
  done
  rm -f "$support_dir/current"
  rm -rf "$support_dir/versions"
  echo "Removed Tiro. Settings, models, and History remain in $support_dir."
}

[ "$(uname -s)" = Linux ] || fail "This installs Tiro on Linux."
[ "$(id -u)" -ne 0 ] || fail "Install Tiro as your desktop user, not root."
case ${1:-install} in
  install) ;;
  uninstall)
    uninstall
    exit 0
    ;;
  *) fail "Usage: $0 [install|uninstall]" ;;
esac

target_dir=${CARGO_TARGET_DIR:-"$root/target"}
cargo build --locked --release --manifest-path "$root/Cargo.toml"
artifact="$target_dir/release/hex"
version=$("$artifact" --version | awk 'NF { value=$NF } END { print value }')
# Every rebuild gets its own version directory, so a running Tiro keeps its
# executable until it restarts.
version="$version+$(git -C "$root" rev-parse --short=10 HEAD)$(git -C "$root" diff --quiet HEAD || echo .dirty)"
case $support_dir$binary in
  *[\"\\]*) fail "Install paths can't contain quotes or backslashes." ;;
esac

umask 022
mkdir -p "$install_dir" "$applications" "$autostart" "$units"
mkdir -p "$support_dir/versions/$version"
chmod 700 "$support_dir" "$support_dir/versions" "$support_dir/versions/$version"
for path in "$applications/$app_id.desktop" "$autostart/$app_id.desktop" "$unit"; do
  refuse_unmanaged "$path"
done
if { [ -e "$binary" ] || [ -L "$binary" ]; } \
  && [ "$(readlink "$binary" 2>/dev/null)" != "$support_dir/current/hex" ]; then
  fail "Refusing to replace a file Tiro didn't write: $binary"
fi

install -m755 "$artifact" "$support_dir/versions/$version/.hex.next"
mv -f "$support_dir/versions/$version/.hex.next" "$support_dir/versions/$version/hex"
ln -sfn "versions/$version" "$support_dir/.current.next"
mv -Tf "$support_dir/.current.next" "$support_dir/current"
ln -sfn "$support_dir/current/hex" "$install_dir/.tiro.next"
mv -Tf "$install_dir/.tiro.next" "$binary"
# Keep the two newest builds: the running one may be the older.
ls -1t "$support_dir/versions" | tail -n +3 | while read -r old; do
  [ "$old" = "$version" ] || rm -rf "${support_dir:?}/versions/$old"
done

cat > "$applications/.$app_id.desktop.next" <<EOF
[Desktop Entry]
Type=Application
Name=Tiro
Comment=Local voice dictation
Exec="$binary"
Icon=$app_id
Terminal=false
Categories=Utility;Accessibility;
StartupNotify=true
StartupWMClass=$app_id
Actions=quit;
X-Hex-Managed=true

[Desktop Action quit]
Name=Quit Tiro
Exec="$binary" quit
EOF
mv -f "$applications/.$app_id.desktop.next" "$applications/$app_id.desktop"

# Launch at login, as Hex's installer sets it up; Settings turns it off.
if [ ! -e "$autostart/$app_id.desktop" ]; then
  cat > "$autostart/.$app_id.desktop.next" <<EOF
[Desktop Entry]
Type=Application
Name=Tiro
Comment=Local voice dictation
Exec="$binary" start
Icon=$app_id
Terminal=false
X-GNOME-Autostart-enabled=true
X-Hex-Managed=true
EOF
  mv -f "$autostart/.$app_id.desktop.next" "$autostart/$app_id.desktop"
fi

# `tiro start` and the launcher start Tiro through this unit, which owns its
# lifetime and restarts it after a crash.
cat > "$units/.$unit_name.next" <<EOF
# X-Hex-Managed=true
[Unit]
Description=Tiro background dictation
PartOf=graphical-session.target
After=graphical-session.target
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=notify
NotifyAccess=main
ExecStart="$binary" service
Environment="HEX_APPLICATION_SUPPORT_DIR=$support_dir"
EnvironmentFile=-$support_dir/session.env
Restart=on-failure
RestartSec=3
TimeoutStopSec=30
UMask=0077

[Install]
WantedBy=graphical-session.target
EOF
mv -f "$units/.$unit_name.next" "$unit"
systemctl --user daemon-reload >/dev/null 2>&1 || true
update-desktop-database "$applications" >/dev/null 2>&1 || true

echo "Installed Tiro $version to $binary"
if systemctl --user is-active --quiet "$unit_name" 2>/dev/null; then
  echo "Restart the running Tiro onto it: $binary restart"
else
  echo "Install a model: $binary model install"
  echo "Start Tiro: $binary start (quit with $binary quit)"
fi
