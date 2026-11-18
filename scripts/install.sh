#!/usr/bin/env bash
# Install Battmon for the current user. No root, no Flatpak.
set -euo pipefail

cd "$(dirname "$0")/.."
root=$(pwd)

bin_dir="${HOME}/.local/bin"
app_dir="${HOME}/.local/share/applications"
icon_dir="${HOME}/.local/share/icons/hicolor/scalable/apps"
unit_dir="${HOME}/.config/systemd/user"
data_dir="${XDG_DATA_HOME:-${HOME}/.local/share}/battmon"

echo "Building release binaries…"
cargo build --release

mkdir -p "$bin_dir" "$app_dir" "$icon_dir" "$unit_dir" "$data_dir"

install -m755 target/release/battmon "$bin_dir/battmon"
install -m755 target/release/battmond "$bin_dir/battmond"
install -m644 "$root/data/design.beben.Battmon.desktop" "$app_dir/design.beben.Battmon.desktop"
install -m644 "$root/data/design.beben.Battmon.svg" "$icon_dir/design.beben.Battmon.svg"
install -m644 "$root/data/battmond.service" "$unit_dir/battmond.service"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$app_dir" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi

systemctl --user daemon-reload
systemctl --user enable battmond.service >/dev/null
systemctl --user restart battmond.service

echo
echo "Installed:"
echo "  $bin_dir/battmon"
echo "  $bin_dir/battmond  (running as battmond.service)"
echo "  $app_dir/design.beben.Battmon.desktop"
echo "  database: $data_dir/battmon.db"
echo
echo "Open the app from the Activities overview as \"Battery\", or run: battmon"
echo "Check the sampler with: systemctl --user status battmond"
