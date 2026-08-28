# Packaging and the toolchain

## Building a device RPM

```sh
scripts/build-rpm.sh aarch64     # or armv7hl, or i486 for the emulator
```

Needs the SailfishOS SDK. Two things that will otherwise waste an afternoon:

- **Rust requires the SDK's Docker build engine.** The VirtualBox engine cannot
  build it.
- **The first build is slow.** Expect tens of minutes.

## Why the spec passes `--bin harbour-vuo`

The workspace's `default-members` are the Qt-free set, so that `cargo build` on
a CI runner without Qt headers does the right thing. The consequence is that a
bare `cargo build --release` builds **nothing installable**, so the spec names
the binary explicitly. `scripts/check-packaging.sh` asserts it still does.

## OBS builds have no network

Hence vendoring:

```sh
make vendor              # writes rpm/vendor.tar.xz and rpm/vendor.toml
sfdk build -- --with vendor
```

Both outputs are gitignored — they are build products, and committing several
hundred megabytes of third-party source would make the repository unusable.
`cargo vendor --locked` so the bundle matches `Cargo.lock` exactly; a bundle
that resolved differently would make the OBS build diverge from every other
build, which is the one thing vendoring exists to prevent.

The spec forces `--with vendor` automatically under OBS and Chum, because
neither can pass `--with` on the command line.

## The Rust floor

SailfishOS ships an older Rust than current stable. Two separate pins, for two
separate purposes:

| File | What it pins | Why |
| --- | --- | --- |
| `Cargo.toml` `rust-version` | the **MSRV** (1.75) | the device build uses the SDK's toolchain; `make msrv` and a CI job re-check against it so a dependency bump cannot silently break it |
| `rust-toolchain.toml` | the **development** toolchain (stable) | so rustfmt and clippy behave identically everywhere; a clippy version drift turns `-D warnings` into a lottery |

`make msrv` installs the floor toolchain if absent and runs `cargo check
--locked` against it. `--locked` matters: the committed lockfile is what OBS
builds offline, so the MSRV check has to apply to those exact versions.

## Cross-compilation

The spec exports `SB2_RUST_TARGET_TRIPLE` because Scratchbox2 accelerates
`rustc` by running it as x86, and that variable is how it learns what the real
target is. It also sets the per-target linker, `CC`, `CXX` and `AR`, and
`QMAKE=/usr/bin/qmake` because `qttypes`'s build script probes for `qmake6`
first and errors out when it is absent.

`Qt5Widgets` is in `BuildRequires` even though Vuo never instantiates a
`QApplication`: `qttypes` emits `cargo:rustc-link-lib=Qt5Widgets`
unconditionally, and without the package the link fails late and confusingly.

## Distribution

Chum and OpenRepos. Harbour is a stretch goal at best and is not designed
around: its library restrictions and its rules about background services are
both awkward for this app. The `%bcond_with harbour` in the spec exists so a
Harbour-shaped build can drop the systemd timer subpackage, not because Harbour
is a target.

## Background sync

A systemd **user** timer running the same binary with `--sync-once`, rather
than an in-process timer.

The reason is that SailfishOS suspends applications aggressively, and a
suspended app's timer does not fire. The timer unit uses
`RandomizedDelaySec=5min` so that every device does not wake at the same
instant and hit the user's server together, and `Persistent=true` so a run
missed while the phone was suspended happens on wake rather than being skipped.

Exit code 75 (`EX_TEMPFAIL`) means "no signal, try later", so the unit treats
the ordinary case of a phone without connectivity as success rather than as a
fault worth restarting and logging about.
