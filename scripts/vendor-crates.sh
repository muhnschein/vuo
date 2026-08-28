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

echo "== compressing =="
tar -C rpm -cJf rpm/vendor.tar.xz vendor
rm -rf rpm/vendor

echo
echo "wrote rpm/vendor.tar.xz ($(du -h rpm/vendor.tar.xz | cut -f1))"
echo "wrote rpm/vendor.toml"
echo
echo "Build offline with:  sfdk build -- --with vendor"
