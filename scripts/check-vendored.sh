#!/usr/bin/env bash
# Prove third_party/qmetaobject = upstream 0.2.10 + one known patch.
#
# In-tree copy of someone's crate safe only while difference visible. Fetch
# crates.io tarball, apply third_party/qmetaobject.patch, require byte-for-byte
# match with vendored tree. Direct edit to copy fails; so does patch no longer
# describing it.
#
# .crate tarball immutable once published: same comparison every time.
#
# NOT in `make check` (offline, docs/scope.md §8). Opt-in gate like `make
# live-test`; CI runs every push. Idea, patch, script from postivene (hit same
# Harbour rule first).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDORED="$ROOT/third_party/qmetaobject"
PATCH_FILE="$ROOT/third_party/qmetaobject.patch"

echo "== vendored qmetaobject =="

[[ -d "$VENDORED" ]] || { echo "FAIL: $VENDORED is missing" >&2; exit 1; }
[[ -f "$PATCH_FILE" ]] || { echo "FAIL: $PATCH_FILE is missing" >&2; exit 1; }

# Compare against version cargo told to replace.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$VENDORED/Cargo.toml" | head -1)
[[ -n "$version" ]] || { echo "FAIL: no version in the vendored Cargo.toml" >&2; exit 1; }

if ! command -v curl >/dev/null 2>&1; then
    # Gate passing without its tool isn't gate. Local: skip; runner (GitHub sets
    # CI): failure.
    if [[ -n "${CI:-}" ]]; then
        echo "FAIL: curl not found; this check proved nothing" >&2
        exit 1
    fi
    echo "  SKIPPED: curl not found"
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

url="https://static.crates.io/crates/qmetaobject/qmetaobject-$version.crate"
# --proto/--proto-redir: see render-store-cover.sh note. Crate checksum verified
# after, but downgrade still leaks which crate, to whom.
curl -sSfL --proto '=https' --proto-redir '=https' "$url" -o "$work/crate.tar.gz" \
    || { echo "FAIL: could not fetch $url" >&2; exit 1; }
tar -C "$work" -xzf "$work/crate.tar.gz"
upstream="$work/qmetaobject-$version"
[[ -d "$upstream" ]] || { echo "FAIL: unexpected tarball layout" >&2; exit 1; }

# Crate's own tests not vendored. Cargo never builds dependency tests
# (unrunnable here); CodeQL scans tree, reports findings in uncompiled code.
# Dropped both sides; comparison stays exact.
rm -rf "$upstream/tests"

patch -s -p1 -d "$upstream" < "$PATCH_FILE" \
    || { echo "FAIL: qmetaobject.patch does not apply to upstream $version" >&2; exit 1; }

# `.cargo-ok` made by cargo's extraction, not tarball; excluded, not deleted
# (script only reads tree).
if diff -r -q -x .cargo-ok "$upstream" "$VENDORED" >/dev/null 2>&1; then
    echo "  third_party/qmetaobject is qmetaobject $version + qmetaobject.patch"
    echo "  vendored-source check passed"
    exit 0
fi

echo "FAIL: third_party/qmetaobject is not upstream $version plus qmetaobject.patch:" >&2
diff -r -q -x .cargo-ok "$upstream" "$VENDORED" >&2 || true
echo "Either revert the stray edit, or fold it into third_party/qmetaobject.patch." >&2
exit 1
