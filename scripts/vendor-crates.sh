#!/usr/bin/env bash
# Build offline crate bundle OBS needs.
#
# §7: "OBS builds have no network. Vendor crate sources and gate the offline
# path behind a spec flag from the start; retrofitting this is tedious."
#
# Outputs rpm/vendor.tar.xz and rpm/vendor.toml (spec Source1/Source2 under
# `--with vendor`). Both gitignored: build products; hundreds of MB third-party
# source would wreck repo.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v cargo >/dev/null || { echo "cargo not found" >&2; exit 127; }

echo "== vendoring crate sources =="
# --locked: bundle matches Cargo.lock exactly. Diverging resolution = OBS build
# differs from all others (what vendoring prevents).
cargo vendor --locked --versioned-dirs rpm/vendor > rpm/vendor.toml

echo "== compressing =="
tar -C rpm -cJf rpm/vendor.tar.xz vendor
rm -rf rpm/vendor

echo
echo "wrote rpm/vendor.tar.xz ($(du -h rpm/vendor.tar.xz | cut -f1))"
echo "wrote rpm/vendor.toml"
echo
echo "Build offline with:  sfdk build -- --with vendor"
