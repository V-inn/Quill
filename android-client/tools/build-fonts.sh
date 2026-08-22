#!/usr/bin/env bash
# Regenerates app/src/main/res/font/ from the upstream OFL sources.
#
# The four .ttf files in res/font/ are committed, so nothing here runs as part
# of a normal build -- this exists so those binaries are reproducible rather
# than mystery blobs, and so the next person can widen the character set
# without reverse-engineering what was done.
#
# Why they are cut down at all: upstream, the four faces total 1,196 KB.
# Instanced and subset they total 115 KB. Archivo in particular ships only as a
# two-axis variable font (658 KB) and this design uses exactly one instance of
# it -- Expanded, SemiBold -- so carrying the whole design space was 30x the
# bytes of the thing actually used.
#
# The character set is Latin-1 plus the punctuation and symbols the settings
# strings use. Every string in this app is an English literal in Kotlin, so
# there is no user-supplied text that could fall outside it. If you add a
# string with a character outside this set, widen UNICODES and re-run, or it
# renders as tofu.
#
# Runs in a container so it needs nothing installed but Docker.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
OUT="app/src/main/res/font"

UNICODES="U+0020-007E,U+00A0-00FF,U+2018,U+2019,U+201C,U+201D,U+2013,U+2014,U+2022,U+00B7,U+00B0,U+00D7,U+2192,U+21BB"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "fetching upstream sources..."
curl -sL -o "$WORK/Archivo.ttf" \
  "https://raw.githubusercontent.com/google/fonts/main/ofl/archivo/Archivo%5Bwdth%2Cwght%5D.ttf"
curl -sL -o "$WORK/PlexSans-Regular.ttf" \
  "https://github.com/IBM/plex/raw/master/packages/plex-sans/fonts/complete/ttf/IBMPlexSans-Regular.ttf"
curl -sL -o "$WORK/PlexSans-SemiBold.ttf" \
  "https://github.com/IBM/plex/raw/master/packages/plex-sans/fonts/complete/ttf/IBMPlexSans-SemiBold.ttf"
curl -sL -o "$WORK/PlexMono-Regular.ttf" \
  "https://raw.githubusercontent.com/google/fonts/main/ofl/ibmplexmono/IBMPlexMono-Regular.ttf"

cat > "$WORK/run.sh" <<EOF
set -e
pip install --quiet fonttools brotli
cd /w
# Archivo: pin the variable font to Expanded (wdth 125) / SemiBold (wght 600)
# before subsetting, so the unused design space goes too.
fonttools varLib.instancer Archivo.ttf wdth=125 wght=600 -o archivo_static.ttf >/dev/null
pyftsubset archivo_static.ttf     --unicodes="$UNICODES" --layout-features='' --drop-tables+=DSIG --output-file=archivo_expanded_semibold.ttf
pyftsubset PlexSans-Regular.ttf   --unicodes="$UNICODES" --layout-features='' --drop-tables+=DSIG --output-file=ibm_plex_sans_regular.ttf
pyftsubset PlexSans-SemiBold.ttf  --unicodes="$UNICODES" --layout-features='' --drop-tables+=DSIG --output-file=ibm_plex_sans_semibold.ttf
pyftsubset PlexMono-Regular.ttf   --unicodes="$UNICODES" --layout-features='' --drop-tables+=DSIG --output-file=ibm_plex_mono_regular.ttf
EOF

docker run --rm -v "$WORK:/w" -v "$WORK/run.sh:/run.sh:ro" python:3-slim bash /run.sh

mkdir -p "$OUT"
for f in archivo_expanded_semibold ibm_plex_sans_regular ibm_plex_sans_semibold ibm_plex_mono_regular; do
    cp "$WORK/$f.ttf" "$OUT/$f.ttf"
done

echo
ls -l "$OUT" | awk '{s+=$5; if ($9) printf "%8d  %s\n", $5, $9} END {printf "%8d  TOTAL\n", s}'
echo
echo "Licences live in app/src/main/assets/licenses/ and are packaged into the"
echo "APK: OFL 1.1 requires the text to travel with the fonts. Do not move them"
echo "somewhere that is not packaged."
