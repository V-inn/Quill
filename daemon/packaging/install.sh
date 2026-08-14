#!/usr/bin/env bash
# Installs the auto-launch pieces (systemd --user unit + udev rule) so the
# daemon starts automatically when a Samsung Android device is plugged in.
# Not run as part of `cargo build` -- opt-in, since it touches
# /etc/udev/rules.d (system-wide, needs root) in addition to the user's own
# systemd/bin directories.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [ ! -f target/release/quill-daemon ]; then
    echo "release binary not found -- run 'cargo build --release' first" >&2
    exit 1
fi

mkdir -p ~/.local/bin ~/.local/share/quill ~/.config/systemd/user
# Symlinked, not copied: a stable path for the unit file to reference no
# matter where this repo is checked out, that stays current across rebuilds
# without needing to reinstall.
ln -sf "$(pwd)/target/release/quill-daemon" ~/.local/bin/quill-daemon

cp packaging/quill-daemon.service ~/.config/systemd/user/quill-daemon.service
systemctl --user daemon-reload

echo "user unit installed. Now run (needs root):"
echo "  sudo cp $(pwd)/packaging/99-quill-daemon.rules /etc/udev/rules.d/99-quill-daemon.rules"
echo "  sudo udevadm control --reload"
