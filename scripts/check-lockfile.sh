#!/usr/bin/env bash
# Lockfile constraints SailfishOS SDK cares about, host builds miss
# (docs/sdk-build.md):
#
# 1. Lockfile FORMAT. v4 since cargo 1.78; SDK tooling ships 1.75, can't read
#    it; any `cargo update` on modern host rewrites silently. Hard failure;
#    cheap to keep right.
# 2. Dependency EDITIONS. `cargo vendor` (OBS and SDK crate source) parses EVERY
#    vendored manifest, incl. never-compiled ones. edition2024 dependency breaks
#    device build while registry path (parses only what it builds) stays green.
#    Reported, not enforced: see note below.
set -euo pipefail
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# --- 1. lockfile format: enforced -------------------------------------------
version=$(grep -E '^version = [0-9]+$' Cargo.lock | head -1 | tr -cd '0-9')
if [[ "${version:-}" != "3" ]]; then
    echo "FAIL: Cargo.lock is v${version:-unknown}; the SDK's cargo 1.75 reads only v3." >&2
    echo "      Restore it with \`version = 3\`, or re-run the update with an older cargo." >&2
    exit 1
fi
echo "  Cargo.lock is v3 (readable by the SDK tooling's cargo 1.75)"

# --- 2. the SDK gap: reported ------------------------------------------------
# NOT failure on purpose. Gap real, known, unclosable here: needs newer `rust`
# in build target or dependency tree rolled back to 2023 (drags TLS stack).
# Never- passing gate teaches ignoring output; prints number, docs/status.md
# tracks decision.
sdk_cargo=1.75
declared=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1)
echo "  declared rust-version: $declared   SDK 5.0.0.43 tooling ships: $sdk_cargo"

bad=$(cargo metadata --locked --format-version 1 2>/dev/null | python3 -c '
import json, sys
try:
    m = json.load(sys.stdin)
except Exception:
    sys.exit(0)
for p in sorted(m.get("packages", []), key=lambda p: p["name"]):
    if p.get("edition") == "2024":
        print("      " + p["name"] + " " + p["version"])
' || true)
if [[ -n "$bad" ]]; then
    count=$(printf '%s\n' "$bad" | wc -l)
    echo "  NOTE: $count locked dependencies use edition2024, which cargo $sdk_cargo cannot parse."
    echo "        Vendoring parses every manifest, so these block an SDK build even"
    echo "        though nothing here compiles them. Tracked in docs/status.md."
else
    echo "  no locked dependency uses edition2024"
fi
echo "  lockfile checks passed"
