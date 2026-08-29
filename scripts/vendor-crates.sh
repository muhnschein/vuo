#!/usr/bin/env bash
# Produce the offline crate bundle that OBS builds need.
#
# §7: "OBS builds have no network. Vendor crate sources and gate the offline
# path behind a spec flag from the start; retrofitting this is tedious."
#
# Outputs rpm/vendor.tar.xz and rpm/vendor.toml, which the spec picks up as
# Source1 and Source2 under `--with vendor`. Both are gitignored: they are
# build products, and committing several hundred megabytes of third-party
# source would make the repository unusable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v cargo >/dev/null || { echo "cargo not found" >&2; exit 127; }

echo "== vendoring crate sources =="
# --locked so the bundle matches Cargo.lock exactly. A vendor bundle that
# resolved differently from the lockfile would make the OBS build diverge from
# every other build, which is the one thing vendoring is supposed to prevent.
cargo vendor --locked --versioned-dirs rpm/vendor > rpm/vendor.toml

# `cargo vendor` SKIPS path dependencies, and [patch.crates-io] makes the two
# forked crates exactly that -- so the bundle comes out without them, and an
# offline build has no pristine source for scripts/patch-deps.sh to patch. It
# fails with "no pristine source for qttypes 0.2.12" at %build, after the
# vendoring looked like it succeeded.
#
# So put them in by hand. These are the UNPATCHED upstream crates: patch-deps
# applies patches/ to them inside the build. See PATCHES.md.
# Into pristine/, NOT vendor/: vendored crates carry a .cargo-checksum.json
# that cargo verifies, and registry copies have none -- putting them in vendor/
# makes cargo reject the entire directory with a misleading "failed to get
# `base64` as a dependency".
echo "== staging pristine sources for the patched crates =="
mkdir -p rpm/pristine
for spec in qmetaobject-0.2.10 qttypes-0.2.12; do
    src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"$spec" 2>/dev/null | head -1)
    if [ -z "$src" ]; then
        echo "FAIL: $spec is not in the cargo registry; run 'cargo fetch' first." >&2
        exit 1
    fi
    cp -a "$src" "rpm/pristine/$spec"
    echo "  $spec"
done

echo "== compressing =="
tar -C rpm -cJf rpm/vendor.tar.xz vendor pristine
rm -rf rpm/vendor rpm/pristine

echo
echo "wrote rpm/vendor.tar.xz ($(du -h rpm/vendor.tar.xz | cut -f1))"
echo "wrote rpm/vendor.toml"
echo
echo "Build offline with:  sfdk build -- --with vendor"
