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

Harbour. Its two rules that bear on this app are both met: one process, with
no background service, and a sandbox declared in the desktop entry. Chum and
OpenRepos take the same package.

## The sandbox

The desktop entry carries an `[X-Sailjail]` section, so the app runs under
Sailjail with exactly the permissions it uses: `Internet` for the Miniflux
server, `WebView` for the Gecko view on the site page. Sailjail lets the app
write `$HOME/.local/share/<OrganizationName>/<ApplicationName>` and nothing
else of the home directory; both names are `harbour-vuo`, so everything Vuo
keeps lives in `~/.local/share/harbour-vuo/harbour-vuo/`.

## Background sync

In-process, on the worker thread that does every other sync: Vuo is one
process, as Harbour requires, so there is no timer outside it. The worker
waits for a command only until the next sync is due and then runs one as if
the pulley had asked. The interval is the account's `sync_interval_index`,
sent to the worker when the context is built and again whenever Settings is
saved; "Manual only" means never. The first sync of a session is scheduled
from the last one, recorded in a stamp beside the mirror, so an app opened on
a fresh mirror draws its list first and does not sync at once.

This runs while the app is open or minimised to its cover, and not at all
when it is closed. Whether SailfishOS keeps a minimised app's worker thread
running through a long idle is the one thing here that needs a device to
answer.
