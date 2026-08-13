#!/bin/sh
# Replace Debian's evdi-dkms (1.14.8+dfsg, predates the "Fix for black
# screens on Intel Xe" upstream fix) with a from-source build at v1.14.16.
set -e

SRC_DIR="/home/vini/Projects/Quill/experiments/evdi-bringup/vendor/evdi"
NEW_VER="1.14.16"

echo "== installing libdrm-dev =="
apt-get install -y libdrm-dev

echo "== unloading currently-loaded evdi module =="
modprobe -r evdi || echo "  (not loaded or busy, continuing)"

echo "== removing old dkms registration =="
dkms remove evdi/1.14.8+dfsg --all || echo "  (not registered, continuing)"

echo "== staging new dkms source at /usr/src/evdi-$NEW_VER =="
rm -rf "/usr/src/evdi-$NEW_VER"
mkdir -p "/usr/src/evdi-$NEW_VER"
cp -r "$SRC_DIR/module/." "/usr/src/evdi-$NEW_VER/"

echo "== dkms add/build/install =="
dkms add -m evdi -v "$NEW_VER"
dkms build evdi/"$NEW_VER"
dkms install evdi/"$NEW_VER"

echo "== loading new module =="
modprobe evdi initial_device_count=1

echo "== building + installing matching userspace library =="
cd "$SRC_DIR/library"
make clean || true
make
make install

ldconfig

echo "== done =="
dkms status
modinfo evdi | grep -E "filename|version"
