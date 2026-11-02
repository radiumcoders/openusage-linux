#!/usr/bin/env bash
# Build the Rust Linux edition and install openusage and openusage-panel to ~/.local/bin.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not on PATH. Install Rust, then re-run. See docs/linux.md." >&2
  exit 1
fi

cargo build --release --manifest-path "$root/linux/Cargo.toml"

dest="${HOME}/.local/bin"
mkdir -p "$dest"
install -m 755 "$root/linux/target/release/openusage" "${dest}/openusage"
install -m 755 "$root/linux/target/release/openusage-panel" "${dest}/openusage-panel"

warn_missing() {
  local cmd=$1
  local message=$2
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "warning: ${cmd} is not installed. ${message}" >&2
  fi
}

warn_missing secret-tool "Sign-ins are stored in the desktop keyring through libsecret. Install libsecret and a Secret Service keyring such as gnome-keyring."
warn_missing notify-send "Quota alerts need libnotify's notify-send, and a notification daemon such as mako, dunst, or swaync."
warn_missing xdg-open "Browser sign-in opens its link with xdg-open."

if ! pkg-config --exists gtk4-layer-shell-0 2>/dev/null && ! ldconfig -p 2>/dev/null | grep -q 'libgtk4-layer-shell'; then
  echo "warning: gtk4-layer-shell is not installed. The panel needs GTK 4 and gtk4-layer-shell." >&2
fi

cat <<EOF
Installed ${dest}/openusage and ${dest}/openusage-panel

Next:
  1. Put ${dest} on PATH if it is not already.
  2. Unlock the keyring (GNOME Keyring, or another Secret Service provider).
  3. Add an account, for example: openusage add claude
  4. Merge contrib/waybar/config.jsonc into Waybar. Left click opens the panel. See docs/linux.md.
EOF
