# Packaging-only spec for a CROSS-BUILT harbour-vuo.
#
# rpm/harbour-vuo.spec is the real one and stays the source of truth: it builds
# under sb2 inside the SDK. This one is its %install/%files half, fed a binary
# that was already cross-compiled outside sb2 (see docs/sdk-build.md for why
# the SDK's own cargo cannot do it). Everything installed here, and every path
# it lands on, is copied from that spec so the two stay comparable.
%global _binaries_in_noarch_packages_terminate_build 0
%global __os_install_post %{nil}
%global debug_package %{nil}
# Ubuntu's rpm otherwise packages /usr/lib/.build-id/* symlinks. Sailfish's own
# rpm does not produce them, they are not in the real spec's %files, and two
# packages claiming that tree would conflict on the device.
%define _build_id_links none

# The device runs SailfishOS 4.5+, but xz payloads stay readable by 4.4 and
# older -- the same reason the real spec defaults to them.
%define _source_payload w6.xzdio
%define _binary_payload w6.xzdio

Name:       harbour-vuo
Summary:    Miniflux feed reader for SailfishOS
Version:    0.1.0
# `--define "vuo_release N"` (scripts/cross-rpm.sh, from VUO_RELEASE) stamps a
# CI build so each one installs as an upgrade of the last; the tree keeps 1.
Release:    %{?vuo_release}%{!?vuo_release:1}
License:    GPL-3.0-or-later
Group:      Applications/Internet
URL:        https://github.com/muhnschein/vuo
BuildArch:  aarch64

Requires:   sailfishsilica-qt5 >= 0.10.9
Requires:   nemo-qml-plugin-notifications-qt5
# Sailfish.WebView, for the site page attached to the right of an article.
Requires:   sailfish-components-webview-qt5

# The binary's real needs are already known and verified from its ELF header
# (Qt5 Core/Gui/Widgets/Quick/Qml, libsailfishapp.so.1, glibc <= 2.30). Letting
# a non-Sailfish rpmbuild generate them instead produces Provides/Requires
# strings the phone's rpmdb does not recognise, and the install fails on
# dependencies that are in fact present.
AutoReqProv: no

%description
A native SailfishOS client for a self-hosted Miniflux instance.

Vuo syncs against Miniflux's own REST API, mirrors entries into a local
database for offline reading, and reconciles changes made offline when
connectivity returns. It does not fetch or parse feeds itself: that is the
server's job.

%package sync
Summary:    Background sync timer for Vuo
Requires:   %{name} = %{version}-%{release}
AutoReqProv: no
%description sync
A systemd user timer that refreshes Vuo's mirror periodically.

%install
rm -rf %{buildroot}
install -D -m 755 %{_sourcedir}/harbour-vuo %{buildroot}%{_bindir}/harbour-vuo
install -D -m 644 %{_sourcedir}/harbour-vuo.desktop \
    %{buildroot}%{_datadir}/applications/harbour-vuo.desktop

for RES in 86x86 108x108 128x128 172x172; do
    install -D -m 644 %{_sourcedir}/icons/${RES}/harbour-vuo.png \
        %{buildroot}%{_datadir}/icons/hicolor/${RES}/apps/harbour-vuo.png
done

# `find ./qml` plus install -D reproduces the ./qml/... path under the datadir,
# which is exactly what SailfishApp::pathTo("qml/...") resolves against.
cd %{_sourcedir}
find ./qml -type f -exec \
    install -D -m 644 "{}" "%{buildroot}%{_datadir}/harbour-vuo/{}" \;
cd -

# The compiled translations. `%{_datadir}/harbour-vuo` is already claimed
# wholesale by %files, so these need no entry of their own -- but they DO need
# to land beside qml/, because the app resolves them with
# SailfishApp::pathTo("translations").
if ls %{_sourcedir}/translations/*.qm >/dev/null 2>&1; then
    install -d %{buildroot}%{_datadir}/harbour-vuo/translations
    install -D -m 644 %{_sourcedir}/translations/*.qm \
        %{buildroot}%{_datadir}/harbour-vuo/translations/
fi

install -D -m 644 %{_sourcedir}/systemd/harbour-vuo-sync.service \
    %{buildroot}%{_userunitdir}/harbour-vuo-sync.service
install -D -m 644 %{_sourcedir}/systemd/harbour-vuo-sync.timer \
    %{buildroot}%{_userunitdir}/harbour-vuo-sync.timer

install -D -m 644 %{_sourcedir}/LICENSE \
    %{buildroot}%{_datadir}/licenses/%{name}/LICENSE

%files
%defattr(-,root,root,-)
%{_datadir}/licenses/%{name}/LICENSE
%{_bindir}/harbour-vuo
%{_datadir}/harbour-vuo
%{_datadir}/applications/harbour-vuo.desktop
%{_datadir}/icons/hicolor/*/apps/harbour-vuo.png

%files sync
%defattr(-,root,root,-)
%{_userunitdir}/harbour-vuo-sync.service
%{_userunitdir}/harbour-vuo-sync.timer

%post sync
systemctl-user daemon-reload || :
systemctl-user enable harbour-vuo-sync.timer || :

%preun sync
if [ "$1" = "0" ]; then
    systemctl-user disable harbour-vuo-sync.timer || :
    systemctl-user stop harbour-vuo-sync.timer || :
fi

%postun sync
systemctl-user daemon-reload || :

%changelog
* Sat Aug 29 2026 Vuo contributors <noreply@example.invalid> - 0.1.0-1
- Cross-built test package: rebuild the app context when an account is saved.
