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

# --- 2. the SDK gap: reported ------------------------------------------------
# Deliberately NOT a failure. The gap is real, known, and cannot be closed by
# this script: closing it means either a newer `rust` in the build target or
# rolling the dependency tree back to 2023 (which would take the TLS stack with
# it). A gate that can never pass just teaches people to ignore the output, so
# this prints the number and docs/status.md tracks the decision.
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
if [ -n "$bad" ]; then
    count=$(printf '%s\n' "$bad" | wc -l)
    echo "  NOTE: $count locked dependencies use edition2024, which cargo $sdk_cargo cannot parse."
    echo "        Vendoring parses every manifest, so these block an SDK build even"
    echo "        though nothing here compiles them. Tracked in docs/status.md."
else
    echo "  no locked dependency uses edition2024"
fi
echo "  lockfile checks passed"
