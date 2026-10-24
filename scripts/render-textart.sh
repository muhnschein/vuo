#!/usr/bin/env bash
# Regenerate qml/art/ texture masks from tools/textart/.
#
# App ships pattern as image, doesn't paint it (qml/components/TextArt.qml says
# why); this turns painter into what ships. Run when painter, strokes or
# densities change; COMMIT result. Like compiled translations, masks generated
# but tracked: build needs no GPU or display.
#
#     scripts/render-textart.sh                 # everything
#     scripts/render-textart.sh cover           # the cover's set only
#     scripts/render-textart.sh cover:42,99+    # two of the cover's masters
#     scripts/render-textart.sh cover:none      # the cover with nothing unread
#     scripts/render-textart.sh onboarding      # the page's one master
#
# Cover set = one mask per unread count, 1 to 99 and "99+", in qml/art/cover/:
# count is negative space, whole background depends on number. "none" = cover
# with nothing unread, no number. Partial run (cover:42) overwrites those,
# leaves rest; full cover run replaces dir.
#
# Needs GL-context QML runtime. Headless: `xvfb-run` supplies one; script finds
# it or says what to install. Cover digits in Fira Sans ExtraBold (SIL OFL),
# fetched once into gitignored font dir like render-store-cover.sh's;
# VUO_FONT_DIR overrides.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QMLSCENE=${QMLSCENE:-$(command -v qmlscene || true)}
if [[ -z "$QMLSCENE" ]] || [[ ! -x "$QMLSCENE" ]]; then
    echo "qmlscene not found. Install qtdeclarative5-dev-tools (Debian) or set QMLSCENE." >&2
    exit 1
fi

# Which sets to paint; for cover which counts.
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
                [[ "$key" =~ ^([1-9][0-9]?|99\+|none)$ ]] \
                    || { echo "cover key '$key' is not 1..99, 99+ or none" >&2; exit 1; }
                keys+=("$key")
            done ;;
        *) echo "unknown set '$arg'; expected onboarding, cover or cover:KEY,..." >&2; exit 1 ;;
    esac
done
full_cover=0
if [[ " ${sets[*]} " == *" cover "* ]] && [[ ${#keys[@]} -eq 0 ]]; then
    full_cover=1
fi

# Digit face. Sail Sans Pro not redistributable; digits only silhouette, weight
# matters more than face. ExtraBold = heaviest where counter of 4 still holds
# text line. TLS only like store cover fonts: -L follows redirects,
# --proto-redir keeps unchosen hop off plain http (§9.1).
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

# Painter reads Theme for font family; needs QML tests' Silica stubs.
#
# FONT NOT DEVICE'S. Sail Sans Pro not redistributable; non-phone renders with
# whatever fontconfig maps name to (default sans). At these sizes letters are
# texture, pattern same, but only respect shipped art differs from device's own.
# Device face for filler if copy at hand. Never fetched (not redistributable):
# copy /usr/share/fonts/sail-sans-pro/SailSansPro-Light.ttf off phone into font
# dir by hand; masks then in device font. Without it: caveat in
# docs/packaging.md.
FILLER="$FONT_DIR/SailSansPro-Light.ttf"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp tools/textart/*.qml "$work/"
cp "$DIGITS" "$work/FiraSans-ExtraBold.ttf"
if [[ -f "$FILLER" ]]; then
    cp "$FILLER" "$work/SailSansPro-Light.ttf"
else
    echo "  no $FILLER: the filler is set in the host's default sans" >&2
fi
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

# Ring traced twice = text over text. Painter counts it; refuse master, don't
# ship.
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
    # Full set replaces dir: no-longer-painted count can't linger as stale mask.
    rm -f qml/art/cover/*.png
fi
mkdir -p qml/art/cover
for png in "${painted[@]}"; do
    rel=${png#"$work"/}
    scripts/png-mask.py "$png" "qml/art/$rel"
done
echo "  masks are in qml/art/; commit them."
