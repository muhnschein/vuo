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

# Run from the repository root: the cargo invocation and the paths below are
# relative to it, and cross-rpm.sh calls this from wherever it was itself run.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ROOTFS="${1:-/home/user/sdk/rootfs}"
ARCH=aarch64
# Overridable: the SDK target version the rootfs was unpacked from.
SDK_VERSION="${SDK_VERSION:-5.0.0.43}"
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
# The hardening the distro's own %optflags would apply, restated.
#
# Inside sb2 these arrive from rpm and reach every C/C++ compile the spec
# drives. This route does not go through rpm's build environment at all, so
# without them the C++ glue -- qmetaobject's, and main.rs's SailfishApp block --
# would be the one part of the package built softer than an SDK build's. All
# four are GCC 10.3 features, which is what /opt/cross is.
HARDEN="-O2 -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIC"
export CFLAGS_aarch64_unknown_linux_gnu="--sysroot=$SR -B$BINDIR/ $HARDEN"
export CXXFLAGS_aarch64_unknown_linux_gnu="--sysroot=$SR -B$BINDIR/ $HARDEN"

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

# `-z relro -z now` is the link half of the same hardening: the GOT is made
# read-only and every symbol is resolved at load rather than lazily, so a
# write through a stray pointer cannot redirect a later call. Checked after
# the link, below.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="\
-C link-arg=--sysroot=$SR \
-C link-arg=-B$BINDIR/ \
-C link-arg=-L$SR/usr/lib64 \
-C link-arg=-L$SR/lib64 \
-C link-arg=-Wl,-z,relro \
-C link-arg=-Wl,-z,now \
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

# -- the shared libraries Harbour allows a package to link ------------------
#
# The one intake rule that is decided by the DEVICE link, so nothing running
# on a bare host can see it. The list lives in scripts/check-linked-libs.sh,
# which `make check` also runs over the host build.
#
# A failure here does NOT stop the package being built: this script's output is
# the test package people install on a phone, and a phone is exactly where you
# want to be when something is wrong. It is reported loudly instead, and CI
# fails the job on it.
# The link half of the hardening above, read back off the ELF. A flag that
# stops being applied -- a rustflags edit, a linker that ignores it -- is
# invisible otherwise, and this is the one place it can be seen.
echo "-- hardening --"
dyn=$("$BINDIR/readelf" -d "$BIN")
hdr=$("$BINDIR/readelf" -lW "$BIN")
soft=0
case "$hdr" in *GNU_RELRO*) echo "   ok       RELRO" ;; *) echo "   MISSING  RELRO"; soft=1 ;; esac
case "$dyn" in *BIND_NOW*|*"Flags: NOW"*) echo "   ok       BIND_NOW" ;; *) echo "   MISSING  BIND_NOW"; soft=1 ;; esac
case "$(file -b "$BIN")" in *"pie executable"*) echo "   ok       PIE" ;; *) echo "   MISSING  PIE"; soft=1 ;; esac
[ "$soft" -eq 0 ] || echo "WARNING: the binary is built softer than an SDK build would be." >&2

echo "-- shared libraries, against Harbour's allowed list --"
if "$ROOT/scripts/check-linked-libs.sh" "$BIN" "$BINDIR/readelf"; then
    :
else
    echo "WARNING: the package is being built anyway; Harbour would refuse it." >&2
fi
