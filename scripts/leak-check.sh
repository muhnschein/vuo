#!/bin/sh
# Every test in the workspace, under AddressSanitizer with leak detection.
#
# What it finds that the normal suite cannot: memory a test's process
# allocated and lost track of by the time it exits, and the out-of-bounds
# reads, use-after-frees and double frees ASan stops on. Not only in our
# Rust -- the C and C++ the build compiles are instrumented too: SQLite
# (rusqlite's `bundled`), ring's C and assembly glue, and qmetaobject's
# cpp! glue, which is where a QObject or a QVariant handed across to Qt
# would be lost.
#
# A test that asserts on timing can fail here and nowhere else: ASan makes
# everything two to three times slower. The log tells the two apart -- a
# leak ends in a LeakSanitizer report with the allocation's stack.
#
# What it cannot find: memory still reachable at exit, such as a cache that
# only grows, and anything in a library that was not rebuilt (Qt, glib).
# Leaks Qt makes on purpose are suppressed by name in scripts/lsan.supp;
# read the reasons there before adding to it.
#
# Nightly only (`-Zsanitizer`, and `-Zbuild-std` so std is instrumented and
# agrees with everything else about the sanitizer). The nightly is pinned so
# a weekly run fails on our code, not on whatever nightly did that week;
# move it on purpose, like rust-toolchain.toml.
#
# scheduled.yml runs it weekly. It rebuilds every dependency with ASan, so it
# is far slower than `make test` and is not part of `make check`. Locally:
#   rustup toolchain install nightly-2026-10-12 --profile minimal -c rust-src
#   apt install llvm    (for llvm-symbolizer)
#   scripts/leak-check.sh            (or: make leak-check)
# Arguments go to `cargo test`, so one test binary can be run on its own:
#   scripts/leak-check.sh -p vuo-shim --test qml_loads
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
toolchain=${LEAK_CHECK_TOOLCHAIN:-nightly-2026-10-12}
# Spelled out, so build scripts and proc macros (built for the host) stay
# uninstrumented, and everything that ends up in a test binary is.
target=x86_64-unknown-linux-gnu

# scripts/lsan.supp names Qt's own leaks by function, and LeakSanitizer reads
# a function's name only through llvm-symbolizer. Without one every frame is
# a bare address, no suppression matches, and every Qt test "leaks" -- which
# is how piirit's first weekly run of this same check failed.
if ! command -v llvm-symbolizer >/dev/null 2>&1; then
    echo "leak-check: FAIL llvm-symbolizer is not on PATH (apt install llvm)" >&2
    exit 1
fi

# The C and C++ compilers, with the sanitizer. Wrappers rather than flags in
# CFLAGS, so that nothing a build script appends or resets can drop it.
wrappers=$(mktemp -d)
trap 'rm -rf "$wrappers"' EXIT
for pair in cc:gcc c++:g++; do
    name=${pair%%:*}
    compiler=${pair#*:}
    printf '#!/bin/sh\nexec %s -fsanitize=address -fno-omit-frame-pointer "$@"\n' \
        "$compiler" >"$wrappers/$name"
    chmod +x "$wrappers/$name"
done
# Plain CC and CXX rather than the cc crate's per-target CC_<triple>: the
# host is the target here, so those would reach exactly the same compiles.
export CC="$wrappers/cc"
export CXX="$wrappers/c++"

# --cap-lints=warn: the workspace denies warnings, and a lint this nightly
# has that the pinned stable does not is scheduled.yml's next-stable-clippy
# job's to report, not a reason to skip the leak check.
export RUSTFLAGS="-Zsanitizer=address --cap-lints=warn"
export CARGO_UNSTABLE_BUILD_STD=std
# Its own directory: nothing built here can be reused by a normal build.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target/leak-check}"
# Enough for file:line in a report, at a fraction of full debuginfo's disk.
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
export CARGO_INCREMENTAL=0
export QT_QPA_PLATFORM=offscreen

export ASAN_OPTIONS=detect_leaks=1
export LSAN_OPTIONS="suppressions=$root/scripts/lsan.supp:print_suppressions=0"

cd "$root"
rustc "+$toolchain" --version
# The whole workspace unless told which part: cargo takes `--workspace`
# over `-p`, so the two cannot both be passed. harbour-vuo has no tests, and
# its device feature needs the SDK, so it is left out either way.
#
# One test is skipped by default: the shim's memory test measures glibc's
# malloc giving pages back through malloc_trim, and under ASan nothing is
# glibc's malloc -- the sanitizer's allocator answers every allocation, so
# the trim has nothing to return and the test can only fail.
if [ $# -eq 0 ]; then
    set -- --workspace --exclude harbour-vuo \
        -- --skip memory::tests::a_freed_peak_is_returned_to_the_operating_system
fi
# --tests: the unit and integration tests, not the doctests, which rustdoc
# builds without the target's sanitizer runtime; a doctest has nothing to
# leak that the tests beside it would not show.
# --no-fail-fast: one failing binary should not hide the leaks in the rest.
exec cargo "+$toolchain" test --locked --no-fail-fast --target "$target" --tests "$@"
