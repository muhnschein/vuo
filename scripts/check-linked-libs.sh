#!/usr/bin/env bash
# Check one binary's DT_NEEDED entries against Harbour's allowed-libraries list.
#
# Usage: scripts/check-linked-libs.sh <binary> [readelf]
#
# Harbour refuses a package whose binary links anything not on this list
# ("Cannot link to shared library"), and it is the one intake rule that cannot
# be read out of the source tree: it is decided by the link. So this is called
# from both places a link happens -- `make check` on the host's Qt, and
# scripts/cross-build.sh on the device's -- with the list written down once.
#
# The list is transcribed from sailfishos/sdk-harbour-rpmvalidator's
# allowed_libraries.conf as of 2026-09-06, minus the entries no Qt/Rust app
# could reach (SDL2, PulseAudio, Wayland, the codecs). `make vendor-check`
# guards the thing that keeps libQt5Widgets off it; see docs/packaging.md.
set -euo pipefail

BIN="${1:?usage: check-linked-libs.sh <binary> [readelf]}"
READELF="${2:-readelf}"
[[ -r "$BIN" ]] || { echo "FAIL: $BIN is not readable" >&2; exit 1; }

harbour_allows() {
    local lib=$1
    case "$lib" in
        libQt5Core.so.5|libQt5Gui.so.5|libQt5Qml.so.5|libQt5Quick.so.5|\
libQt5Network.so.5|libQt5Concurrent.so.5|libQt5DBus.so.5|libQt5Sql.so.5|\
libQt5Svg.so.5|libQt5Xml.so.5|libQt5XmlPatterns.so.5|libQt5Multimedia.so.5|\
libQt5Sensors.so.5|libQt5Positioning.so.5|libQt5WebSockets.so.5|libQt5Location.so.5) ;;
        libsailfishapp.so.1|libsailfishsilica.so.1|libmdeclarativecache5.so.0) ;;
        libqt5embedwidget.so.1|libsailfishwebengine.so.1) ;;
        libEGL.so.1|libGLESv1_CM.so.1|libGLESv2.so.2) ;;
        # The device's own loader. A host build names its own instead, which is
        # not a finding about the package -- see the caller's note.
        ld-linux-aarch64.so.1|ld-linux-armhf.so.3|ld-linux.so.2) ;;
        libc.so.6|libm.so.6|libdl.so.2|librt.so.1|libpthread.so.0|libresolv.so.2) ;;
        libstdc++.so.6|libgcc_s.so.1|libz.so.1) ;;
        libcrypto.so.3|libssl.so.3|libsqlite3.so.0|libpng16.so.16|libxml2.so.2) ;;
        libdbus-1.so.3|libglib-2.0.so.0|libgobject-2.0.so.0|libgio-2.0.so.0) ;;
        *) return 1 ;;
    esac
    return 0
}

# A host build links the host's loader, which is neither on the list nor a
# problem for the package: the device build names one that is. Ignored by name
# so a host run reports what it can rather than one false finding.
host_only() {
    local lib=$1
    case "$lib" in
        ld-linux-x86-64.so.2|ld-linux-x86-64.so.*|ld-linux.so.2) return 0 ;;
        *) return 1 ;;
    esac
}

forbidden=0
for lib in $("$READELF" -d "$BIN" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p'); do
    if harbour_allows "$lib" || host_only "$lib"; then
        echo "   ok       $lib"
    else
        echo "   REJECTED $lib"
        forbidden=$((forbidden + 1))
    fi
done

if [[ "$forbidden" -ne 0 ]]; then
    echo "FAIL: $forbidden linked libraries are not on Harbour's allowed list." >&2
    echo "The package would install and run on a device, but Harbour will refuse it." >&2
    echo 'See docs/packaging.md, "Harbour readiness".' >&2
    exit 1
fi
echo "  every linked library is one Harbour allows"
