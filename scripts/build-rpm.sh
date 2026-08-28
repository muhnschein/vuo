#!/usr/bin/env bash
# Build a SailfishOS RPM with the Sailfish SDK.
#
# Rust needs the SDK's **Docker** build engine; the VirtualBox engine cannot
# build it (§7). Expect the first build to be slow.
#
# Usage: scripts/build-rpm.sh [aarch64|armv7hl|i486] [extra sfdk args...]
set -euo pipefail

ARCH="${1:-aarch64}"
shift || true

case "$ARCH" in
    aarch64|armv7hl|i486) ;;
    *) echo "unknown architecture: $ARCH (expected aarch64, armv7hl or i486)" >&2; exit 2 ;;
esac

if ! command -v sfdk >/dev/null 2>&1; then
    cat >&2 <<'MSG'
sfdk not found.

This script needs the SailfishOS SDK, which is not installable on a CI runner
and is not part of `make check` for that reason. Install the SDK and make sure
sfdk is on PATH, and note that Rust requires the SDK's Docker build engine --
the VirtualBox engine cannot build it.
MSG
    exit 127
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TARGET="SailfishOS-latest-${ARCH}"
echo "== building harbour-vuo for ${ARCH} (target ${TARGET}) =="

# --with vendor is not passed here: a local SDK build has network access, so
# vendoring only slows it down. OBS forces it via the spec instead.
sfdk -c target="$TARGET" build "$@"

echo
echo "RPMs:"
find RPMS -name '*.rpm' -newermt '-10 minutes' 2>/dev/null || true
