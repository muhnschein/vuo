#!/bin/sh
# Copy fuzz target's committed seeds into working corpus.
#
# §8.3: targets "seeded from the snapshot corpus". `fuzz/corpus/` gitignored
# (generated, unbounded), so CI otherwise starts EMPTY. Measured at 60-second PR
# budget: `entry_deserialise` from empty never produced valid domain `Entry`
# (~3M execs per run, per-item validation assertions never reached);
# `content_transform` never built >256 blocks, cap assertions couldn't fire.
#
# Seeding isn't fuzzing substitute; puts fuzzer inside input language so
# mutation has somewhere to go.
#
# Usage: scripts/fuzz-seed.sh <target>...
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fuzz="$root/crates/vuo-core/fuzz"

for target in "$@"; do
    seeds="$fuzz/seeds/$target"
    corpus="$fuzz/corpus/$target"
    if [ ! -d "$seeds" ]; then
        echo "no seeds for target '$target' at $seeds" >&2
        exit 1
    fi
    mkdir -p "$corpus"
    # -n: never overwrite. Nightly restores corpus from cache (seeds + fuzzer's
    # minimised finds); clobbering loses that.
    cp -n "$seeds"/* "$corpus"/ 2>/dev/null || true
    echo "seeded $target: $(ls -1 "$corpus" | wc -l | tr -d ' ') corpus files"
done
