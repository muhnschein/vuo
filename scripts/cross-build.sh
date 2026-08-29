#!/usr/bin/env bash
# Cross-build harbour-vuo for a SailfishOS device WITHOUT the SDK's cargo.
#
# Why this exists: `mb2` cannot build Vuo, because the SDK 5.0.0.43 tooling
# ships cargo 1.75 and the locked graph uses edition2024 (docs/sdk-build.md).
# The blocker is the SDK's *cargo*, not its *compiler* -- so this keeps the
# SDK's aarch64 GCC 10.3.1 and its target sysroot, which are the parts that
# must match the device, and drives them with the host's cargo.
#
# It is NOT how a release should be built. The output is a test package.
#
# Usage: scripts/cross-build.sh [path-to-unpacked-sdk-rootfs]
set -euo pipefail

ROOTFS="${1:-/home/user/sdk/rootfs}"
ARCH=aarch64
SDK_VERSION=5.0.0.43
TRIPLE=aarch64-unknown-linux-gnu

SR="$ROOTFS/srv/mer/targets/SailfishOS-${SDK_VERSION}-${ARCH}"
[ -d "$SR" ] || { echo "no target sysroot at $SR" >&2; exit 1; }

# GCC resolves cc1, its specs and its libexec against its own absolute install
# prefix, so it has to be reachable at /opt/cross rather than in place.
if [ ! -e /opt/cross ]; then
    ln -sfn "$ROOTFS/opt/cross" /opt/cross
fi
CROSS=/opt/cross/bin/aarch64-meego-linux-gnu
[ -x "$CROSS-gcc" ] || { echo "no cross gcc at $CROSS-gcc" >&2; exit 1; }

# GCC invokes plain `as` and `ld`; without -B it finds the host's x86 binutils
# on PATH and dies with "as: unrecognized option '-EL'".
BINDIR="$(mktemp -d)"
trap 'rm -rf "$BINDIR"' EXIT
for t in as ld ar nm ranlib objcopy objdump strip readelf; do
    ln -sf "$CROSS-$t" "$BINDIR/$t"
done

export VUO_SYSROOT="$SR"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$CROSS-gcc"
export CC_aarch64_unknown_linux_gnu="$CROSS-gcc"
export CXX_aarch64_unknown_linux_gnu="$CROSS-g++"
export AR_aarch64_unknown_linux_gnu="$CROSS-ar"
export CFLAGS_aarch64_unknown_linux_gnu="--sysroot=$SR -B$BINDIR/"
export CXXFLAGS_aarch64_unknown_linux_gnu="--sysroot=$SR -B$BINDIR/"

# Setting BOTH makes qttypes read the Qt version out of qtcoreversion.h rather
# than shelling out to a qmake it cannot exec. Same trick as the spec.
export QT_INCLUDE_PATH="$SR/usr/include/qt5"
export QT_LIBRARY_PATH="$SR/usr/lib64"

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$SR"
export PKG_CONFIG_LIBDIR="$SR/usr/lib64/pkgconfig:$SR/usr/share/pkgconfig"

# cc1/cc1plus are 32-bit and link libmpc/libmpfr/libgmp, which exist only
# inside the rootfs. The rest comes from the host's :i386 packages.
export LD_LIBRARY_PATH="$ROOTFS/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="\
-C link-arg=--sysroot=$SR \
-C link-arg=-B$BINDIR/ \
-C link-arg=-L$SR/usr/lib64 \
-C link-arg=-L$SR/lib64 \
-C link-arg=-Wl,-rpath-link,$SR/usr/lib64 \
-C link-arg=-Wl,-rpath-link,$SR/lib64"

echo "== cross-building harbour-vuo for $ARCH =="
cargo build --release --locked \
    --package harbour-vuo --bin harbour-vuo \
    --features sailfishapp --target "$TRIPLE"

BIN="target/$TRIPLE/release/harbour-vuo"
echo
echo "== $BIN =="
file "$BIN"
echo "-- highest versioned symbols required (must not exceed the device's) --"
"$BINDIR/readelf" --version-info "$BIN" | grep -oE "GLIBC_2\.[0-9]+|GLIBCXX_3\.4(\.[0-9]+)?" | sort -uV | tail -4
