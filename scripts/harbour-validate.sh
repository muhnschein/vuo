#!/usr/bin/env bash
# Run Jolla's own Harbour validator over built RPM.
#
# Usage: scripts/harbour-validate.sh <rpm>
#
# Harbour QA runs `rpmvalidation.sh` from sailfishos/sdk-harbour-rpmvalidator
# on every submission; ERROR = automatic rejection. scripts/check-harbour.sh,
# check-linked-libs.sh, check-main-export.sh hold transcribed rules offline in
# `make check`; this runs upstream script itself over finished package, so a
# rule nobody transcribed fails CI, not intake.
#
# Pinned commit: fetched code runs in CI, so reviewed and fixed. Bump = new
# VALIDATOR_REV, read upstream diff first.
#
# Needs network, rpm, rpm2cpio, cpio, file, objdump, readelf, c++filt. Exit 0
# = validator passed (WARNING/INFO allowed, Harbour accepts them); 1 = failed.
set -euo pipefail

VALIDATOR_REPO=https://github.com/sailfishos/sdk-harbour-rpmvalidator.git
# master, 2026-09-10.
VALIDATOR_REV=7dd7dd5d62aa3e814c92ade089fdca4c3872b8ac

RPM_FILE="${1:?usage: harbour-validate.sh <rpm>}"
[[ -r "$RPM_FILE" ]] || { echo "FAIL: $RPM_FILE is not readable" >&2; exit 1; }

for tool in git rpm rpm2cpio cpio file objdump readelf c++filt; do
    command -v "$tool" >/dev/null 2>&1 \
        || { echo "FAIL: $tool not installed; the validator needs it" >&2; exit 1; }
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git -c advice.detachedHead=false init -q "$work/validator"
git -C "$work/validator" fetch -q --depth 1 "$VALIDATOR_REPO" "$VALIDATOR_REV"
git -C "$work/validator" -c advice.detachedHead=false checkout -q FETCH_HEAD
[[ "$(git -C "$work/validator" rev-parse HEAD)" = "$VALIDATOR_REV" ]] \
    || { echo "FAIL: fetched validator is not $VALIDATOR_REV" >&2; exit 1; }

echo "== Harbour validator ${VALIDATOR_REV:0:12} over $(basename "$RPM_FILE") =="
# -g: read our checkout's config, never an SDK copy in /usr/share. -d1: no
# cpio file list. -c: plain text for CI log.
rc=0
bash "$work/validator/rpmvalidation.sh" -c -d1 -g "$work/validator" "$RPM_FILE" \
    | tee "$work/log" || rc=$?
# Exit status is the verdict; ERROR lines are double check, so a validator that
# printed a rejection can't pass on a lost status.
if [[ "$rc" -ne 0 ]] || grep -q '^ERROR ' "$work/log"; then
    echo "FAIL: Harbour would reject $(basename "$RPM_FILE"):" >&2
    grep '^ERROR ' "$work/log" >&2 || true
    exit 1
fi
echo "  Harbour validator passed"
