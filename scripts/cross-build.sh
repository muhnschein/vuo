#!/usr/bin/env bash
# Cross-build harbour-vuo for SailfishOS device WITHOUT SDK cargo.
#
# `mb2` can't build Vuo: SDK 5.0.0.43 tooling ships cargo 1.75, locked graph
# uses edition2024 (docs/sdk-build.md). Blocker = SDK *cargo*, not *compiler*:
# keep SDK aarch64 GCC 10.3.1 and target sysroot (must match device), drive with
# host cargo.
#
# NOT release build. Output = test package.
#
# Usage: scripts/cross-build.sh [path-to-unpacked-sdk-rootfs]
set -euo pipefail

# Run from repo root: cargo invocation and paths relative to it; cross-rpm.sh
# calls from anywhere.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ROOTFS="${1:-/home/user/sdk/rootfs}"
ARCH=aarch64
# Overridable: SDK target version rootfs unpacked from.
SDK_VERSION="${SDK_VERSION:-5.0.0.43}"
TRIPLE=aarch64-unknown-linux-gnu

SR="$ROOTFS/srv/mer/targets/SailfishOS-${SDK_VERSION}-${ARCH}"
[[ -d "$SR" ]] || { echo "no target sysroot at $SR" >&2; exit 1; }

# GCC resolves cc1, specs, libexec against own absolute prefix: must be
# reachable at /opt/cross, not in place.
if [[ ! -e /opt/cross ]]; then
    ln -sfn "$ROOTFS/opt/cross" /opt/cross
fi
CROSS=/opt/cross/bin/aarch64-meego-linux-gnu
[[ -x "$CROSS-gcc" ]] || { echo "no cross gcc at $CROSS-gcc" >&2; exit 1; }

# GCC calls plain `as` and `ld`; without -B finds host x86 binutils on PATH,
# dies "as: unrecognized option '-EL'". Fixed path, not mktemp: it lands in
# RUSTFLAGS and CFLAGS; random one = different build inputs every run.
BINDIR="$ROOT/target/cross-bin"
mkdir -p "$BINDIR"
for t in as ld ar nm ranlib objcopy objdump strip readelf; do
    ln -sf "$CROSS-$t" "$BINDIR/$t"
done

export VUO_SYSROOT="$SR"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$CROSS-gcc"
export CC_aarch64_unknown_linux_gnu="$CROSS-gcc"
export CXX_aarch64_unknown_linux_gnu="$CROSS-g++"
export AR_aarch64_unknown_linux_gnu="$CROSS-ar"
# Hardening distro %optflags would apply, restated.
#
# In sb2 these come from rpm, reach every C/C++ compile. This route skips rpm
# build env; without them C++ glue (qmetaobject's, main.rs SailfishApp block)
# softer than SDK build. All four GCC 10.3 features (= /opt/cross).
HARDEN="-O2 -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIC"
export CFLAGS_aarch64_unknown_linux_gnu="--sysroot=$SR -B$BINDIR/ $HARDEN"
export CXXFLAGS_aarch64_unknown_linux_gnu="--sysroot=$SR -B$BINDIR/ $HARDEN"

# BOTH set = qttypes reads Qt version from qtcoreversion.h, no unexec-able
# qmake. Same trick as spec.
export QT_INCLUDE_PATH="$SR/usr/include/qt5"
export QT_LIBRARY_PATH="$SR/usr/lib64"

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$SR"
export PKG_CONFIG_LIBDIR="$SR/usr/lib64/pkgconfig:$SR/usr/share/pkgconfig"

# cc1/cc1plus 32-bit, link libmpc/libmpfr/libgmp (only in rootfs). Rest from
# host :i386 packages.
export LD_LIBRARY_PATH="$ROOTFS/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# `-z relro -z now` = link half of hardening: GOT read-only, all symbols
# resolved at load, not lazily; stray-pointer write can't redirect later call.
# Checked after link below.
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
# Only intake rule decided by DEVICE link; bare host can't see it. List in
# scripts/check-linked-libs.sh (`make check` runs it on host build too).
#
# Failure does NOT stop package build: output = test package for phone, best
# place to see breakage. Reported loudly; CI fails job on it. Link half of
# hardening above, read back off ELF. Flag silently dropped (rustflags edit,
# linker ignoring it) only visible here.
echo "-- hardening --"
dyn=$("$BINDIR/readelf" -d "$BIN")
hdr=$("$BINDIR/readelf" -lW "$BIN")
soft=0
case "$hdr" in *GNU_RELRO*) echo "   ok       RELRO" ;; *) echo "   MISSING  RELRO"; soft=1 ;; esac
case "$dyn" in *BIND_NOW*|*"Flags: NOW"*) echo "   ok       BIND_NOW" ;; *) echo "   MISSING  BIND_NOW"; soft=1 ;; esac
case "$(file -b "$BIN")" in *"pie executable"*) echo "   ok       PIE" ;; *) echo "   MISSING  PIE"; soft=1 ;; esac
[[ "$soft" -eq 0 ]] || echo "WARNING: the binary is built softer than an SDK build would be." >&2

echo "-- shared libraries, against Harbour's allowed list --"
if "$ROOT/scripts/check-linked-libs.sh" "$BIN" "$BINDIR/readelf"; then
    :
else
    echo "WARNING: the package is being built anyway; Harbour would refuse it." >&2
fi
