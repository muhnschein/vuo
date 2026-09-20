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
#     scripts/render-textart.sh                 # everything
#     scripts/render-textart.sh cover           # the cover's set only
#     scripts/render-textart.sh cover:42,99+    # two of the cover's masters
#     scripts/render-textart.sh onboarding      # the page's one master
#
# The cover's set is one mask per unread count, 0 to 99 and "99+", in
# qml/art/cover/: the count is negative space in the pattern, so the whole
# background depends on the number. A partial run (cover:42) overwrites those
# masks and leaves the rest; a full cover run replaces the directory.
#
# It needs a QML runtime with a GL context. On a headless machine `xvfb-run`
# supplies one; the script finds it if it is there and says what to install
# if it is not. The cover's digits are set in Fira Sans ExtraBold (SIL OFL),
# fetched once into the gitignored font directory as render-store-cover.sh
# fetches its two; VUO_FONT_DIR points it elsewhere.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QMLSCENE=${QMLSCENE:-$(command -v qmlscene || true)}
if [[ -z "$QMLSCENE" ]] || [[ ! -x "$QMLSCENE" ]]; then
    echo "qmlscene not found. Install qtdeclarative5-dev-tools (Debian) or set QMLSCENE." >&2
    exit 1
fi

# Which sets to paint, and for the cover which counts.
sets=()
keys=()
if [[ $# -eq 0 ]]; then
    sets=(onboarding cover)
fi
for arg in "$@"; do
    case "$arg" in
        onboarding) sets+=(onboarding) ;;
        cover) sets+=(cover) ;;
        cover:*)
            sets+=(cover)
            IFS=',' read -r -a wanted <<< "${arg#cover:}"
            for key in "${wanted[@]}"; do
                [[ "$key" =~ ^([0-9]{1,2}|99\+)$ ]] \
                    || { echo "cover key '$key' is not 0..99 or 99+" >&2; exit 1; }
                keys+=("$key")
            done ;;
        *) echo "unknown set '$arg'; expected onboarding, cover or cover:KEY,..." >&2; exit 1 ;;
    esac
done
full_cover=0
if [[ " ${sets[*]} " == *" cover "* ]] && [[ ${#keys[@]} -eq 0 ]]; then
    full_cover=1
fi

# The digits' face. Sail Sans Pro ships with SailfishOS and is not
# redistributable; the digits are only a silhouette, so the face matters
# less than its weight, and ExtraBold is the heaviest at which the counter
# of a 4 still holds a line of text. Fetched over TLS only, like the store
# cover's fonts: -L follows redirects, and --proto-redir keeps the hop
# nobody chose from walking down to plain http (§9.1).
FONT_DIR=${VUO_FONT_DIR:-"$ROOT/.fonts"}
DIGITS="$FONT_DIR/FiraSans-ExtraBold.ttf"
TLS_ONLY=(--proto '=https' --proto-redir '=https')
if [[ ! -f "$DIGITS" ]]; then
    echo "== fetching Fira Sans ExtraBold (SIL OFL) into $FONT_DIR =="
    mkdir -p "$FONT_DIR"
    css=$(curl -sSfL "${TLS_ONLY[@]}" \
        "https://fonts.googleapis.com/css2?family=Fira+Sans:wght@800&display=swap") || {
        echo "could not reach Google Fonts; set VUO_FONT_DIR to a directory holding FiraSans-ExtraBold.ttf." >&2
        exit 1; }
    url=$(grep -oE "https://fonts.gstatic.com/[^)]*" <<< "$css" | head -1)
    [[ -n "$url" ]] || { echo "the font CSS named no file" >&2; exit 1; }
    curl -sSfL "${TLS_ONLY[@]}" "$url" -o "$DIGITS"
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
cp "$DIGITS" "$work/FiraSans-ExtraBold.ttf"
mkdir -p "$work/cover"
{
    printf 'var sets = ['
    sep=""
    for s in "${sets[@]}"; do printf '%s"%s"' "$sep" "$s"; sep=", "; done
    printf '];\nvar keys = ['
    sep=""
    for k in "${keys[@]}"; do printf '%s"%s"' "$sep" "$k"; sep=", "; done
    printf '];\n'
} > "$work/selection.js"

echo "== painting the masters (${sets[*]}${keys[*]:+: ${keys[*]}}) =="
if command -v xvfb-run >/dev/null 2>&1 && [[ -z "${DISPLAY:-}" ]]; then
    ( cd "$work" && xvfb-run -a -s "-screen 0 1400x1000x24" \
        env QT_QPA_PLATFORM=xcb "$QMLSCENE" -I "$ROOT/qml-stubs" render.qml )
elif [[ -n "${DISPLAY:-}" ]]; then
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
        grep "^qml: OVERLAPS" "$work/painted.log" | grep -v " 0$" >&2
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
painted=("$work"/*.png "$work"/cover/*.png)
if [[ ${#painted[@]} -eq 0 ]]; then
    echo "the painter wrote nothing; see the output above." >&2
    exit 1
fi
if [[ $full_cover -eq 1 ]]; then
    # A full set replaces the directory, so a count that is no longer
    # painted cannot linger as a stale mask.
    rm -f qml/art/cover/*.png
fi
mkdir -p qml/art/cover
for png in "${painted[@]}"; do
    rel=${png#"$work"/}
    scripts/png-mask.py "$png" "qml/art/$rel"
done
echo "  masks are in qml/art/; commit them."
