#!/usr/bin/env bash
# Prove third_party/qmetaobject is upstream 0.2.10 plus one known patch.
#
# Carrying someone else's crate in-tree is only safe while the difference is
# visible. So: fetch the crates.io tarball, apply third_party/qmetaobject.patch
# to it, and require the result to match the vendored tree byte for byte. An
# edit made directly to the copy fails here, and so does a patch that has
# stopped describing it.
#
# The .crate tarball is immutable once published, so this is the same
# comparison every time.
#
# NOT part of `make check`, which runs with no network (docs/scope.md §8). It
# is an opt-in gate like `make live-test`, and CI runs it on every push. The
# idea, the patch and this script are all from postivene, which hit the same
# Harbour rule first.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDORED="$ROOT/third_party/qmetaobject"
PATCH_FILE="$ROOT/third_party/qmetaobject.patch"

echo "== vendored qmetaobject =="

[ -d "$VENDORED" ] || { echo "FAIL: $VENDORED is missing" >&2; exit 1; }
[ -f "$PATCH_FILE" ] || { echo "FAIL: $PATCH_FILE is missing" >&2; exit 1; }

# The version to compare against is the one cargo is told to replace.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$VENDORED/Cargo.toml" | head -1)
[ -n "$version" ] || { echo "FAIL: no version in the vendored Cargo.toml" >&2; exit 1; }

if ! command -v curl >/dev/null 2>&1; then
    # A gate that passes without its tool is not a gate. Locally that is a
    # skip; on a runner, where GitHub sets CI, it is a failure.
    if [ -n "${CI:-}" ]; then
        echo "FAIL: curl not found; this check proved nothing" >&2
        exit 1
    fi
    echo "  SKIPPED: curl not found"
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

url="https://static.crates.io/crates/qmetaobject/qmetaobject-$version.crate"
curl -sSfL "$url" -o "$work/crate.tar.gz" \
    || { echo "FAIL: could not fetch $url" >&2; exit 1; }
tar -C "$work" -xzf "$work/crate.tar.gz"
upstream="$work/qmetaobject-$version"
[ -d "$upstream" ] || { echo "FAIL: unexpected tarball layout" >&2; exit 1; }

# The crate's own tests are not vendored. Cargo never builds a dependency's
# tests, so they are lines that cannot run here -- and CodeQL scans whatever is
# in the tree and reports findings in code this repository does not compile.
# Dropped from both sides so the comparison stays exact.
rm -rf "$upstream/tests"

patch -s -p1 -d "$upstream" < "$PATCH_FILE" \
    || { echo "FAIL: qmetaobject.patch does not apply to upstream $version" >&2; exit 1; }

# `.cargo-ok` is dropped by cargo's own extraction, not by the tarball; it is
# excluded rather than deleted from a tree this script only means to read.
if diff -r -q -x .cargo-ok "$upstream" "$VENDORED" >/dev/null 2>&1; then
    echo "  third_party/qmetaobject is qmetaobject $version + qmetaobject.patch"
    echo "  vendored-source check passed"
    exit 0
fi

echo "FAIL: third_party/qmetaobject is not upstream $version plus qmetaobject.patch:" >&2
diff -r -q -x .cargo-ok "$upstream" "$VENDORED" >&2 || true
echo "Either revert the stray edit, or fold it into third_party/qmetaobject.patch." >&2
exit 1
