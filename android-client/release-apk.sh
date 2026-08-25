#!/usr/bin/env bash
#
# Builds the release APK and attaches it to a GitHub release.
#
# Why this is a script on the developer's machine and not a workflow: the
# signing key cannot go into CI. Losing it strands every installed copy and
# leaking it lets someone else ship something that installs *as* Quill, so it
# lives in ~/quill-release.jks and its password lives in
# ~/.gradle/gradle.properties, on this machine only. That is also why
# .github/workflows only builds the Linux packages.
#
# Nothing here ever reads, prints or passes the password: Gradle picks it up
# from ~/.gradle/gradle.properties by itself (see app/build.gradle.kts). The
# check that the right key was used is done afterwards, from the certificate in
# the built APK, which needs no password at all.
#
# Usage:
#     ./release-apk.sh v0.1              # build, verify, and upload
#     ./release-apk.sh v0.1 --dry-run    # build and verify only
#
set -euo pipefail

# The one identity that may ever ship as Quill. An APK signed by anything else
# will not install over an existing copy, so publishing one is worse than
# publishing nothing -- it is an update nobody can take.
EXPECTED_FINGERPRINT=f9264e72ddf3d8df5a641723f68d9e44fe2086888248df5d79079e77879f6f7d

cd "$(dirname "$0")"

tag=${1:-}
dry_run=${2:-}
if [[ -z $tag ]]; then
    echo "usage: $0 <tag> [--dry-run]" >&2
    exit 2
fi

# The tag is the release's public name and versionName is the name the app
# gives itself when someone opens Settings; they disagreeing is a bug report
# nobody can act on. build.gradle.kts is the source of truth, so read it rather
# than asking for the version twice.
version_name=$(sed -n 's/^ *versionName = "\(.*\)"/\1/p' app/build.gradle.kts)
version_code=$(sed -n 's/^ *versionCode = \([0-9]*\)/\1/p' app/build.gradle.kts)
if [[ "$tag" != "v$version_name" ]]; then
    echo "tag $tag does not match versionName $version_name (expected v$version_name)" >&2
    echo "bump versionName and versionCode in app/build.gradle.kts first." >&2
    exit 1
fi
echo "building Quill $version_name (versionCode $version_code) for $tag"

./gradlew --quiet clean assembleRelease

apk=app/build/outputs/apk/release/app-release.apk
if [[ ! -f $apk ]]; then
    # The build succeeds without the keystore properties and produces
    # app-release-unsigned.apk instead -- deliberate, so the project still
    # builds for anyone who clones it, but not something to publish.
    echo "no signed APK at $apk." >&2
    echo "the keystore properties are missing; this machine can only build an unsigned release." >&2
    exit 1
fi

# apksigner is not on PATH; it ships per build-tools version.
apksigner=$(ls -d "$HOME"/Android/Sdk/build-tools/*/apksigner 2>/dev/null | tail -1)
if [[ -z ${apksigner:-} ]]; then
    echo "apksigner not found under ~/Android/Sdk/build-tools" >&2
    exit 1
fi

fingerprint=$("$apksigner" verify --print-certs "$apk" |
    sed -n 's/^Signer #1 certificate SHA-256 digest: //p')
if [[ "$fingerprint" != "$EXPECTED_FINGERPRINT" ]]; then
    echo "REFUSING TO PUBLISH: $apk is signed by $fingerprint" >&2
    echo "expected $EXPECTED_FINGERPRINT -- this APK would not install as an update to Quill." >&2
    exit 1
fi
echo "signed by the expected key ($fingerprint)"

# Named for the release rather than left as app-release.apk, which tells
# someone who downloaded it nothing about what they have.
out="quill-$version_name.apk"
cp "$apk" "$out"
echo "built $out ($(du -h "$out" | cut -f1))"

if [[ $dry_run == "--dry-run" ]]; then
    echo "dry run: not uploading."
    exit 0
fi

# Deliberately not the .aab. A bundle is not installable, and once the app is
# enrolled in Play App Signing the bundle is an upload artifact for one store
# and useful to nobody else. `bundleRelease` builds it when Play needs it.
echo "attaching $out to release $tag"
gh release upload "$tag" "$out" --clobber
echo "done. The .aab for Play, if one is needed, comes from ./gradlew bundleRelease."
