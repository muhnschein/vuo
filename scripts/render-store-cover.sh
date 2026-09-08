#!/usr/bin/env bash
# Render store/cover.png -- the 1080x540 banner on Vuo's Harbour Store page.
#
# Not part of the app and not installed. It is the onboarding screen laid out
# for a landscape frame, so the Store page and the app look like one thing;
# tools/textart/store-cover.qml is the composition and this puts the pieces
# where it expects them.
#
# Needs the same QML runtime with a GL context that `make textart` does, and
# two font files. Sail Sans Pro ships with SailfishOS and is not
# redistributable, so the wordmark is Fira Sans (SIL OFL). Point
# VUO_FONT_DIR at a directory holding FiraSans-Light.ttf and
# FiraSans-Regular.ttf, or let this fetch them from Google Fonts once.
#
# The result is COMMITTED, like the masks in qml/art/: the Store wants a file.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QMLSCENE=${QMLSCENE:-$(command -v qmlscene || true)}
if [[ -z "$QMLSCENE" ]] || [[ ! -x "$QMLSCENE" ]]; then
    echo "qmlscene not found. Install qtdeclarative5-dev-tools (Debian) or set QMLSCENE." >&2
    exit 1
fi

FONT_DIR=${VUO_FONT_DIR:-"$ROOT/.fonts"}
LIGHT="$FONT_DIR/FiraSans-Light.ttf"
BOOK="$FONT_DIR/FiraSans-Regular.ttf"
if [[ ! -f "$LIGHT" ]] || [[ ! -f "$BOOK" ]]; then
    echo "== fetching Fira Sans (SIL OFL) into $FONT_DIR =="
    mkdir -p "$FONT_DIR"
    # Resolved through the CSS API rather than hardcoding a versioned path,
    # which Google rotates.
    css=$(curl -sSfL "https://fonts.googleapis.com/css2?family=Fira+Sans:wght@300;400&display=swap") || {
        echo "could not reach Google Fonts; set VUO_FONT_DIR to a directory holding the two files." >&2
        exit 1; }
    mapfile -t urls < <(grep -oE "https://fonts.gstatic.com/[^)]*" <<< "$css")
    [[ "${#urls[@]}" -ge 2 ]] || { echo "the font CSS named ${#urls[@]} files, expected 2" >&2; exit 1; }
    curl -sSfL "${urls[0]}" -o "$LIGHT"
    curl -sSfL "${urls[1]}" -o "$BOOK"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp tools/textart/TextArtPainter.qml tools/textart/store-cover.qml "$work/"
cp "$LIGHT" "$work/FiraSans-Light.ttf"
cp "$BOOK" "$work/FiraSans-Regular.ttf"

echo "== painting the cover =="
if command -v xvfb-run >/dev/null 2>&1 && [[ -z "${DISPLAY:-}" ]]; then
    ( cd "$work" && xvfb-run -a -s "-screen 0 1400x1000x24" \
        env QT_QPA_PLATFORM=xcb "$QMLSCENE" -I "$ROOT/qml-stubs" store-cover.qml )
elif [[ -n "${DISPLAY:-}" ]]; then
    ( cd "$work" && QT_QPA_PLATFORM=xcb "$QMLSCENE" -I "$ROOT/qml-stubs" store-cover.qml )
else
    echo "no DISPLAY and no xvfb-run: install xvfb, or run this on a desktop." >&2
    exit 1
fi

[[ -f "$work/cover.png" ]] || { echo "the painter wrote nothing; see the output above." >&2; exit 1; }
mkdir -p store
cp "$work/cover.png" store/cover.png
echo "  wrote store/cover.png ($(du -h store/cover.png | cut -f1)); commit it."
