# Building with the SailfishOS Platform SDK

What a real `mb2` build of `rpm/harbour-vuo.spec` needs, what it found, and
where it currently stops. Everything here was executed, not inferred; the
recipe follows [muhnschein/postivene](https://github.com/muhnschein/postivene)'s
`docs/SDK-BUILD.md`, which solved the same problem for the same SDK first.

## Result, in one line

**The device build does not currently complete**, and the reason is a real
defect rather than a missing step: Vuo's dependency graph needs Rust 1.88,
while the SDK 5.0.0.43 tooling ships 1.75. See *The blocker* below. Everything
up to the Rust version — image, chroot, sb2 targets, the reconstructed
standard library, and the spec's own `%build` — works and is checked in.

## Environment, without a Docker daemon

There is no dockerd here, so the image is pulled from the registry API and
unpacked as a chroot. Streaming each layer straight into the rootfs matters:
the image is ~4.6 GB compressed and ~13 GB unpacked, and writing the tarballs
first needs headroom this box does not have.

```sh
REG=https://mirror.gcr.io/v2/coderus/sailfishos-platform-sdk
curl -sS -H 'Accept: application/vnd.docker.distribution.manifest.v2+json' \
     "$REG/manifests/5.0.0.43"                      # 12 layers
curl -sSL "$REG/blobs/sha256:<digest>" | tar -xz -C rootfs   # per layer
```

`-L` is load-bearing: blob URLs redirect to a storage CDN and without it you
silently save a 140-byte redirect body.

Then bind-mount `/proc`, `/sys`, `/dev`, `/dev/pts` and enter with
`chroot --userspec=mersdk:mersdk`, with `HOME=/home/mersdk` — the sb2 targets
are registered under that user, and as root sb2 only says
"Invalid target specified".

The image also ships the target/tooling install tarballs at the rootfs root
(`tooling-i486.tar`, `target-*.tar`, ~4.2 GB). The targets are already
extracted under `/srv/mer`, so those are safe to delete if space is tight.

## Reconstructing Rust in the target

The stock 5.0.0.43 targets ship **no rust at all**. Normally `mb2` zypper-installs
`rust`, `cargo` and `rust-std-static-<triple>` from the Jolla repos on first
build; those are unreachable here. Three pieces are needed, and the third is
easy to miss:

1. **Host (i686) std**, copied from the tooling, into the target *and* its
   `.default` snapshot — mb2 builds against the snapshot.
2. **aarch64 std**, built from source *with the tooling's own rustc*. Do not
   substitute rustup's std for the same version: Jolla's compiler reports
   `1.75.0-nightly (82e1608df)` where upstream stable reports `1.75.0
   (82e1608df)` — same commit, different release string — and rustc rejects the
   rlibs with `E0514: found crate compiled by an incompatible version of rustc`.
   Unpack `rust-src-1.75.0` into `<tooling>/usr/lib/rustlib/src/rust`, then in a
   dummy crate, with `RUSTC_BOOTSTRAP=1`:

   ```sh
   cargo build --release -Zbuild-std=std,panic_unwind \
       --target aarch64-unknown-linux-gnu
   ```

   The dummy crate's own final link fails — it uses the tooling's i686 linker —
   and that is fine. The 21 `lib*.rlib` in
   `target/aarch64-unknown-linux-gnu/release/deps/` are what you install into
   the target's `usr/lib/rustlib/aarch64-unknown-linux-gnu/lib/`.
3. **The i686 rustlib at `/usr/lib/rustlib` in the SDK chroot itself**, not
   only in the target. Build-script links run in sb2's *host* mode, where
   `/usr` maps to the SDK filesystem.

std's own dependencies come from crates.io, which the chroot cannot reach here.
Vendor them on the host (`cargo vendor` from `library/sysroot`, which is the
workspace `-Zbuild-std` uses) and point a `.cargo/config.toml` at the copy.

Sanity check, which passes:

```
$ sb2 -t SailfishOS-5.0.0.43-aarch64 rustc --target aarch64-unknown-linux-gnu hello.rs
$ file hello
hello: ELF 64-bit LSB pie executable, ARM aarch64, ... dynamically linked,
interpreter /lib/ld-linux-aarch64.so.1
```

## What the build found in the spec

Four defects, each of which only a real `mb2` run could surface. All are fixed
in `rpm/harbour-vuo.spec`.

| | Was | Why it fails |
| --- | --- | --- |
| Package selection | `--bin harbour-vuo` alone | `--features` resolves against the *selected packages*, and `default-members` is just `vuo-core`, which has no `sailfishapp` feature. The spec as written could never have built the device binary: `none of the selected packages contains these features`. |
| Parallelism | `--jobs %{_smp_build_ncpus}` | Parallel cargo deadlocks under sb2 — it futex-waits forever on an unreaped child while qmetaobject's C++ glue compiles. `-j1` when `SBOX_SESSION_DIR` is set. |
| Build-script linking | nothing | rustc links build scripts by calling plain `cc`, which sb2 rewrites to the *cross* compiler: `unrecognized command-line option '-m32'`. sb2 exposes the native one as `host-gcc`; point the host triple's linker at it. An absolute path to the tooling's gcc is *not* enough — sb2 still rewrites the `ld` gcc invokes. |
| Qt discovery | `export QMAKE=/usr/bin/qmake` | qttypes shells out to `qmake -query`, which a build script under sb2 cannot reliably exec. Setting `QT_INCLUDE_PATH` **and** `QT_LIBRARY_PATH` makes it skip qmake and read `qtcoreversion.h` instead. `QT_LIBRARY_PATH` must be `%{_libdir}` — Qt is in `/usr/lib64` on aarch64, not `/usr/lib`. |

## The blocker

With those fixed, the build reaches cargo and stops there:

```
failed to parse manifest at `.../vendor/idna_adapter/Cargo.toml`
Caused by: feature `edition2024` is required
  ... not stabilized in this version of Cargo (1.75.0-nightly)
```

That is not one unlucky crate. Measured against the committed lockfile:

- **19 locked dependencies use edition2024**, which cargo 1.75 cannot parse.
  Vendoring — how OBS and SDK builds get their crates — parses *every* vendored
  manifest, so these break the build even though nothing compiles them.
- The graph's true floor is **1.88**, via `url` → `idna` → the `icu_*` crates.
  `Cargo.toml` said `rust-version = "1.75"`; that was fiction.

### Why CI never noticed

`.github/workflows/ci.yml`'s "SailfishOS Rust floor" job installed the MSRV
toolchain and then ran a **bare `cargo check`**. `rust-toolchain.toml` pins
`channel = "stable"`, and a rustup toolchain *file* overrides the toolchain a
workflow installs — so the job ran on stable every time and passed. Reproduced
directly:

```
$ rustup default 1.75 && cargo --version
cargo 1.94.1        # in this repo, because of rust-toolchain.toml
```

The job now runs `cargo +${{ steps.msrv.outputs.version }}`, and
`rust-version` is the measured 1.88.

### Resolving it

Two options, and the choice is a real trade rather than a cleanup:

1. **A newer Rust in the build target.** `mb2` normally installs Jolla's
   `rust`/`cargo` packages, which may be newer than the tooling's 1.75. This
   environment cannot reach those repos, so it is unverified — but it is the
   only option that does not move the code backwards.
2. **Roll the dependency tree back** to something cargo 1.75 can parse. This
   was attempted and abandoned deliberately: it cascades through
   `reqwest` → `tower-http` → `async-compression` → `compression-codecs`, and
   drags the TLS stack back with it. For an app whose whole job is talking TLS
   to the user's server, and whose §9.1 is explicit about it, trading current
   rustls for an SDK version number is the wrong way round.

`make lockfile` reports the gap on every run so it stays visible.

## Not yet done

- `armv7hl` (same recipe as aarch64 — build std for
  `armv7-unknown-linux-gnueabihf`).
- `BuildRequires` resolution against zypper, which this environment cannot reach.
- Anything on a real device or emulator.
