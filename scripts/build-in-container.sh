#!/bin/sh
# Builds device RPM in pinned containers, via Podman or Docker. Same steps as
# release: rpm.yml runs this too.
#
#     scripts/build-in-container.sh [<sfos-version>]
#
# Version default: 5.2.0.15. Two images:
# - Sailfish SDK (coderus/sailfishos-platform-sdk), by digest from
#   rpm/sdk-images. Only its cross compiler and target sysroot are used,
#   lifted once into VUO_SDK_CACHE; image dropped after if this script pulled
#   it (~13 GB unpacked).
# - Build environment, rpm/Containerfile: cargo, rpm, qemu, all pinned. Built
#   here, tagged by Containerfile hash and Rust release.
# Then scripts/cross-rpm.sh inside, tree at /build/vuo, SDK at /sdk (read-only).
# Output: dist/*.rpm.
#
# Reproducible: same commit, same pins = same RPM, byte for byte
# (docs/verifying.md). SOURCE_DATE_EPOCH (default: commit time) sets package
# build time and file dates; paths and host name fixed.
#
# Environment, all optional:
#   VUO_ENGINE       docker | podman; default: Docker if running, else Podman
#   VUO_RELEASE      RPM Release; default 1 (release package)
#   VUO_CARGO_HOME   host dir for crate downloads; default target/container-cargo
#   VUO_SDK_CACHE    where lifted SDKs go; default ~/.cache/vuo-sdk (~2 GB each)
#   CARGO_BUILD_JOBS passed through
#
# Runs as calling user in both containers' output: tree stays yours. Rootless
# Podman pulling SDK image may need more subordinate ids; it says how.
#
# aarch64 only: scripts/cross-build.sh knows no other.

set -eu

prog=build-in-container
sfos=${1:-5.2.0.15}
arch=aarch64
target=SailfishOS-$sfos-$arch

case "$sfos" in
    *[!0-9.]* | '') echo "$prog: '$sfos' is not a dotted SDK version" >&2; exit 2 ;;
    *) ;;
esac

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

# shellcheck source=scripts/container-engine.sh
. "$repo_root/scripts/container-engine.sh"
pick_engine
echo ">> using $engine$([ "$rootless" = 1 ] && echo ' (rootless)')"

digest=$(awk -v v="$sfos" '$1 == v { print $2; exit }' rpm/sdk-images)
[ -n "$digest" ] || {
    echo "$prog: no SDK image pinned for $sfos in rpm/sdk-images" >&2
    exit 1
}
sdk_image=docker.io/coderus/sailfishos-platform-sdk@$digest

# Commit time, unless caller set one. Uncommitted files still get packaged:
# result won't match a release. Warn, don't refuse.
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
    SOURCE_DATE_EPOCH=$(git log -1 --format=%ct 2>/dev/null || true)
fi
if [ -z "$SOURCE_DATE_EPOCH" ]; then
    echo "$prog: warning: not a git checkout and no SOURCE_DATE_EPOCH;" \
         "package dates will be today's, so it won't match a release" >&2
else
    dirty=$(git status --porcelain 2>/dev/null || true)
    if [ -n "$dirty" ]; then
        echo "$prog: warning: these changes aren't committed; they go into the" \
             "package, so it won't match a release:" >&2
        echo "$dirty" | head -10 >&2
    fi
fi

# -- SDK compiler and sysroot, lifted once per digest -----------------------
#
#   opt/cross                  SDK's aarch64-meego-linux-gnu GCC; must match
#                              device glibc and libstdc++ (docs/sdk-build.md)
#   srv/mer/targets/<target>   sysroot: Qt 5.6, sailfishapp, glibc; minus dev/
#                              (device nodes, unneeded, rootless can't make)
#   usr/lib                    tooling's libmpc/libmpfr/libgmp etc. cc1 needs;
#                              exactly what image's own ldd lists for compiler
#                              binaries, dereferenced. glibc skipped: host
#                              i386 libc serves.
cache=${VUO_SDK_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/vuo-sdk}
sdk=$cache/$sfos-$(echo "$digest" | cut -c8-19)
if [ -f "$sdk/.complete" ]; then
    echo ">> SDK $sfos already lifted: $sdk"
else
    pulled=0
    "$engine" image inspect "$sdk_image" >/dev/null 2>&1 || pulled=1
    ctr_pull "$sdk_image"
    targets=$(ctr_run "$sdk_image" sb2-config -l 2>/dev/null || true)
    echo "$targets" | grep -qx -- "$target" || {
        echo "$prog: no $target in $sdk_image; it has:" >&2
        echo "$targets" >&2
        exit 1
    }
    rm -rf "$sdk"
    mkdir -p "$sdk"
    # Root in container to read everything; files handed back to us after.
    # Rootless: container root already is us.
    if [ "$rootless" = 1 ]; then owner=0:0; else owner="$(id -u):$(id -g)"; fi
    echo ">> lifting $target compiler and sysroot into $sdk"
    # shellcheck disable=SC2016 # expanded by the container's shell.
    ctr_run --user 0 -v "$sdk:/out" -e T="$target" -e OWNER="$owner" "$sdk_image" sh -c '
        set -e
        mkdir -p /out/opt/cross /out/srv/mer/targets/$T /out/usr/lib
        tar -C /opt/cross -cf - . | tar -C /out/opt/cross -xf - --no-same-owner
        tar -C /srv/mer/targets/$T --exclude=./dev -cf - . |
            tar -C /out/srv/mer/targets/$T -xf - --no-same-owner
        libexec=/opt/cross/libexec/gcc/aarch64-meego-linux-gnu
        for b in $libexec/*/cc1 $libexec/*/cc1plus $libexec/*/collect2 $libexec/*/lto1 \
                 $libexec/*/lto-wrapper /opt/cross/bin/aarch64-meego-linux-gnu-*; do
            [ -x "$b" ] || continue
            ldd "$b" 2>/dev/null | awk "/=> \\//{print \$3}"
        done | sort -u | while read -r lib; do
            case "$lib" in
                /usr/lib/*) cp -L "$lib" /out/usr/lib/ ;;
            esac
        done
        ls /out/usr/lib/libmpc.so* /out/usr/lib/libmpfr.so* /out/usr/lib/libgmp.so* >/dev/null
        # Owner can read and remove all: rpm.yml caches dir as runner user.
        # Modes inside sysroot never reach package.
        chmod -R u+rwX /out
        chown -R "$OWNER" /out'
    touch "$sdk/.complete"
    if [ "$pulled" = 1 ]; then
        "$engine" rmi "$sdk_image" >/dev/null 2>&1 || true
    fi
fi

# -- build environment --------------------------------------------------------
rust=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
[ -n "$rust" ] || { echo "$prog: no channel in rust-toolchain.toml" >&2; exit 1; }
tag=$(sha256sum rpm/Containerfile | cut -c1-12)
build_image=localhost/vuo-build:$tag-rust$rust
if "$engine" image inspect "$build_image" >/dev/null 2>&1; then
    echo ">> $build_image already built here"
else
    echo ">> building $build_image"
    "$engine" build -t "$build_image" --build-arg RUST_VERSION="$rust" \
        -f rpm/Containerfile rpm
fi

# -- build --------------------------------------------------------------------
cargo_home=${VUO_CARGO_HOME:-$repo_root/target/container-cargo}
mkdir -p "$cargo_home" dist
echo ">> building in $build_image against $target"
# shellcheck disable=SC2046 # ctr_as_me: several flags or none.
ctr_run $(ctr_as_me) --hostname vuo-build -w /build/vuo \
    -v "$repo_root:/build/vuo" -v "$sdk:/sdk:ro" -v "$cargo_home:/cargo" \
    -e SDK_VERSION="$sfos" -e VUO_RELEASE="${VUO_RELEASE:-1}" \
    ${SOURCE_DATE_EPOCH:+-e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH"} \
    ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS"} \
    "$build_image" scripts/cross-rpm.sh /sdk
