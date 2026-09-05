# Packaging and the toolchain

## Building a device RPM

**Unattended, on GitHub:** `.github/workflows/rpm.yml` builds one against the
Sailfish SDK image and uploads it as a workflow artifact. Dispatch it from the
Actions tab (the SDK version is an input) or push a `v*` or `build-*` tag; it
also runs on any pull request that changes the recipe. The skeleton follows
muhnschein/postivene's `rpm.yml`, but not its `mb2` build: Jolla's
repositories install `rust`/`cargo` 1.75.0+git2 into the target (measured
against the 5.2.0.15 SDK), and this lockfile needs 1.88. So the job lifts the
SDK's cross compiler and target sysroot out of the image and runs
`scripts/cross-rpm.sh` with the host's cargo -- the route in
[`sdk-build.md`](sdk-build.md). The output is a test package: the binary
links against the chosen SDK version's glibc (2.39 for 5.2.0.15), so it runs
on phones at that release or newer.

**Locally, with the SDK installed:**

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

## The sandbox

The desktop entry carries an `[X-Sailjail]` section, so the app runs under
Sailjail with exactly the permissions it uses: `Internet` for the Miniflux
server, `WebView` for the Gecko view on the site page. Sailjail lets the app
write `$HOME/.local/share/<OrganizationName>/<ApplicationName>` and nothing
else of the home directory; both names are `harbour-vuo`, so everything Vuo
keeps lives in `~/.local/share/harbour-vuo/harbour-vuo/`. An install from
before the sandbox kept its files one level up. The first start after the
update moves them there (`AppPaths::adopt_legacy_files`), the mirror through
SQLite's backup API so a write-ahead log comes across too, and removes the
originals only once every copy is in place. The old directory is still visible
inside the sandbox while it exists, which is what makes the move possible from
within.

## Background sync

A systemd **user** timer running the same binary with `--sync-once`, rather
than an in-process timer.

The reason is that SailfishOS suspends applications aggressively, and a
suspended app's timer does not fire. The timer unit uses `RandomizedDelaySec`
so that every device does not wake at the same instant and hit the user's
server together, and `Persistent=true` so a run missed while the phone was
suspended happens on wake rather than being skipped.

The timer fires every 15 minutes -- the finest interval Settings offers --
whatever the user chose, and the interval is applied by the process it
starts: `--sync-once` reads the chosen interval from the account file and the
time of the last sync from a stamp beside the mirror (written by the app and
by the timer's process alike), and exits at once when the interval has not
passed or the choice is "Manual only". The interval used to be applied by a
systemd drop-in written from the Settings page; the sandbox does not let the
app write `~/.config/systemd` or reload systemd, and the timer's process,
which runs outside the sandbox, is the one place the choice can take effect.

Exit code 75 (`EX_TEMPFAIL`) means "no signal, try later", so the unit treats
the ordinary case of a phone without connectivity as success rather than as a
fault worth restarting and logging about.
