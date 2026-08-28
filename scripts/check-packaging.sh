#!/usr/bin/env bash
# Packaging sanity checks that need no SDK, so they can run in `make check`.
#
# These catch the packaging mistakes that are otherwise only discovered by a
# 40-minute SDK build: a file referenced by the spec that does not exist, a
# desktop entry that fails validation, a version that has drifted between
# Cargo.toml and the spec.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail=0
note() { echo "  $*"; }
bad() { echo "FAIL: $*" >&2; fail=1; }

echo "== packaging checks =="

SPEC=rpm/harbour-vuo.spec
[ -f "$SPEC" ] || { bad "$SPEC is missing"; exit 1; }

# 1. The spec's version must match the workspace's.
spec_version=$(sed -n 's/^Version:[[:space:]]*//p' "$SPEC" | head -1)
cargo_version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "$spec_version" != "$cargo_version" ]; then
    bad "version drift: spec says $spec_version, Cargo.toml says $cargo_version"
else
    note "version $spec_version matches Cargo.toml"
fi

# 2. Every file the spec installs must exist.
for f in harbour-vuo.desktop \
         systemd/harbour-vuo-sync.service \
         systemd/harbour-vuo-sync.timer \
         qml/harbour-vuo.qml \
         LICENSE; do
    [ -e "$f" ] || bad "the spec installs $f, which does not exist"
done
for res in 86x86 108x108 128x128 172x172; do
    [ -e "icons/$res/harbour-vuo.png" ] || bad "missing icon icons/$res/harbour-vuo.png"
done
note "every file the spec installs is present"

# 3. The desktop entry must validate.
if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate harbour-vuo.desktop || bad "harbour-vuo.desktop failed validation"
    note "desktop entry validates"
else
    note "desktop-file-validate not installed; skipping (CI installs it)"
fi

# 4. The spec must build the binary explicitly. The workspace's
#    default-members are the Qt-free set, so a bare `cargo build --release`
#    would silently produce no installable binary.
grep -q -- '--bin harbour-vuo' "$SPEC" || \
    bad "the spec must pass --bin harbour-vuo; default-members would otherwise build nothing"
grep -q -- '--features sailfishapp' "$SPEC" || \
    bad "the spec must pass --features sailfishapp for the device entry point"
note "the spec builds the right binary with the right features"

# 5. Cargo.lock must be committed: OBS builds --locked and offline.
[ -f Cargo.lock ] || bad "Cargo.lock must be committed for reproducible offline builds"
note "Cargo.lock is present"

if [ "$fail" -ne 0 ]; then
    echo "packaging checks FAILED" >&2
    exit 1
fi
echo "  packaging checks passed"
