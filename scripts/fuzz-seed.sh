#!/bin/sh
# Copy a fuzz target's committed seeds into its working corpus.
#
# Why this exists: §8.3 says the targets are "seeded from the snapshot corpus",
# and they were not. `fuzz/corpus/` is gitignored (it is generated, and grows
# without bound), so every CI run started from an EMPTY corpus. Measured on the
# 60-second PR budget, four independent runs of `entry_deserialise` from empty
# never once produced a valid domain `Entry` -- about three million executions
# each, and the per-item validation assertions, which are the point of the
# target, were never reached. `content_transform` never built a document with
# more than 256 blocks either, so its cap assertions could not fire: removing
# BOTH `max_blocks` enforcement points and fuzzing for seven minutes found
# nothing.
#
# Seeding is not a substitute for fuzzing; it is what puts the fuzzer inside
# the input language so that mutation has somewhere to go.
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
    # -n: never overwrite. On the nightly run the corpus is restored from cache
    # and already contains everything the seeds hold plus what the fuzzer has
    # since minimised; clobbering it would throw that away.
    cp -n "$seeds"/* "$corpus"/ 2>/dev/null || true
    echo "seeded $target: $(ls -1 "$corpus" | wc -l | tr -d ' ') corpus files"
done
