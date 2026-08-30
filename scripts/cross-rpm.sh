#!/usr/bin/env bash
# Package the output of scripts/cross-build.sh as an aarch64 RPM.
#
# rpm/harbour-vuo.spec stays the source of truth; this drives its %install and
# %files half over an already-cross-built binary, because the SDK's own build
# cannot run (docs/sdk-build.md). The output is a test package, not a release.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
TRIPLE=aarch64-unknown-linux-gnu
BIN="target/$TRIPLE/release/harbour-vuo"

# Build, every time, rather than packaging whatever happens to be on disk.
#
# This used to be a bare `[ -f "$BIN" ]` guard with "run scripts/cross-build.sh
# first" in the message. That only catches a MISSING binary, never a stale one
# -- so an RPM was once shipped to a device with thirteen hours of Rust changes
# absent and only the QML fresh. Every new model role read as empty and every
# new method was undefined, which on the device looked like five unrelated
# feature bugs. Cargo makes the no-op case cheap; the guard did not make the
# wrong case detectable.
"$ROOT/scripts/cross-build.sh" "$@"
[ -f "$BIN" ] || { echo "cross-build.sh produced no $BIN" >&2; exit 1; }

command -v rpmbuild >/dev/null || { echo "rpmbuild not found" >&2; exit 127; }

TOP="$(mktemp -d)"
trap 'rm -rf "$TOP"' EXIT
mkdir -p "$TOP"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

cp "$BIN" harbour-vuo.desktop LICENSE "$TOP/SOURCES/"
cp -r icons qml systemd "$TOP/SOURCES/"
cp rpm/harbour-vuo-cross.spec "$TOP/SPECS/"

# This box is x86_64 and the package is aarch64; without the compat entry
# rpmbuild refuses with "No compatible architectures found for build".
cat > "$TOP/rpmrc" <<'RC'
include: /usr/lib/rpm/rpmrc
buildarch_compat: x86_64: aarch64
arch_compat: x86_64: aarch64
RC

rpmbuild -bb "$TOP/SPECS/harbour-vuo-cross.spec" \
    --rcfile "/usr/lib/rpm/rpmrc:$TOP/rpmrc" \
    --define "_topdir $TOP" \
    --define "_userunitdir /usr/lib/systemd/user" \
    --target aarch64

mkdir -p dist
cp "$TOP"/RPMS/aarch64/*.rpm dist/
echo
echo "RPMs:"
ls -lh dist/*.rpm
