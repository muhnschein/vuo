#!/usr/bin/env bash
# Package scripts/cross-build.sh output as aarch64 RPM.
#
# rpm/harbour-vuo.spec stays source of truth; this drives its %install/%files
# half over cross-built binary (SDK build can't run, docs/sdk-build.md). Test
# package, not release.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
ROOTFS="${1:-/home/user/sdk/rootfs}"
TRIPLE=aarch64-unknown-linux-gnu
BIN="target/$TRIPLE/release/harbour-vuo"

# Build every time; never package whatever's on disk.
#
# Existence guard catches MISSING binary, never stale one: stale Rust + fresh
# QML = new model roles empty, new methods undefined, looks like unrelated
# feature bugs on device. Cargo no-op build cheap.
"$ROOT/scripts/cross-build.sh" "$@"
[[ -f "$BIN" ]] || { echo "cross-build.sh produced no $BIN" >&2; exit 1; }

command -v rpmbuild >/dev/null || { echo "rpmbuild not found" >&2; exit 127; }

TOP="$(mktemp -d)"
trap 'rm -rf "$TOP"' EXIT
mkdir -p "$TOP"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

# Compile translations with SDK's OWN lrelease, under qemu.
#
# Host /usr/bin/lrelease = qtchooser symlink to uninstalled Qt; newer-Qt .qm not
# guaranteed readable by device Qt 5.6. SDK ships matching one.
SR_TR="$ROOTFS/srv/mer/targets/SailfishOS-${SDK_VERSION:-5.0.0.43}-aarch64"
if [[ -x "$SR_TR/usr/lib64/qt5/bin/lrelease" ]] && ls translations/*.ts >/dev/null 2>&1; then
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

# Box x86_64, package aarch64; without compat entry rpmbuild refuses "No
# compatible architectures found for build".
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
