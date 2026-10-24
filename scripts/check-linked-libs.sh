#!/usr/bin/env bash
# Check one binary's DT_NEEDED entries against Harbour allowed-libraries list.
#
# Usage: scripts/check-linked-libs.sh <binary> [readelf]
#
# Harbour refuses package linking anything off list ("Cannot link to shared
# library"); only intake rule not readable from source, decided by link. Called
# from both link sites (`make check` on host Qt, scripts/cross-build.sh on
# device's); list written once.
#
# Transcribed from sailfishos/sdk-harbour-rpmvalidator allowed_libraries.conf as
# of 2026-09-06, minus entries no Qt/Rust app reaches (SDL2, PulseAudio,
# Wayland, codecs). `make vendor-check` guards what keeps libQt5Widgets off; see
# docs/packaging.md.
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
        # Device's own loader. Host build names its own; not package finding
        # (see caller note).
        ld-linux-aarch64.so.1|ld-linux-armhf.so.3|ld-linux.so.2) ;;
        libc.so.6|libm.so.6|libdl.so.2|librt.so.1|libpthread.so.0|libresolv.so.2) ;;
        libstdc++.so.6|libgcc_s.so.1|libz.so.1) ;;
        libcrypto.so.3|libssl.so.3|libsqlite3.so.0|libpng16.so.16|libxml2.so.2) ;;
        libdbus-1.so.3|libglib-2.0.so.0|libgobject-2.0.so.0|libgio-2.0.so.0) ;;
        *) return 1 ;;
    esac
    return 0
}

# Host build links host loader: not on list, not package problem (device build
# names listed one). Ignored by name; host run reports real findings, no false
# one.
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
