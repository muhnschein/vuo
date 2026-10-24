# Vuo SailfishOS RPM spec.
#
# Structure follows Whisperfish (closest Rust+QML Sailfish prior art, §4).
# Non-obvious parts noted inline; approach rationale in docs/packaging.md.

%bcond_with vendor
%bcond_with lto
%bcond_without xz

# OBS and Chum can't pass --with on command line; flags forced here.
%if 0%{?_chum} || 0%{?_obs}
%define with_lto 1
%define with_vendor 1
%endif

%if %{with xz}
# SailfishOS 4.5+ defaults to Zstd payloads; 4.4 and older can't read them.
%define _source_payload w6.xzdio
%define _binary_payload w6.xzdio
%endif

Name:       harbour-vuo
Summary:    Miniflux feed reader for SailfishOS
Version:    1.1.1
Release:    1
License:    GPL-3.0-or-later
Group:      Applications/Internet
URL:        https://github.com/muhnschein/vuo
Source0:    %{name}-%{version}.tar.xz
%if %{with vendor}
# Made by scripts/vendor-crates.sh; not in git.
Source1:    vendor.tar.xz
Source2:    vendor.toml
%endif

Requires:   sailfishsilica-qt5 >= 0.10.9
# Sailfish.WebView, for site page right of article. Ships with OS since 3.1;
# only states dependency.
Requires:   sailfish-components-webview-qt5

BuildRequires:  pkgconfig(sailfishapp) >= 1.0.3
BuildRequires:  pkgconfig(Qt5Core)
BuildRequires:  pkgconfig(Qt5Qml)
BuildRequires:  pkgconfig(Qt5Quick)
# qttypes links Qt5Widgets unconditionally though Vuo never makes QApplication.
# Without it link fails late, confusingly.
BuildRequires:  pkgconfig(Qt5Widgets)
BuildRequires:  rust >= 1.75
BuildRequires:  rust-std-static >= 1.75
BuildRequires:  cargo >= 1.75
BuildRequires:  gcc-c++
BuildRequires:  zlib-devel
BuildRequires:  desktop-file-utils
BuildRequires:  meego-rpm-config
BuildRequires:  qt5-qttools-linguist

%description
A native SailfishOS client for a self-hosted Miniflux instance.

Vuo syncs against Miniflux's own REST API, mirrors entries into a local
database for offline reading, and reconciles changes made offline when
connectivity returns. It does not fetch or parse feeds itself: that is the
server's job. One process, sandboxed, as Harbour requires: the app syncs on
its own while it is open or on the cover, and nothing runs when it is not.

%prep
%setup -q -n %{name}-%{version}

%build
rustc --version
cargo --version

%if %{with vendor}
echo "Setting up an OFFLINE vendored build (OBS has no network -- §7)."
export OFFLINE="--offline --locked"
if [ -d "vendor" ]; then
    echo "Not overwriting existing vendored sources."
else
    tar -xf %SOURCE1
    mkdir -p .cargo/
fi
cp %SOURCE2 .cargo/config.toml
%endif

# Scratchbox2 runs rustc as x86 for speed; this tells it real target.
%ifarch %arm
export SB2_RUST_TARGET_TRIPLE=armv7-unknown-linux-gnueabihf
export CFLAGS_armv7_unknown_linux_gnueabihf="$CFLAGS"
export CXXFLAGS_armv7_unknown_linux_gnueabihf="$CXXFLAGS"
%define targetdir target/armv7-unknown-linux-gnueabihf/release
%endif
%ifarch aarch64
export SB2_RUST_TARGET_TRIPLE=aarch64-unknown-linux-gnu
export CFLAGS_aarch64_unknown_linux_gnu="$CFLAGS -march=armv8-a+crypto+fp+simd"
export CXXFLAGS_aarch64_unknown_linux_gnu="$CXXFLAGS"
%define targetdir target/aarch64-unknown-linux-gnu/release
%endif
%ifarch %ix86
export SB2_RUST_TARGET_TRIPLE=i686-unknown-linux-gnu
export CFLAGS_i686_unknown_linux_gnu="$CFLAGS"
export CXXFLAGS_i686_unknown_linux_gnu="$CXXFLAGS"
%define targetdir target/i686-unknown-linux-gnu/release
%endif

export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=armv7hl-meego-linux-gnueabi-gcc
export CC_armv7_unknown_linux_gnueabihf=armv7hl-meego-linux-gnueabi-gcc
export CXX_armv7_unknown_linux_gnueabihf=armv7hl-meego-linux-gnueabi-g++
export AR_armv7_unknown_linux_gnueabihf=armv7hl-meego-linux-gnueabi-ar
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-meego-linux-gnu-gcc
export CC_aarch64_unknown_linux_gnu=aarch64-meego-linux-gnu-gcc
export CXX_aarch64_unknown_linux_gnu=aarch64-meego-linux-gnu-g++
export AR_aarch64_unknown_linux_gnu=aarch64-meego-linux-gnu-ar

# qttypes build script runs `qmake -query` by default; build script under sb2
# can't reliably exec target qmake. BOTH set = qttypes skips qmake, reads Qt
# version from qtcoreversion.h (checked against crate build.rs). QMAKE alone
# still shells out.
#
# %{_libdir}, not hardcoded /usr/lib: Qt in /usr/lib64 on aarch64 target. Both =
# target-rootfs paths seen inside sb2.
export QT_INCLUDE_PATH=%{_includedir}/qt5
export QT_LIBRARY_PATH=%{_libdir}
export PKG_CONFIG_ALLOW_CROSS_i686_unknown_linux_gnu=1
export PKG_CONFIG_ALLOW_CROSS_armv7_unknown_linux_gnueabihf=1
export PKG_CONFIG_ALLOW_CROSS_aarch64_unknown_linux_gnu=1

%if %{with lto}
export CARGO_PROFILE_RELEASE_LTO=thin
%endif

# Scratchbox bug workaround: /tmp/[...]/symbols.o not found.
export TMPDIR=${TMPDIR:-"$PWD/.tmp"}
mkdir -p $TMPDIR

# Build scripts and proc-macros compiled for tooling arch; rustc links them via
# plain `cc`, which sb2 rewrites to CROSS compiler → "aarch64-meego-linux-gnu-cc:
# error: unrecognized command-line option '-m32'" before first crate. scratchbox2
# exposes native compiler as `host-gcc` (SBOX_HOST_GCC_NAME in target sb2.config)
# for this. Tooling gcc by absolute path NOT enough: sb2 still rewrites `ld` gcc
# calls → "cannot find /lib/libgcc_s.so.1". SBOX_SESSION_DIR set by sb2; outside
# sb2 nothing overridden.
if [ -n "${SBOX_SESSION_DIR:-}" ]; then
    host_triple=$(rustc -vV | sed -n 's/^host: //p')
    export "CARGO_TARGET_$(echo "$host_triple" | tr 'a-z-' 'A-Z_')_LINKER"=host-gcc
fi

# Workspace default-members = Qt-free set; bare `cargo build --release` builds
# nothing installable. Name bin explicitly. Under scratchbox2 PARALLEL cargo
# deadlocks: default -j futex-waits forever on unreaped child while qmetaobject
# C++ glue compiles. sb2 rust builds effectively single-threaded anyway: -j1
# there, cargo's choice on native OBS worker. (Shell, not rpm macro: no "inside
# sb2" macro; both forms = two --jobs, cargo refuses.)
jobs_opt="--jobs %{?_smp_build_ncpus:%{_smp_build_ncpus}}%{!?_smp_build_ncpus:1}"
if [ -n "${SBOX_SESSION_DIR:-}" ]; then
    jobs_opt="-j1"
fi

# `--package`, not only `--bin`: `--features` resolves against SELECTED
# packages; default-members is just vuo-core, no `sailfishapp` feature. Without
# it: "none of the selected packages contains these features: sailfishapp". No
# host check builds this package; only mb2 build catches it.
cargo build $jobs_opt \
    --release \
    --package harbour-vuo \
    --bin harbour-vuo \
    --features sailfishapp \
    $OFFLINE

# NOT -idbased (Sailfish app template's): keys by `qtTrId` ids; Vuo uses `qsTr`
# (context + source). -idbased = every lookup misses, English in every locale.
lrelease translations/*.ts || :

%install
install -D %{targetdir}/harbour-vuo %{buildroot}%{_bindir}/harbour-vuo

desktop-file-install --dir %{buildroot}%{_datadir}/applications harbour-vuo.desktop

for RES in 86x86 108x108 128x128 172x172; do
    install -Dm 644 icons/${RES}/harbour-vuo.png \
        %{buildroot}%{_datadir}/icons/hicolor/${RES}/apps/harbour-vuo.png
done

# `find ./qml` + install -D reproduces ./qml/... under datadir =
# SailfishApp::pathTo("qml/...") base.
find ./qml -type f -exec \
    install -Dm 644 "{}" "%{buildroot}%{_datadir}/harbour-vuo/{}" \;

if ls translations/*.qm >/dev/null 2>&1; then
    install -d %{buildroot}%{_datadir}/harbour-vuo/translations
    install -Dm 644 translations/*.qm %{buildroot}%{_datadir}/harbour-vuo/translations/
fi

# Licence text INSIDE app datadir, not via `%%license`. GPL-3.0 wants it with
# binary; rpm `%%license` puts it in %{_defaultlicensedir}, Harbour rejects:
# only /usr/bin/harbour-vuo, /usr/share/harbour-vuo/*, .desktop file, icons;
# rest = "Installation not allowed in this location".
install -Dm 644 LICENSE %{buildroot}%{_datadir}/harbour-vuo/LICENSE

%files
%defattr(-,root,root,-)
%{_bindir}/harbour-vuo
%{_datadir}/harbour-vuo
%{_datadir}/applications/harbour-vuo.desktop
%{_datadir}/icons/hicolor/*/apps/harbour-vuo.png

%changelog
* Thu Oct 15 2026 Vuo contributors <noreply@example.invalid> - 1.1.1-1
- Update rustls to 0.23.46 for RUSTSEC-2026-0349 and a second advisory fixed in
  the same release.

* Tue Oct 13 2026 Vuo contributors <noreply@example.invalid> - 1.1.0-1
- Search the local mirror: feeds by name, articles by title and text, grouped by
  where they matched and showing why. Ten results per group, more on request.
  Return closes the search keyboard.
- Zoom into an article's images: pinch to zoom, and a double tap no longer
  jumps.
- The cover states the unread total beside the count of new articles after a
  sync, carves the unread count out of its text art, and shows the bare texture
  when nothing is unread.
- "Only sync on Wi-Fi" now takes effect, and no radio or CPU is spent on work
  nobody is watching.
- Globally hidden feeds stay out of Unread and All.
- On Miniflux older than 2.3.2, read and star changes are refused rather than
  sent. A server upgraded past it is noticed without restarting the app.
- A failed server version check keeps the version recorded last time, and a
  read or star toggled again while its request is in flight no longer takes
  that request's error.
- Rewritten English strings, translated into every catalog.

* Wed Sep 16 2026 Vuo contributors <noreply@example.invalid> - 1.0.1-1
- Cut the read path's memory use: article lists no longer carry entry bodies
  they never draw, feed icons are held once instead of per row and per repaint,
  and an article block lays its text out once rather than four times over.
- Return a sync pass's peak to the kernel with malloc_trim instead of letting
  glibc keep it resident.
- Cap both dimensions of an article image, so one oversized image can no longer
  decode without bound.
- Optional retention: read, unfavourited articles can now be dropped from the
  local mirror after a month, three months, six months or a year. Off by
  default -- "Forever" is the default and what every earlier version did.
- Keep the reader's place when a reload returns the same rows.
- rustls 0.23.45, for RUSTSEC-2026-0285 (TLS 1.3 messages accepted at the wrong
  encryption level).
- Log the server's own words when a sync fails, instead of dropping them on a
  field-name collision.

* Mon Sep 07 2026 Vuo contributors <noreply@example.invalid> - 1.0.0-1
- First release.

* Fri Aug 28 2026 Vuo contributors <noreply@example.invalid> - 0.1.0-1
- Initial packaging.
