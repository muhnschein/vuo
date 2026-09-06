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

# -- the shared libraries Harbour allows a package to link ------------------
#
# The one Harbour rule that cannot be checked anywhere else: it is decided by
# the DEVICE link, so scripts/check-harbour.sh -- which runs in `make check`,
# where there is no cross toolchain -- cannot see it. Anything not on this
# list fails intake with "Cannot link to shared library".
#
# Transcribed from sailfishos/sdk-harbour-rpmvalidator's
# allowed_libraries.conf as of 2026-09-06, minus the entries no Qt/Rust app
# could reach (SDL2, PulseAudio, Wayland, the codecs). Note what is NOT there:
# libQt5Widgets. qttypes links it unconditionally (build.rs:246) and
# qmetaobject's QmlEngine is a QApplication, so this is a real risk for this
# binary rather than a theoretical one.
echo "-- shared libraries, against Harbour's allowed list --"
harbour_allows() {
    case "$1" in
        libQt5Core.so.5|libQt5Gui.so.5|libQt5Qml.so.5|libQt5Quick.so.5|\
libQt5Network.so.5|libQt5Concurrent.so.5|libQt5DBus.so.5|libQt5Sql.so.5|\
libQt5Svg.so.5|libQt5Xml.so.5|libQt5XmlPatterns.so.5|libQt5Multimedia.so.5|\
libQt5Sensors.so.5|libQt5Positioning.so.5|libQt5WebSockets.so.5|libQt5Location.so.5) ;;
        libsailfishapp.so.1|libsailfishsilica.so.1|libmdeclarativecache5.so.0) ;;
        libqt5embedwidget.so.1|libsailfishwebengine.so.1) ;;
        libEGL.so.1|libGLESv1_CM.so.1|libGLESv2.so.2) ;;
        ld-linux-aarch64.so.1|ld-linux-armhf.so.3|ld-linux.so.2) ;;
        libc.so.6|libm.so.6|libdl.so.2|librt.so.1|libpthread.so.0|libresolv.so.2) ;;
        libstdc++.so.6|libgcc_s.so.1|libz.so.1) ;;
        libcrypto.so.3|libssl.so.3|libsqlite3.so.0|libpng16.so.16|libxml2.so.2) ;;
        libdbus-1.so.3|libglib-2.0.so.0|libgobject-2.0.so.0|libgio-2.0.so.0) ;;
        *) return 1 ;;
    esac
    return 0
}
forbidden=0
for lib in $("$BINDIR/readelf" -d "$BIN" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p'); do
    if harbour_allows "$lib"; then
        echo "   ok       $lib"
    else
        echo "   REJECTED $lib"
        forbidden=$((forbidden + 1))
    fi
done
if [ "$forbidden" -ne 0 ]; then
    echo
    echo "WARNING: $forbidden linked libraries are not on Harbour's allowed list." >&2
    echo "The package will install and run on a device, but Harbour will refuse it." >&2
    echo "See docs/packaging.md, \"Harbour readiness\"." >&2
fi
