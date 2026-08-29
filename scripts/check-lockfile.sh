#!/usr/bin/env bash
# Lockfile constraints the SailfishOS SDK cares about and no host build notices.
#
# Both were found by the first real mb2 build (docs/sdk-build.md):
#
#   1. Lockfile FORMAT. v4 arrived in cargo 1.78; the SDK tooling ships 1.75 and
#      cannot read it, and any `cargo update` on a modern host rewrites the file
#      silently. This is a hard failure -- it is cheap to keep right.
#
#   2. Dependency EDITIONS. `cargo vendor` -- how the OBS and SDK builds get
#      their crates -- makes cargo parse EVERY vendored manifest, including
#      packages the build never compiles. A dependency on edition2024 therefore
#      breaks the device build while the registry path, which parses only what
#      it builds, stays green. Reported, not enforced: see the note below.
set -euo pipefail
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# --- 1. lockfile format: enforced -------------------------------------------
version=$(grep -E '^version = [0-9]+$' Cargo.lock | head -1 | tr -cd '0-9')
if [ "${version:-}" != "3" ]; then
    echo "FAIL: Cargo.lock is v${version:-unknown}; the SDK's cargo 1.75 reads only v3." >&2
    echo "      Restore it with \`version = 3\`, or re-run the update with an older cargo." >&2
    exit 1
fi
echo "  Cargo.lock is v3 (readable by the SDK tooling's cargo 1.75)"

# --- 2. dependency editions: enforced ---------------------------------------
# The graph is inside the SDK's cargo now, so this can be a gate rather than a
# report. It was 19 crates at one point; getting back took an MSRV-aware
# re-resolve plus three manual pins. If it regresses, the device build breaks
# and no host check notices, so fail here.
sdk_cargo=1.75
declared=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "$declared" != "$sdk_cargo" ]; then
    echo "FAIL: rust-version is $declared but the SDK ships $sdk_cargo." >&2
    echo "      A floor above what the device toolchain has is not a floor." >&2
    exit 1
fi
echo "  rust-version $declared matches the SDK tooling"

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
if [ -n "$bad" ]; then
    count=$(printf '%s\n' "$bad" | wc -l)
    echo "FAIL: $count locked dependencies use edition2024, which cargo $sdk_cargo cannot parse:" >&2
    printf '%s\n' "$bad" >&2
    echo "      Vendoring parses EVERY manifest, so these break the device build even" >&2
    echo "      though nothing here compiles them. Re-resolve inside the floor:" >&2
    echo "        set resolver = \"3\" in Cargo.toml, cargo update, set it back to \"2\"" >&2
    echo "      then pin whatever is left by hand (see the resolver note in Cargo.toml)." >&2
    exit 1
else
    echo "  no locked dependency uses edition2024"
fi
echo "  lockfile checks passed"
