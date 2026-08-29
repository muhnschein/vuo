#!/usr/bin/env bash
# Check a built binary against the Harbour rules that can be checked without a
# full RPM: the shared libraries it links, and the symbols it exports.
#
# The real gate is sdk-harbour-rpmvalidator, which needs an RPM and an SDK. This
# is the fast half, so a regression is caught before a twenty-minute cross build
# rather than after one. Both rules here were REAL failures, not hypotheticals:
#
#   ERROR [/usr/bin/harbour-vuo] Cannot link to shared library: libQt5Widgets.so.5
#   ERROR [/usr/bin/harbour-vuo] Binary must export main() symbol for booster to work
#
# Usage: scripts/check-harbour.sh [path-to-binary]
set -euo pipefail

BIN="${1:-target/release/harbour-vuo}"

if [ ! -f "$BIN" ]; then
    echo "  no binary at $BIN; skipping (build it with: cargo build --release -p harbour-vuo)"
    exit 0
fi

fail=0

# --- 1. no forbidden shared library ------------------------------------------
# Qt5Widgets is the one that bites, because qttypes linked it unconditionally
# and qmetaobject's QmlEngine built a QApplication. third_party/ holds the
# patches; PATCHES.md explains them. If this fires, the patches stopped being
# applied -- check that [patch.crates-io] in Cargo.toml still resolves.
FORBIDDEN='libQt5Widgets|libQt5WebKit|libQt5Test|libQt5Xml'
if command -v readelf >/dev/null 2>&1; then
    needed=$(readelf -d "$BIN" 2>/dev/null | sed -n 's/.*NEEDED.*\[\(.*\)\]/\1/p')
    bad=$(printf '%s\n' "$needed" | grep -E "$FORBIDDEN" || true)
    if [ -n "$bad" ]; then
        echo "FAIL: links shared libraries Harbour does not allow:" >&2
        printf '  %s\n' $bad >&2
        echo "      allowed_libraries.conf has no entry for these, so rpmvalidation.sh" >&2
        echo "      fails on both 'Cannot link to' and 'Cannot require'. See PATCHES.md." >&2
        fail=1
    else
        echo "  no forbidden shared library ($(printf '%s\n' "$needed" | grep -c . ) NEEDED entries)"
    fi

    # --- 2. main must be in .dynsym ------------------------------------------
    # The silica-qt5 booster dlopens the binary and looks up `main`. Release
    # builds strip .symtab, so it has to be exported into the dynamic table --
    # crates/harbour-vuo/main.dynlist does that. This is the validator's own
    # pipeline.
    mains=$(readelf --wide --syms "$BIN" 2>/dev/null | c++filt 2>/dev/null \
        | while read -r _ _ _ t _ _ i n; do
              [ "$t" = FUNC ] && [ "$i" != UND ] && echo "$n"
          done | grep -c '^main$' || true)
    if [ "${mains:-0}" -lt 1 ]; then
        echo "FAIL: main is not in the dynamic symbol table." >&2
        echo "      Harbour errors on this, and the booster cannot start the app:" >&2
        echo "      it dlopens the binary and dlsym()s main. See crates/harbour-vuo/main.dynlist." >&2
        fail=1
    else
        echo "  main is exported for the booster"
    fi
else
    echo "  readelf not available; skipping the ELF checks"
fi

[ "$fail" -eq 0 ] || exit 1
echo "  harbour binary checks passed"
