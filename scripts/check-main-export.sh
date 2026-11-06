#!/usr/bin/env bash
# Check binary exports `main` in its dynamic symbol table.
#
# Usage: scripts/check-main-export.sh <binary> [readelf]
#
# Booster dlopen()s app binary, dlsym()s `main`. Harbour rpmvalidation.sh
# (validatesymbols) refuses Silica app without defined FUNC `main` in
# `readelf --syms`; stripped binary has only .dynsym, so read .dynsym here: host
# debug build unstripped, .symtab would hide missing export. Exported by
# crates/harbour-vuo/build.rs `--dynamic-list`.
set -euo pipefail

BIN="${1:?usage: check-main-export.sh <binary> [readelf]}"
READELF="${2:-readelf}"
[[ -r "$BIN" ]] || { echo "FAIL: $BIN is not readable" >&2; exit 1; }

# Columns: Num Value Size Type Bind Vis Ndx Name.
if "$READELF" --wide --dyn-syms "$BIN" \
    | awk '$4 == "FUNC" && $5 == "GLOBAL" && $7 != "UND" && $8 == "main" { found = 1 } END { exit !found }'; then
    echo "  main exported in .dynsym (booster can start it)"
else
    echo "FAIL: $BIN does not export main in .dynsym." >&2
    echo "Harbour refuses it: \"Binary must export main() symbol for booster to work\"." >&2
    exit 1
fi
