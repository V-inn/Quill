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
# `quill` itself is copied, not symlinked -- it's a tiny fixed wrapper
# script, not a build artifact tied to this checkout.
cp packaging/quill ~/.local/bin/quill
chmod +x ~/.local/bin/quill

cp packaging/quill-daemon.service ~/.config/systemd/user/quill-daemon.service
systemctl --user daemon-reload

echo "user unit installed. Now run (needs root):"
echo
echo "  # auto-launch when the tablet is plugged in"
echo "  sudo cp $(pwd)/packaging/99-quill-daemon.rules /etc/udev/rules.d/99-quill-daemon.rules"
echo

# The pen, touch and pointer devices are all uinput devices, and /dev/uinput is
# root-owned unless a rule says otherwise. Only mentioned when it's actually
# needed: a machine that already has an equivalent rule from some other package
# (Steam ships one) needs nothing here, and telling people to install a udev
# rule they don't need is how a project earns a reputation for wanting root.
if [ -r /dev/uinput ] && [ -w /dev/uinput ]; then
    echo "  # (/dev/uinput is already accessible to you -- nothing to do for input)"
else
    echo "  # S Pen / touch input: grant this user access to /dev/uinput"
    echo "  sudo cp $(pwd)/packaging/70-quill-uinput.rules /etc/udev/rules.d/70-quill-uinput.rules"
fi
echo
echo "  sudo udevadm control --reload"
echo "  sudo udevadm trigger --subsystem-match=misc"
echo
echo "Then log out and back in, so logind applies the /dev/uinput ACL to your session."
