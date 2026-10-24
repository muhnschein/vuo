#!/usr/bin/env bash
# Render four Harbour icon sizes from icons/harbour-vuo.svg.
#
# SVG = source; PNGs = committed build output (RPM installs them, packaging path
# has no SVG renderer). Regenerate, never edit PNG, or they drift and phone
# shows one nobody looks at.
#
# Harbour requires EXACTLY these four sizes; `scripts/check-harbour.sh` reads
# dimensions back from each IHDR.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SVG=icons/harbour-vuo.svg
[[ -f "$SVG" ]] || { echo "no $SVG" >&2; exit 1; }
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
