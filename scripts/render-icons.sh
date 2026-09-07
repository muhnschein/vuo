#!/usr/bin/env bash
# Render the four Harbour icon sizes from icons/harbour-vuo.svg.
#
# The SVG is the source; the PNGs are build output that happens to be
# committed, because the RPM installs them and nothing in the packaging path
# has an SVG renderer. Regenerate rather than editing a PNG, or the two drift
# and the one everybody looks at is the one the phone does not show.
#
# Harbour requires EXACTLY these four sizes, and `scripts/check-harbour.sh`
# reads the dimensions back out of each file's IHDR.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SVG=icons/harbour-vuo.svg
[ -f "$SVG" ] || { echo "no $SVG" >&2; exit 1; }
command -v rsvg-convert >/dev/null || {
    echo "rsvg-convert is missing (apt: librsvg2-bin, brew: librsvg)" >&2
    exit 1
}

for size in 86 108 128 172; do
    out="icons/${size}x${size}/harbour-vuo.png"
    mkdir -p "$(dirname "$out")"
    rsvg-convert -w "$size" -h "$size" -f png -o "$out" "$SVG"
    echo "  $out"
done
echo "  four icon sizes rendered from $SVG"
