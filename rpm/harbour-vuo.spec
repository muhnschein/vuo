# Vuo -- SailfishOS RPM spec.
#
# Structure follows Whisperfish, which is the closest prior art for a Rust+QML
# Sailfish app (§4). Notes on the non-obvious parts are inline; the rationale
# for the whole approach is in docs/packaging.md.

%bcond_with harbour
%bcond_with vendor
%bcond_with lto
%bcond_without xz

# OBS and Chum cannot pass --with on the command line, so the flags they need
# are forced here instead.
%if 0%{?_chum} || 0%{?_obs}
%define with_lto 1
%define with_vendor 1
%endif

%if %{with xz}
# SailfishOS 4.5+ defaults to Zstd payloads, which 4.4 and older cannot read.
%define _source_payload w6.xzdio
%define _binary_payload w6.xzdio
%endif

Name:       harbour-vuo
Summary:    Miniflux feed reader for SailfishOS
Version:    0.1.0
Release:    1
License:    GPL-3.0-or-later
Group:      Applications/Internet
URL:        https://github.com/muhnschein/vuo
Source0:    %{name}-%{version}.tar.xz
%if %{with vendor}
# Produced by scripts/vendor-crates.sh; not present in the git repository.
Source1:    vendor.tar.xz
Source2:    vendor.toml
%endif

Requires:   sailfishsilica-qt5 >= 0.10.9
Requires:   nemo-qml-plugin-notifications-qt5

BuildRequires:  pkgconfig(sailfishapp) >= 1.0.3
BuildRequires:  pkgconfig(Qt5Core)
BuildRequires:  pkgconfig(Qt5Qml)
BuildRequires:  pkgconfig(Qt5Quick)
# qttypes links Qt5Widgets unconditionally, even though Vuo never instantiates
# a QApplication. Without this the link fails late and confusingly.
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
server's job.

%if %{without harbour}
%package -n %{name}-sync
Summary:    Background sync timer for Vuo
Requires:   %{name} = %{version}-%{release}
%description -n %{name}-sync
A systemd user timer that refreshes Vuo's mirror periodically.
%endif

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

# Scratchbox2 accelerates rustc by running it as x86; this is how it learns
# what the real target is.
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

# qttypes probes for qmake6 first and errors out when it is absent.
export QMAKE=/usr/bin/qmake
export PKG_CONFIG_ALLOW_CROSS_i686_unknown_linux_gnu=1
export PKG_CONFIG_ALLOW_CROSS_armv7_unknown_linux_gnueabihf=1
export PKG_CONFIG_ALLOW_CROSS_aarch64_unknown_linux_gnu=1

%if %{with lto}
export CARGO_PROFILE_RELEASE_LTO=thin
%endif

# Works around a Scratchbox bug where /tmp/[...]/symbols.o is not found.
export TMPDIR=${TMPDIR:-"$PWD/.tmp"}
mkdir -p $TMPDIR

# The workspace's default-members are the Qt-free set, so a bare
# `cargo build --release` would build nothing installable. The bin must be
# named explicitly.
cargo build --jobs %{?_smp_build_ncpus:%{_smp_build_ncpus}}%{!?_smp_build_ncpus:1} \
    --release \
    --bin harbour-vuo \
    --features sailfishapp \
    $OFFLINE

lrelease -idbased translations/*.ts || :

%install
install -D %{targetdir}/harbour-vuo %{buildroot}%{_bindir}/harbour-vuo

desktop-file-install --dir %{buildroot}%{_datadir}/applications harbour-vuo.desktop

for RES in 86x86 108x108 128x128 172x172; do
    install -Dm 644 icons/${RES}/harbour-vuo.png \
        %{buildroot}%{_datadir}/icons/hicolor/${RES}/apps/harbour-vuo.png
done

# `find ./qml` plus install -D reproduces the ./qml/... path under the datadir,
# which is exactly what SailfishApp::pathTo("qml/...") resolves against.
find ./qml -type f -exec \
    install -Dm 644 "{}" "%{buildroot}%{_datadir}/harbour-vuo/{}" \;

if ls translations/*.qm >/dev/null 2>&1; then
    install -d %{buildroot}%{_datadir}/harbour-vuo/translations
    install -Dm 644 translations/*.qm %{buildroot}%{_datadir}/harbour-vuo/translations/
fi

%if %{without harbour}
install -Dm 644 systemd/harbour-vuo-sync.service \
    %{buildroot}%{_userunitdir}/harbour-vuo-sync.service
install -Dm 644 systemd/harbour-vuo-sync.timer \
    %{buildroot}%{_userunitdir}/harbour-vuo-sync.timer
%endif

%files
%defattr(-,root,root,-)
%license LICENSE
%{_bindir}/harbour-vuo
%{_datadir}/harbour-vuo
%{_datadir}/applications/harbour-vuo.desktop
%{_datadir}/icons/hicolor/*/apps/harbour-vuo.png

%if %{without harbour}
%files -n %{name}-sync
%defattr(-,root,root,-)
%{_userunitdir}/harbour-vuo-sync.service
%{_userunitdir}/harbour-vuo-sync.timer

%post -n %{name}-sync
systemctl-user daemon-reload || :
systemctl-user enable harbour-vuo-sync.timer || :

%preun -n %{name}-sync
if [ "$1" = "0" ]; then
    systemctl-user disable harbour-vuo-sync.timer || :
    systemctl-user stop harbour-vuo-sync.timer || :
fi

%postun -n %{name}-sync
systemctl-user daemon-reload || :
%endif

%changelog
* Fri Aug 28 2026 Vuo contributors <noreply@example.invalid> - 0.1.0-1
- Initial packaging.
