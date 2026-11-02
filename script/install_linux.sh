#!/usr/bin/env bash
# Build the Linux openusage binary and install it to ~/.local/bin.
# Swift 6.2 on PATH is used when it is there; otherwise mise's swift@6.2.
# --disable-automatic-resolution keeps Package.resolved in place. A plain
# `swift build` on Linux deletes it and drops the Mac app's package pins.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

args=(build -c release --static-swift-stdlib --disable-automatic-resolution)

if command -v swift >/dev/null 2>&1 && swift --version 2>/dev/null | grep -q 'Swift version 6\.2'; then
  swift "${args[@]}"
elif command -v mise >/dev/null 2>&1; then
  mise exec swift@6.2 -- swift "${args[@]}"
else
  echo "error: Swift 6.2 is not on PATH and mise is not available. Install Swift 6.2, or install mise, then retry." >&2
  exit 1
fi

dest="${HOME}/.local/bin"
mkdir -p "$dest"
install -m 755 .build/release/openusage "${dest}/openusage"

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

cat <<EOF
Installed ${dest}/openusage

Next:
  1. Put ${dest} on PATH if it is not already.
  2. Unlock the keyring (GNOME Keyring, or another Secret Service provider).
  3. Add an account, for example: openusage add claude
  4. Merge contrib/waybar/config.jsonc into Waybar. See docs/linux.md.
EOF
