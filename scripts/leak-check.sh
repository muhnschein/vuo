#!/bin/sh
# Every workspace test under AddressSanitizer with leak detection.
#
# Finds what normal suite can't: memory lost by process exit, plus out-of-bounds
# reads, use-after-free, double free. Not just Rust: compiled C/C++ instrumented
# too (SQLite via rusqlite `bundled`, ring C/asm glue, qmetaobject cpp! glue
# where QObject/QVariant handed to Qt would leak).
#
# Timing-asserting tests may fail only here: ASan 2-3x slower. Log tells apart:
# leak ends in LeakSanitizer report with allocation stack.
#
# Can't find: memory reachable at exit (ever-growing cache), anything in
# non-rebuilt library (Qt, glib). Intentional Qt leaks suppressed by name in
# scripts/lsan.supp; read reasons before adding.
#
# Nightly only (`-Zsanitizer`, `-Zbuild-std` so std instrumented, agrees on
# sanitizer). Nightly pinned: weekly run fails on our code, not nightly churn;
# move on purpose, like rust-toolchain.toml.
#
# scheduled.yml runs weekly. Rebuilds every dependency with ASan; far slower
# than `make test`, not in `make check`. Locally:
#   rustup toolchain install nightly-2026-10-12 --profile minimal -c rust-src
#   apt install llvm    (for llvm-symbolizer)
#   scripts/leak-check.sh            (or: make leak-check)
# Arguments go to `cargo test`; run one test binary:
#   scripts/leak-check.sh -p vuo-shim --test qml_loads
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
toolchain=${LEAK_CHECK_TOOLCHAIN:-nightly-2026-10-12}
# Spelled out: build scripts and proc macros (host-built) stay uninstrumented;
# everything in test binary instrumented.
target=x86_64-unknown-linux-gnu

# scripts/lsan.supp names Qt leaks by function; LeakSanitizer gets names only
# via llvm-symbolizer. Without it frames bare addresses, no suppression matches,
# every Qt test "leaks" (piirit's first weekly run failed so).
if ! command -v llvm-symbolizer >/dev/null 2>&1; then
    echo "leak-check: FAIL llvm-symbolizer is not on PATH (apt install llvm)" >&2
    exit 1
fi

# C/C++ compilers with sanitizer. Wrappers, not CFLAGS: nothing a build script
# appends or resets can drop it.
wrappers=$(mktemp -d)
trap 'rm -rf "$wrappers"' EXIT
for pair in cc:gcc c++:g++; do
    name=${pair%%:*}
    compiler=${pair#*:}
    printf '#!/bin/sh\nexec %s -fsanitize=address -fno-omit-frame-pointer "$@"\n' \
        "$compiler" >"$wrappers/$name"
    chmod +x "$wrappers/$name"
done
# Plain CC/CXX, not cc crate's CC_<triple>: host = target here, same compiles.
export CC="$wrappers/cc"
export CXX="$wrappers/c++"

# --cap-lints=warn: workspace denies warnings; nightly-only lint is
# scheduled.yml next-stable-clippy's to report, not reason to skip leak check.
export RUSTFLAGS="-Zsanitizer=address --cap-lints=warn"
export CARGO_UNSTABLE_BUILD_STD=std
# Own dir: nothing here reusable by normal build.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target/leak-check}"
# Enough for file:line in report, fraction of full debuginfo disk.
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
export CARGO_INCREMENTAL=0
export QT_QPA_PLATFORM=offscreen

export ASAN_OPTIONS=detect_leaks=1
export LSAN_OPTIONS="suppressions=$root/scripts/lsan.supp:print_suppressions=0"

cd "$root"
rustc "+$toolchain" --version
# Whole workspace unless told: cargo takes `--workspace` over `-p`, can't pass
# both. harbour-vuo excluded: no tests, device feature needs SDK.
#
# One test skipped by default: shim memory test measures glibc malloc returning
# pages via malloc_trim; under ASan sanitizer allocator answers everything, trim
# returns nothing, test can only fail.
if [ $# -eq 0 ]; then
    set -- --workspace --exclude harbour-vuo \
        -- --skip memory::tests::a_freed_peak_is_returned_to_the_operating_system
fi
# --tests: unit + integration, not doctests (rustdoc builds without target's
# sanitizer runtime; doctest leaks nothing tests wouldn't show). --no-fail-fast:
# one failing binary mustn't hide leaks in rest.
exec cargo "+$toolchain" test --locked --no-fail-fast --target "$target" --tests "$@"
