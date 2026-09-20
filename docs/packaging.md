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

`cargo vendor` does *not* put `qmetaobject` in the bundle, and that is correct:
`[patch.crates-io]` resolves it from `third_party/qmetaobject`, which is in the
source tarball already. Only its proc-macro half, `qmetaobject_impl`, is
fetched. See "QtWidgets, and the vendored qmetaobject" below.

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

Two rules are decided by the link rather than by the tree:

- **The shared libraries the binary needs.** `scripts/check-linked-libs.sh`
  holds the allowed list and is run over both links: the host build, by
  `make check`, and the cross build, by `scripts/cross-build.sh`. The host
  build is the stricter of the two -- it is the one that actually constructs
  `qmetaobject`'s application object, where the device entry point uses
  SailfishApp's instead -- so a regression is caught without a phone. The cross
  build reports rather than stops, and the `rpm` workflow fails the job *after*
  uploading the package: a package that breaks this rule still installs and
  runs, and is exactly the one you want in your hands while working out why.
- **The glibc symbol versions.** `scripts/cross-build.sh` prints the highest
  ones required; the validator wants `__libc_start_main@GLIBC_2.34`, which
  means building against a current SDK target. Measured on 5.2.0.15: `GLIBC_2.34`.

### QtWidgets, and the vendored qmetaobject

`qmetaobject` builds its QML engine on `QApplication`, which comes from
QtWidgets -- and `libQt5Widgets.so.5` is not on Harbour's list, since a Silica
app is expected to use QtGui's `QGuiApplication`. Upstream carries that
unconditionally, on the released crate and on master, with no feature to turn
it off. It was the one `NEEDED` entry the aarch64 link produced that intake
would have refused.

Nothing here needs QtWidgets. Vuo's device entry point never constructs a
`QmlEngine` at all -- it uses `SailfishApp::application()`, which returns a
`QGuiApplication` -- but the reference survives anyway, because `cpp!` compiles
a crate's C++ into one object and the linker takes all of it or none. So
`third_party/qmetaobject` is upstream 0.2.10 plus
`third_party/qmetaobject.patch`: three lines, swapping the include, the member
type and the constructor.

`qttypes` separately passes `-lQt5Widgets` unconditionally, which would record
the dependency even with nothing using it. Rather than fork a second crate for
one line, `crates/harbour-vuo/build.rs` links the binary with `--as-needed`,
which drops any library no symbol refers to. That works only *because* the
patch removed the last reference; with `QApplication` still in use the library
is genuinely needed and `--as-needed` keeps it. Which is why `make check`
links and inspects the host binary rather than trusting the flag.

Carrying someone else's crate in-tree is only safe while the difference is
visible, so `make vendor-check` (`scripts/check-vendored.sh`) fetches the
crates.io tarball, applies the patch, and requires the result to match the
vendored tree byte for byte. It needs the network, so it is an opt-in gate
rather than part of `make check`; CI runs it on every push. The crate's own
`tests/` are not vendored -- cargo never builds a dependency's tests, and
leaving them in gives the security scanners a thousand lines to report on that
this repository does not compile.

One consequence worth knowing: a path dependency is not lint-capped the way a
fetched crate is, so `RUSTFLAGS: -D warnings` in CI turned the vendored crate's
own forty-six warnings into errors. Warnings are denied by the lint tables in
`Cargo.toml` instead, which apply to the crates that opt into them. The
vendored crate still prints its warnings on a clean build; they are upstream's.

The approach, the patch, the vendor check and this section all come from
[postivene](https://github.com/muhnschein/postivene), which hit the same rule
first. The real fix is upstream: a feature flag choosing between `QApplication`
and `QGuiApplication` would serve every Sailfish app built on `qmetaobject`.

### The submission package

Harbour takes a package you built: nothing at Jolla rebuilds it from source,
and `rpmvalidation.sh` judges the RPM's contents rather than its provenance.
So the SDK gap in `docs/sdk-build.md` — `rpm/harbour-vuo.spec` cannot run under
the SDK's own cargo — is not on the path to the store. It costs Chum and OBS,
which do build from the spec, and it makes the recipe ours to maintain.

`.github/workflows/rpm.yml` builds the package that gets submitted. A **release
build** (a `v*` tag, or the dispatch form's `release` box) differs from a test
build in one field: `Release: 1` rather than `1.<run number>`, so the file is
`harbour-vuo-VERSION-1.aarch64.rpm` as Harbour's naming rule wants. The same
job then checks that name, and the binary's linked libraries, over the result.

Two deliberate departures from what an SDK build would produce:

- **`AutoReqProv: no`.** Ubuntu's rpm generates soname `Requires` the phone's
  rpmdb does not recognise, and the install then fails on dependencies that are
  present — a worse failure than having none. The package declares its
  dependencies at PACKAGE level instead (`sailfishsilica-qt5`,
  `sailfish-components-webview-qt5`), which is what Harbour's own allowed list
  is written in terms of, and every library the binary links comes with those
  two. `rpm/harbour-vuo.spec` leaves the generator on, which is right for the
  rpm that gets it right.
- **Hardening restated by hand.** Inside `sb2` the distro's `%optflags` reach
  every C/C++ compile through rpm's build environment; this route does not go
  through rpm at all, so `scripts/cross-build.sh` passes `-O2
  -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIC` and links `-z relro -z
  now` itself, then reads RELRO, BIND_NOW and PIE back off the ELF so a flag
  that stops being applied is visible rather than silent.

Everything else measured on the 5.2.0.15 aarch64 build passes: every linked
library is allowed, the glibc floor is right, the binary is stripped and has no
`rpath`, and the packaged tree is exactly the four locations Harbour allows.
The two rules that shaped the app itself are met as well: one process, with no
background service, and a sandbox declared in the desktop entry.

Only `aarch64` is built. Harbour's form asks for `aarch64, armv7hl`; armv7hl
has never been attempted here, and the store simply will not offer the app to
a device of an architecture it has no package for.

## Generated files that are committed

Two things in the tree are produced by a tool and tracked anyway, so that a
build needs neither of the tools:

| File | Made by | Needs |
| --- | --- | --- |
| `translations/*.qm` | `lrelease` | Qt's linguist tools |
| `qml/art/onboarding.png` | `make textart` (`scripts/render-textart.sh onboarding`) | a QML runtime and a display (or `xvfb`) |
| `qml/art/cover/*.png` | `make textart` (`scripts/render-textart.sh cover`) | the same, plus the digits' font; the device's own font if you have it |
| `store/cover.png` | `scripts/render-store-cover.sh` | the same, plus two font files |

The art is the texture the cover and the onboarding page wear: nested curves
of tiny filler text, after Jolla's own packaging. It used to be painted at
runtime, and a device reported the onboarding page freezing for fourteen
seconds while it was; painting it ahead of time costs nothing to show, looks
the same on every phone, and cannot half-arrive. `tools/textart/` holds the
painter and is **not** installed.

What ships is a **coverage mask** -- one grayscale channel, no colour -- which
`qml/components/TextArt.qml` tints with the theme's own colour. One file is
therefore right on every ambience, a light one included, and there is nothing
to regenerate when Sailfish gains another. The same shader dims it and can cut
two holes in it: a band at the top for a heading, and a disc for the
onboarding page's title. Both are geometry the app knows and the painter does
not, so neither is baked in.

### The cover's set

On the cover the unread count is **negative space**: the lines of text flow
around the digits, hug their outline, and a few lines out are the usual
sweeps again. Nothing is drawn inside the digits and nothing is drawn on top
of the texture. That means the whole background depends on the number, so
the cover is not one mask but a set, `qml/art/cover/0.png` … `99.png` and
`99+.png` -- every count from 0 to 99 and one for everything past that. The
cover names the one for its count (`CoverPage.qml`, `countKey`) and the
count itself is still there as data, in an invisible `unreadTotal` label.

Three ways of packaging that were weighed; this is the first:

| | Masks | Installed | Costs on the phone |
| --- | --- | --- | --- |
| **0–99 plus "99+"** (chosen) | 101 | about 6.5 MB | nothing: one image load, from Qt's pixmap cache between covers |
| 0–999 plus "999+" | 1001 | about 70 MB | nothing, but too large for a Harbour package |
| paint on the device, cache per count | 0 | a cache | a second or so of a core every time the count changes, memory for the field and the canvas, and the device-side painting this art was retired from |

"Art is shipped, never drawn" is the rule the first keeps, and the megabytes
are the whole of its price. Two things keep them few: the master is
512×768, the smallest 2:3 size that still only scales *down* on a cover at
pixel ratio 2 (a cover is 234×374 at ratio 1; 480×720 was tried and looked
grainy on a phone), and `scripts/png-mask.py` quantises coverage to sixteen
levels, which is indistinguishable from the full range at well under half
the size. Eight levels were tried and are grainy, because the halo below
leaves the far lines few levels to be antialiased in; JPEG was measured and
is two to three times larger at any quality that does not ring. The cap is
99, not the 999 the old label had: the digits are set large enough to read
from across a room, and three of them do not fit.

The masks also carry a **halo**: the lines are painted at full strength
where they touch the digits and ease down to 0.55 / 1.5 of it six line
spacings out. The cover draws its masks at an ink of 1.5, so the far lines
land at the 0.55 everything else is drawn at, and the lines on the digits
are driven past full: the shader's output saturates there, which makes the
thin glyphs bolder and brighter than a mask alone could, and the number is
the brightest thing on the cover.

The digits are Fira Sans **ExtraBold** (SIL OFL), which `render-textart.sh`
fetches over TLS into the gitignored `.fonts/` as the store cover's script
does its two, with `VUO_FONT_DIR` to point it elsewhere. Black was tried and
rejected: the counter of the 4 closes up at that weight and the digit reads
as a solid shape. The digits are only a silhouette, so the difference between
this face and the device's own does not show.

The placement rests on one number that should be **checked on a device**:
`tools/textart/render.qml` takes the cover-action strip to be
`Theme.itemSizeSmall` (80 at ratio 1) of a 374-tall cover, about 0.214 of
its height, and centres the digits' ink in the height above it. Both scale
with the pixel ratio, so the fraction should hold everywhere; a screenshot of
a stock cover with one action, measured, is what would confirm it.

`scripts/render-textart.sh cover:42` paints one count, for looking at a
change to the painter before running it over all hundred and one; a full
`cover` run replaces the directory. The painter refuses any master on which
two lines of text run over one another, so the whole set is checked as it is
made.

`store/` is the Harbour Store page's own assets and is **not installed** --
nothing in either spec touches it. `store/cover.png` is the 1080x540 banner at
the top of Vuo's Store page: the onboarding screen laid out for a landscape
frame, from the same painter with strokes read for that shape, so the Store
page and the app look like one thing. Its wordmark is Fira Sans (SIL OFL); the
script fetches the two files it needs and `.gitignore` keeps them out of the
tree, since the rendered PNG is what is tracked.

One caveat about the filler's face. Sail Sans Pro ships with SailfishOS and
is not redistributable, so the script never fetches it and it is never
committed. Put a copy of `/usr/share/fonts/sail-sans-pro/SailSansPro-Light.ttf`
from a phone into the font directory by hand and `render-textart.sh` sets
the filler in it, which is how the cover's set was made; without it, the
host renders whatever its fontconfig calls "Sail Sans Pro", in practice its
default sans, which is how `onboarding.png` was made. At these sizes the
letters are texture rather than reading matter and the pattern is the same
either way. It does mean two machines need not paint byte-identical masks,
so regenerate a set only when the painter or its inputs change, and
regenerate the whole set when you do.

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
