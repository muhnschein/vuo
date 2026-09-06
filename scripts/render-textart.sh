#!/usr/bin/env bash
# Regenerate the texture masks in qml/art/ from tools/textart/.
#
# The app ships the pattern as an image rather than painting it (see
# qml/components/TextArt.qml for why), so this is the step that turns the
# painter into what ships. Run it when the painter, the strokes or the
# densities change, and COMMIT the result -- like the compiled translations,
# the masks are generated but tracked, so a build needs neither a GPU nor a
# display.
#
# It needs a QML runtime with a GL context. On a headless machine `xvfb-run`
# supplies one; the script finds it if it is there and says what to install
# if it is not.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QMLSCENE=${QMLSCENE:-$(command -v qmlscene || true)}
if [ -z "$QMLSCENE" ] || [ ! -x "$QMLSCENE" ]; then
    echo "qmlscene not found. Install qtdeclarative5-dev-tools (Debian) or set QMLSCENE." >&2
    exit 1
fi

# The painter reads Theme for the font family, so it needs the Silica stubs
# the QML tests use.
#
# THE FONT IS NOT THE DEVICE'S. Sail Sans Pro ships with SailfishOS and is not
# redistributable, so a machine that is not a phone renders these with
# whatever its fontconfig calls that name -- in practice the default sans. At
# the sizes here the letters are texture rather than reading matter, and the
# pattern is the same either way, but it is the one respect in which the
# shipped art is not what the device would have drawn for itself.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp tools/textart/*.qml "$work/"

echo "== painting the masters =="
if command -v xvfb-run >/dev/null 2>&1 && [ -z "${DISPLAY:-}" ]; then
    ( cd "$work" && xvfb-run -a -s "-screen 0 1400x1000x24" \
        env QT_QPA_PLATFORM=xcb "$QMLSCENE" -I "$ROOT/qml-stubs" render.qml )
elif [ -n "${DISPLAY:-}" ]; then
    ( cd "$work" && QT_QPA_PLATFORM=xcb "$QMLSCENE" -I "$ROOT/qml-stubs" render.qml )
else
    echo "no DISPLAY and no xvfb-run: install xvfb, or run this on a desktop." >&2
    exit 1
fi | tee "$work/painted.log"

# A ring traced twice draws text on top of text, which is what a device
# reported once the art was otherwise right. The painter counts it; refuse
# the master rather than shipping it.
if grep -q "^qml: OVERLAPS" "$work/painted.log"; then
    if grep "^qml: OVERLAPS" "$work/painted.log" | grep -qv " 0$"; then
        echo "FAIL: the painter drew over itself:" >&2
        grep "^qml: OVERLAPS" "$work/painted.log" >&2
        exit 1
    fi
    echo "  no master draws over itself"
else
    echo "FAIL: the painter reported no overlap count; did render.qml change?" >&2
    exit 1
fi

echo "== reducing them to masks =="
mkdir -p qml/art
shopt -s nullglob
painted=("$work"/*.png)
if [ ${#painted[@]} -eq 0 ]; then
    echo "the painter wrote nothing; see the output above." >&2
    exit 1
fi
for png in "${painted[@]}"; do
    scripts/png-mask.py "$png" "qml/art/$(basename "$png")"
done
echo "  masks are in qml/art/; commit them."
