#!/usr/bin/env bash
# Package the output of scripts/cross-build.sh as an aarch64 RPM.
#
# rpm/harbour-vuo.spec stays the source of truth; this drives its %install and
# %files half over an already-cross-built binary, because the SDK's own build
# cannot run (docs/sdk-build.md). The output is a test package, not a release.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
ROOTFS="${1:-/home/user/sdk/rootfs}"
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

# Compile the translations with the SDK's OWN lrelease, under qemu.
#
# The host's /usr/bin/lrelease is a qtchooser symlink resolving to a Qt that is
# not installed here, and a .qm built by a newer Qt is not guaranteed readable
# by the device's Qt 5.6 anyway. The SDK ships the matching one.
SR_TR="$ROOTFS/srv/mer/targets/SailfishOS-${SDK_VERSION:-5.0.0.43}-aarch64"
if [ -x "$SR_TR/usr/lib64/qt5/bin/lrelease" ] && ls translations/*.ts >/dev/null 2>&1; then
    echo "== compiling translations =="
    qemu-aarch64-static -L "$SR_TR" \
        -E LD_LIBRARY_PATH="$SR_TR/usr/lib64:$SR_TR/lib64" \
        "$SR_TR/usr/lib64/qt5/bin/lrelease" translations/*.ts
else
    echo "warning: no lrelease or no .ts files; the package will be English only" >&2
fi

cp "$BIN" harbour-vuo.desktop LICENSE "$TOP/SOURCES/"
cp -r icons qml "$TOP/SOURCES/"
if ls translations/*.qm >/dev/null 2>&1; then
    mkdir -p "$TOP/SOURCES/translations"
    cp translations/*.qm "$TOP/SOURCES/translations/"
fi
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
    --define "vuo_release ${VUO_RELEASE:-1}" \
    --target aarch64

mkdir -p dist
cp "$TOP"/RPMS/aarch64/*.rpm dist/
echo
echo "RPMs:"
ls -lh dist/*.rpm
