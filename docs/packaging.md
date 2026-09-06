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

Harbour. Chum and OpenRepos take the same package.

## Harbour readiness

Harbour runs `rpmvalidation.sh` from
[`sailfishos/sdk-harbour-rpmvalidator`](https://github.com/sailfishos/sdk-harbour-rpmvalidator)
over the submitted RPM. Most of what it checks is decided in this repository
rather than by the compiler, so `scripts/check-harbour.sh` holds those rules on
every `make check`: the paths the specs install to, the `[X-Sailjail]` keys,
permissions and names, the QML modules the pages import, the four icon sizes,
and the RPM constructs (scriptlets, triggers, `Obsoletes`, `%license`) that
intake rejects. Its rules are **transcribed** from the validator's own
configuration -- `make check` has no network -- so re-read them against
upstream when a submission is being prepared.

Two rules cannot be checked there, because both are decided by the device link:

- **The shared libraries the binary needs.** `scripts/cross-build.sh` reads
  them off the cross-built ELF and reports any that Harbour's
  `allowed_libraries.conf` does not list. It warns rather than fails, because
  that script's output is the test package people install on a phone.
- **The glibc symbol versions.** The same script prints the highest ones
  required; the validator wants `__libc_start_main@GLIBC_2.34`, which means
  building against a current SDK target.

### Known blockers

- **`libQt5Widgets.so.5` is linked, and Harbour does not allow it.** `qttypes`
  emits `-lQt5Widgets` unconditionally (its `build.rs`), and `qmetaobject`'s
  `QmlEngine` is a `QApplication`, whose constructor and `exec` stay as
  undefined references in the C++ glue even though the device entry point uses
  `SailfishApp::application()` instead. Confirmed on a host build; the device
  link is the same shape. `--as-needed` does not help, because the references
  are real. Resolving it means removing them -- garbage-collecting the unused
  `QmlEngineHolder` at link time, or carrying a patch to `qmetaobject` -- and
  proving the result on a device.
- **No release package can be built yet.** `rpm/harbour-vuo.spec` cannot run
  under the SDK's own cargo (see "The Rust floor" and `docs/sdk-build.md`), and
  what CI produces is a *test* package: cross-built outside `sb2`, unstripped,
  with `AutoReqProv: no`. A submission has to come from the spec.

The two rules that shaped the app itself are met: one process, with no
background service, and a sandbox declared in the desktop entry.

## Generated files that are committed

Two things in the tree are produced by a tool and tracked anyway, so that a
build needs neither of the tools:

| File | Made by | Needs |
| --- | --- | --- |
| `translations/*.qm` | `lrelease` | Qt's linguist tools |
| `qml/art/*.png` | `make textart` | a QML runtime and a display (or `xvfb`) |

The art is the texture the cover and the onboarding page wear: nested curves
of tiny filler text, after Jolla's own packaging. It used to be painted at
runtime, and a device reported the onboarding page freezing for fourteen
seconds while it was; painting it ahead of time costs nothing to show, looks
the same on every phone, and cannot half-arrive. `tools/textart/` holds the
painter and is **not** installed.

What ships is a **coverage mask** -- one grayscale channel, no colour -- which
`qml/components/TextArt.qml` tints with the theme's own colour. One file is
therefore right on every ambience, a light one included, and there is nothing
to regenerate when Sailfish gains another. The same shader dims it and cuts
the two holes the app needs: the band the cover's heading sits in, and the
disc the onboarding page's title sits in. Both are geometry the app knows and
the painter does not, so neither is baked in.

One caveat, stated because it cannot be fixed here: **the masks are not
rendered in the device's own font.** Sail Sans Pro ships with SailfishOS and
is not redistributable, so whichever machine runs `make textart` renders them
with its default sans instead. At these sizes the letters are texture rather
than reading matter and the pattern is identical, but it is the one respect
in which the shipped art is not what the device would have drawn for itself.

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
