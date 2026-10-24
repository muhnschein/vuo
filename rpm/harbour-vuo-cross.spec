# Packaging-only spec for CROSS-BUILT harbour-vuo.
#
# rpm/harbour-vuo.spec = real one, source of truth, builds under sb2 in SDK.
# This = its %install/%files half, fed binary cross-compiled outside sb2
# (docs/sdk-build.md: why SDK cargo can't). Every install and path copied from
# that spec; keep comparable.
%global _binaries_in_noarch_packages_terminate_build 0
%global __os_install_post %{nil}
%global debug_package %{nil}
# Ubuntu rpm otherwise packages /usr/lib/.build-id/* symlinks. Sailfish rpm
# doesn't, not in real spec's %files; two packages claiming tree conflict on
# device.
%define _build_id_links none

# Device runs SailfishOS 4.5+, but xz payloads readable by 4.4 and older; same
# as real spec.
%define _source_payload w6.xzdio
%define _binary_payload w6.xzdio

Name:       harbour-vuo
Summary:    Miniflux feed reader for SailfishOS
Version:    1.1.1
# `--define "vuo_release N"` (scripts/cross-rpm.sh, from VUO_RELEASE) stamps CI
# build so each installs as upgrade; tree keeps 1.
Release:    %{?vuo_release}%{!?vuo_release:1}
License:    GPL-3.0-or-later
Group:      Applications/Internet
URL:        https://github.com/muhnschein/vuo
BuildArch:  aarch64

Requires:   sailfishsilica-qt5 >= 0.10.9
# Sailfish.WebView, for the site page attached to the right of an article.
Requires:   sailfish-components-webview-qt5

# Requires above PACKAGE-level on purpose.
#
# Binary's needs checked from ELF header every build
# (scripts/check-linked-libs.sh); everything it links comes with those two
# packages. Non-Sailfish rpmbuild soname requires produce Provides/Requires
# strings phone rpmdb doesn't recognise: install fails on deps actually present
# (worse than none).
#
# rpm/harbour-vuo.spec (sb2, Sailfish rpm) keeps generator ON: SDK rpm gets
# strings right, Ubuntu's doesn't.
AutoReqProv: no

%description
A native SailfishOS client for a self-hosted Miniflux instance.

Vuo syncs against Miniflux's own REST API, mirrors entries into a local
database for offline reading, and reconciles changes made offline when
connectivity returns. It does not fetch or parse feeds itself: that is the
server's job. One process, sandboxed, as Harbour requires: the app syncs on
its own while it is open or on the cover, and nothing runs when it is not.

%install
rm -rf %{buildroot}
install -D -m 755 %{_sourcedir}/harbour-vuo %{buildroot}%{_bindir}/harbour-vuo
install -D -m 644 %{_sourcedir}/harbour-vuo.desktop \
    %{buildroot}%{_datadir}/applications/harbour-vuo.desktop

for RES in 86x86 108x108 128x128 172x172; do
    install -D -m 644 %{_sourcedir}/icons/${RES}/harbour-vuo.png \
        %{buildroot}%{_datadir}/icons/hicolor/${RES}/apps/harbour-vuo.png
done

# `find ./qml` + install -D reproduces ./qml/... under datadir =
# SailfishApp::pathTo("qml/...") base.
cd %{_sourcedir}
find ./qml -type f -exec \
    install -D -m 644 "{}" "%{buildroot}%{_datadir}/harbour-vuo/{}" \;
cd -

# Compiled translations. `%{_datadir}/harbour-vuo` claimed wholesale by %files,
# no own entry; must land beside qml/ (app resolves SailfishApp::pathTo
# ("translations")).
if ls %{_sourcedir}/translations/*.qm >/dev/null 2>&1; then
    install -d %{buildroot}%{_datadir}/harbour-vuo/translations
    install -D -m 644 %{_sourcedir}/translations/*.qm \
        %{buildroot}%{_datadir}/harbour-vuo/translations/
fi

# Inside app datadir: Harbour allows only /usr/bin/harbour-vuo,
# /usr/share/harbour-vuo/*, .desktop file, icons. %{_datadir}/licenses not on
# list.
install -D -m 644 %{_sourcedir}/LICENSE \
    %{buildroot}%{_datadir}/harbour-vuo/LICENSE

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

* Sat Aug 29 2026 Vuo contributors <noreply@example.invalid> - 0.1.0-1
- Cross-built test package: rebuild the app context when an account is saved.
