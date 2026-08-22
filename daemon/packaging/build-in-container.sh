#!/usr/bin/env bash
# Runs *inside* the build container, with the staged sources bind-mounted at
# /src. Not meant to be run on a developer machine directly -- see
# build-packages.sh, which is what starts it.
#
# Argument: `deb` or `rpm`.
#
# The container is the point of this script, not an implementation detail.
# cargo-deb resolves `depends = "$auto"` by running dpkg-shlibdeps against the
# machine it builds on, so a package built on Debian trixie records
# `libpipewire-0.3-0t64` -- a name that does not exist on bookworm or on Ubuntu
# 24.04, which would make the package uninstallable there for no reason. Build
# on the oldest release we intend to support and the names come out right.
set -euo pipefail

format=${1:?usage: build-in-container.sh deb|rpm}

# /src is where build-packages.sh bind-mounts the staged tree. CI runs this
# same script inside a job container instead, where the checkout is somewhere
# else entirely, so the root is overridable.
src_root=${QUILL_SRC:-/src}
cd "$src_root/daemon"

# Pinned so a release is reproducible and an upstream change cannot alter what
# a tagged build produces without a commit here.
CARGO_DEB_VERSION=3.7.0
CARGO_GENERATE_RPM_VERSION=0.21.0

# --- Build dependencies -----------------------------------------------------
# Deliberately the same package lists daemon/README.md gives its readers. If
# one of these ever has to change, the README is wrong too.
. /etc/os-release
case "$ID" in
    debian|ubuntu)
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y --no-install-recommends \
            build-essential pkg-config curl ca-certificates \
            clang libclang-dev \
            libva-dev libpipewire-0.3-dev libusb-1.0-0-dev \
            dpkg-dev
        ;;
    fedora)
        dnf install -y -q \
            gcc gcc-c++ pkgconf-pkg-config curl \
            clang-devel \
            libva-devel pipewire-devel libusb1-devel
        ;;
    *)
        echo "build-in-container.sh: unsupported base image ($ID)" >&2
        exit 1
        ;;
esac

# --- Rust -------------------------------------------------------------------
# rustup, not the distro's rustc: ashpd's zbus chain needs Rust 1.87+, and
# neither bookworm nor Fedora's default stream is reliably that new. This is
# the same reason MILESTONES.md, Milestone 2 gives for the dev machine.
export RUSTUP_HOME=$src_root/.rustup
export CARGO_HOME=$src_root/.cargo
export PATH="$CARGO_HOME/bin:$PATH"
if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable
fi
rustc --version

# --- The two files that differ between a hand install and a package ---------
# install.sh puts the binary in ~/.local/bin; a package puts it in /usr/bin.
# That single path is the whole difference, so the shipped files are generated
# from the checked-in ones by rewriting it, rather than being kept as a second
# copy that would drift.
#
# Each rewrite is verified. A sed that matches nothing is silent, and a wrapper
# or unit still pointing at ~/.local/bin would produce a package that installs
# cleanly and then fails to start for everyone who is not the developer.
gen=packaging/generated
mkdir -p "$gen"

sed 's|exec ~/.local/bin/quill-daemon|exec /usr/bin/quill-daemon|' \
    packaging/quill > "$gen/quill"
grep -q '^exec /usr/bin/quill-daemon ' "$gen/quill" || {
    echo "packaging: wrapper path rewrite matched nothing -- packaging/quill changed?" >&2
    exit 1
}
chmod 755 "$gen/quill"

sed 's|^ExecStart=%h/.local/bin/quill-daemon|ExecStart=/usr/bin/quill-daemon|' \
    packaging/quill-daemon.service > "$gen/quill-daemon.service"
grep -q '^ExecStart=/usr/bin/quill-daemon ' "$gen/quill-daemon.service" || {
    echo "packaging: unit ExecStart rewrite matched nothing -- the unit changed?" >&2
    exit 1
}
# %h is kept everywhere else on purpose: the daemon's data and config still
# live in the user's home, and ReadWritePaths must keep pointing there.
grep -q '^ReadWritePaths=%h/' "$gen/quill-daemon.service" || {
    echo "packaging: unit lost its %h ReadWritePaths -- refusing to ship it" >&2
    exit 1
}

# --- Build ------------------------------------------------------------------
# Just the daemon: src/bin/uinput_test.rs and vui_bitstream_test.rs are
# throwaway diagnostics (see MILESTONES.md) and have no business in a package.
cargo build --release --bin quill-daemon

case "$format" in
    deb)
        cargo install --locked --quiet "cargo-deb@${CARGO_DEB_VERSION}"
        # --no-build: the release binary is already there, and letting cargo-deb
        # drive the build would pull in the two diagnostic binaries.
        cargo deb --no-build
        cargo deb --no-build --variant uinput
        out=target/debian

        # cargo-deb has no field for a per-variant synopsis: the one-line
        # Description comes from `package.description` for every package it
        # builds. For quill-uinput that line would describe the daemon, so it
        # is rewritten here -- unpack, replace the first Description line,
        # repack -- and verified, because a silent no-op would ship the wrong
        # text.
        uinput_deb=$(ls "$out"/quill-uinput_*.deb)
        work=$(mktemp -d)
        dpkg-deb -R "$uinput_deb" "$work"
        sed -i '0,/^Description: /s|^Description: .*|Description: udev rule letting Quill'"'"'s pen report pressure and tilt|' \
            "$work/DEBIAN/control"
        grep -q '^Description: udev rule ' "$work/DEBIAN/control" || {
            echo "packaging: quill-uinput synopsis rewrite matched nothing" >&2
            exit 1
        }
        rm -f "$uinput_deb"
        dpkg-deb --build --root-owner-group "$work" "$uinput_deb"
        rm -rf "$work"
        ;;
    rpm)
        cargo install --locked --quiet "cargo-generate-rpm@${CARGO_GENERATE_RPM_VERSION}"
        # builtin auto-req reads the binary with ldd instead of shelling out to
        # rpmbuild's find-requires, so no rpm tooling is needed in the image.
        cargo generate-rpm --auto-req builtin
        # The uinput package is one text file; there is nothing to scan, and
        # leaving auto-req on would only add a dependency on /bin/sh.
        cargo generate-rpm --variant uinput --auto-req disabled
        out=target/generate-rpm
        ;;
    *)
        echo "build-in-container.sh: unknown format '$format'" >&2
        exit 1
        ;;
esac

ls -l "$out"

# Everything under /src is bind-mounted from the host, and the container runs
# as root; without this the developer cannot delete their own build directory.
if [ -n "${HOST_UID:-}" ] && [ -n "${HOST_GID:-}" ]; then
    chown -R "$HOST_UID:$HOST_GID" "$src_root"
fi
