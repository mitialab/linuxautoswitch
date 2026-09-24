#!/usr/bin/env bash
# Builds linuxautoswitch and installs it as a systemd --user service.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

BIN_DIR="$HOME/.local/bin"
UNIT_DIR="$HOME/.config/systemd/user"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/linuxautoswitch"

echo "==> Checking dependencies"
missing=()
command -v cargo >/dev/null 2>&1 || missing+=(rust)
command -v wtype >/dev/null 2>&1 || missing+=(wtype)
if [ ${#missing[@]} -gt 0 ]; then
  echo "Missing dependencies: ${missing[*]}"
  echo "On Omarchy/Arch, install with: sudo pacman -S ${missing[*]}"
  exit 1
fi

if ! id -nG "$USER" | tr ' ' '\n' | grep -qx input; then
  echo "==> Adding $USER to the 'input' group (needed to read /dev/input/eventN)"
  sudo usermod -aG input "$USER"
  echo "    NOTE: you must log out and back in for this to take effect,"
  echo "    then re-run this script (or just start the service after logging back in)."
fi

echo "==> Building release binary"
cargo build --release

echo "==> Installing"
install -Dm755 target/release/linuxautoswitch "$BIN_DIR/linuxautoswitch"
install -Dm644 packaging/linuxautoswitch.service "$UNIT_DIR/linuxautoswitch.service"

if [ ! -f "$CONFIG_DIR/config.toml" ]; then
  install -Dm644 assets/config.example.toml "$CONFIG_DIR/config.toml"
  echo "==> Wrote default config to $CONFIG_DIR/config.toml"
fi

systemctl --user daemon-reload
systemctl --user enable --now linuxautoswitch.service

echo "==> Done."
echo "    Status: systemctl --user status linuxautoswitch"
echo "    Logs:   journalctl --user -u linuxautoswitch -f"
