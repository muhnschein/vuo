#!/usr/bin/env bash
# Materialise third_party/ -- the two forked crates -- from pristine sources.
#
# WHY A SCRIPT AND NOT VENDORED SOURCE
#
# qmetaobject and qttypes are patched (see PATCHES.md) so the binary stops
# linking libQt5Widgets.so.5, which Harbour does not allow. That is four
# changed lines. Committing the crates outright would put ~440 KB of unmodified
# upstream Rust in this repository and bury those four lines in it -- every
# review of a dependency bump would be a review of a vendored tree. The patches
# in patches/ are the diff, and this script is how they get applied.
#
# Run before any build: `cargo build` fails immediately if the paths named by
# [patch.crates-io] do not exist. `make` targets do this for you, and the spec
# runs it in %build.
#
# Idempotent: re-running is a no-op once the trees are in place and patched.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$(pwd)

QMETAOBJECT_VERSION=0.2.10
QTTYPES_VERSION=0.2.12

# crates.io SHA-256 of each .crate tarball, taken from the lockfile entries
# these patches replace. Every downloaded archive is checked against them, so a
# fetched source is verified rather than trusted -- which is more than a warm
# registry cache gives us.
QMETAOBJECT_SHA256=426a57e85d36f055a0c82cb0a8a261d49ba051ab2a2ef5471835f69d477816cd
QTTYPES_SHA256=c7edf5b38c97ad8900ad2a8418ee44b4adceaa866a4a3405e2f1c909871d7ebd

# Pristine sources come from wherever this machine already has them: the cargo
# registry on a dev host, or pristine/ inside the SDK chroot, where there is no
# crates.io route at all.
#
# pristine/ is a SEPARATE directory, not vendor/. Vendored crates carry a
# .cargo-checksum.json that cargo verifies, and registry copies have none --
# dropping them into vendor/ makes cargo reject the whole directory with an
# unrelated-looking "failed to get `base64` as a dependency".
find_source() {
    local name=$1 version=$2 candidate
    for candidate in \
        "$ROOT/pristine/$name-$version" \
        "$ROOT/.patch-deps-cache/$name-$version" \
        "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"$name-$version"
    do
        if [ -f "$candidate/Cargo.toml" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

# Download a crate straight from crates.io, checksum it, unpack it.
#
# Needed because there is a bootstrap cycle otherwise: this script exists to
# create the paths [patch.crates-io] names, and `cargo fetch` -- the obvious way
# to populate the registry -- cannot run until those paths exist. A fresh CI
# checkout has an empty registry, so "just run cargo first" is not available.
# Every job failed here on exactly that.
#
# Not used by the SDK build: pristine/ is searched first and ships in the vendor
# tarball, so the offline path never reaches this.
download_source() {
    local name=$1 version=$2 want=$3
    local cache="$ROOT/.patch-deps-cache"
    local dest="$cache/$name-$version"
    local tarball="$cache/$name-$version.crate"

    command -v curl >/dev/null || return 1
    mkdir -p "$cache"

    curl -sSfL -o "$tarball" \
        "https://static.crates.io/crates/$name/$name-$version.crate" || return 1

    local got
    got=$(sha256sum "$tarball" | cut -d" " -f1)
    if [ "$got" != "$want" ]; then
        echo "FAIL: checksum mismatch for $name $version" >&2
        echo "      expected $want" >&2
        echo "      got      $got" >&2
        rm -f "$tarball"
        exit 1
    fi

    mkdir -p "$dest"
    tar -xzf "$tarball" -C "$dest" --strip-components=1
    rm -f "$tarball"
    printf '%s\n' "$dest"
}

materialise() {
    local name=$1 version=$2 sha256=$3 patch=$4 marker=$5
    local dest="$ROOT/third_party/$name"

    if [ -f "$dest/.patched" ] && grep -q "$version" "$dest/.patched" 2>/dev/null; then
        echo "  $name $version already patched"
        return 0
    fi

    local src
    if ! src=$(find_source "$name" "$version"); then
        echo "  $name $version not present locally; fetching from crates.io"
        if ! src=$(download_source "$name" "$version" "$sha256"); then
            echo "FAIL: no source for $name $version, and the download failed." >&2
            echo "      Looked in pristine/, .patch-deps-cache/ and the cargo" >&2
            echo "      registry. With no network, populate one of those --" >&2
            echo "      'cargo vendor' produces pristine/ for the SDK build." >&2
            exit 1
        fi
    fi

    echo "  $name $version <- $src"
    rm -rf "$dest"
    mkdir -p "$(dirname "$dest")"
    cp -a "$src" "$dest"
    chmod -R u+w "$dest"

    # Not built by a path dependency, and not worth carrying.
    rm -rf "$dest/tests" "$dest/README.md" "$dest/Cargo.toml.orig" \
           "$dest/.cargo-ok" "$dest/.cargo_vcs_info.json"

    patch -s -p1 -d "$dest" < "$ROOT/patches/$patch"

    # Prove the patch did what it claims rather than trusting exit status: a
    # fuzzy apply can succeed against the wrong context. The marker is a regex
    # anchored to the start of a line so the comments the patches leave behind
    # -- which name what they removed -- do not count as the thing itself.
    if grep -rqE "$marker" "$dest"; then
        echo "FAIL: $name still has an active '$marker' after patching." >&2
        grep -rnE "$marker" "$dest" >&2
        exit 1
    fi

    echo "$name $version patched $(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$dest/.patched"
}

echo "== patching forked dependencies =="
materialise qttypes "$QTTYPES_VERSION" "$QTTYPES_SHA256" \
    qttypes-0.2.12-drop-widgets.patch '^[[:space:]]*link_lib\("Widgets"\)'
materialise qmetaobject "$QMETAOBJECT_VERSION" "$QMETAOBJECT_SHA256" \
    qmetaobject-0.2.10-qguiapplication.patch '^[[:space:]]*#include <QtWidgets/QApplication>'
echo "  done"
