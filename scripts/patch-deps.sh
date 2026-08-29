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
        "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"$name-$version" \
        /root/.cargo/registry/src/*/"$name-$version"
    do
        if [ -f "$candidate/Cargo.toml" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

materialise() {
    local name=$1 version=$2 patch=$3 marker=$4
    local dest="$ROOT/third_party/$name"

    if [ -f "$dest/.patched" ] && grep -q "$version" "$dest/.patched" 2>/dev/null; then
        echo "  $name $version already patched"
        return 0
    fi

    local src
    if ! src=$(find_source "$name" "$version"); then
        echo "FAIL: no pristine source for $name $version." >&2
        echo "      Looked in vendor/ and the cargo registry. Run 'cargo fetch'," >&2
        echo "      or 'cargo vendor' if you are preparing an offline SDK build." >&2
        exit 1
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
materialise qttypes     "$QTTYPES_VERSION"     qttypes-0.2.12-drop-widgets.patch        '^[[:space:]]*link_lib\("Widgets"\)'
materialise qmetaobject "$QMETAOBJECT_VERSION" qmetaobject-0.2.10-qguiapplication.patch '^[[:space:]]*#include <QtWidgets/QApplication>'
echo "  done"
