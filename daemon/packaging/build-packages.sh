#!/usr/bin/env bash
# Builds quill's .deb and .rpm packages, each inside a container.
#
#     ./packaging/build-packages.sh            # both formats
#     ./packaging/build-packages.sh deb        # one of them
#     ./packaging/build-packages.sh --clean    # throw away the cached build tree first
#
# Finished packages land in daemon/target/packages/.
#
# Why containers rather than just building here: cargo-deb resolves
# `depends = "$auto"` with dpkg-shlibdeps, which reads *this* machine's package
# names. Built on Debian trixie the daemon comes out depending on
# `libpipewire-0.3-0t64`, which does not exist on bookworm or Ubuntu 24.04 --
# an uninstallable package, for no reason other than where it was built.
# Building on the oldest release we support gets the names right, and glibc
# forward-compatibility does the rest.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
daemon_dir=$(pwd)
repo_root=$(cd .. && pwd)

# Overridable so a newer baseline can be tried without editing this file.
DEB_IMAGE=${QUILL_DEB_IMAGE:-debian:12}
RPM_IMAGE=${QUILL_RPM_IMAGE:-fedora:latest}

formats=()
clean=0
for arg in "$@"; do
    case "$arg" in
        deb|rpm) formats+=("$arg") ;;
        --clean) clean=1 ;;
        *) echo "usage: build-packages.sh [deb|rpm] [--clean]" >&2; exit 1 ;;
    esac
done
[ ${#formats[@]} -eq 0 ] && formats=(deb rpm)

command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }

# The staging tree lives under target/ rather than /tmp: it holds a full release
# build plus a rustup toolchain per image, which is more than a tmpfs /tmp
# wants, and keeping it means the second run does not recompile the world.
# target/ is already gitignored.
stage="$daemon_dir/target/package-build"
[ "$clean" = 1 ] && rm -rf "$stage"

# Sources only. `cp -r` of the whole daemon directory drags in target/, which is
# multiple gigabytes -- that filled the container the first time this project
# tried a clean-room build (MILESTONES.md, Milestone 25).
mkdir -p "$stage/daemon"
cp "$repo_root/LICENSE" "$stage/"
cp Cargo.toml Cargo.lock build.rs wrapper.h README.md "$stage/daemon/"
rm -rf "$stage/daemon/src" "$stage/daemon/packaging"
cp -r src packaging "$stage/daemon/"
# A stale generated/ from a previous run would be shipped verbatim if the
# rewrite step ever stopped running. It is rebuilt in the container every time.
rm -rf "$stage/daemon/packaging/generated"

out="$daemon_dir/target/packages"
mkdir -p "$out"

for format in "${formats[@]}"; do
    case "$format" in
        deb) image=$DEB_IMAGE; built=target/debian ;;
        rpm) image=$RPM_IMAGE; built=target/generate-rpm ;;
    esac

    echo "==> building .$format in $image"
    docker run --rm \
        -v "$stage:/src" \
        -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
        "$image" \
        bash /src/daemon/packaging/build-in-container.sh "$format"

    # Separate target dirs per format would be tidier, but cargo's is shared and
    # the two formats write to different subdirectories anyway.
    cp "$stage/daemon/$built"/*."$format" "$out/"
done

echo
echo "packages in $out:"
ls -l "$out"
