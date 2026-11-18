#!/usr/bin/env sh
# Regenerates docs/images/*.png from demo data.
# Needs a Chromium: set CHROME, or have chromium / google-chrome / chrome-headless-shell on PATH.
# Fonts: JetBrains Mono (falls back to DejaVu Sans Mono).
set -eu

cd "$(dirname "$0")/.."
out=target/screenshots
rm -rf "$out"
cargo run --quiet --release --example screenshots -- "$out"

chrome="${CHROME:-}"
if [ -z "$chrome" ]; then
  for c in chromium chromium-browser google-chrome chrome-headless-shell; do
    if command -v "$c" >/dev/null 2>&1; then chrome="$c"; break; fi
  done
fi
if [ -z "$chrome" ]; then
  echo "no Chromium found; set CHROME=/path/to/chrome" >&2
  exit 1
fi

mkdir -p docs/images
for page in "$out"/*.html; do
  name=$(basename "$page" .html)
  size=$(sed -n 's/.*width: \([0-9]*\)px; height: \([0-9]*\)px.*/\1,\2/p' "$page" | head -n 1)
  "$chrome" --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=2 \
    --window-size="$size" --screenshot="docs/images/$name.png" "file://$PWD/$page" >/dev/null 2>&1
  echo "docs/images/$name.png"
done
