pragma Singleton
import QtQuick 2.6
QtObject {
    property real paddingSmall: 4
    property real paddingMedium: 8
    property real paddingLarge: 16
    property real horizontalPageMargin: 24
    property real fontSizeExtraSmall: 18
    property real fontSizeSmall: 24
    property real fontSizeMedium: 30
    property real fontSizeLarge: 40
    property real fontSizeExtraLarge: 56
    property real fontSizeHuge: 80
    property real itemSizeExtraSmall: 60
    property real itemSizeSmall: 80
    property real itemSizeMedium: 100
    property real itemSizeLarge: 120
    property real itemSizeExtraLarge: 160
    property real iconSizeSmall: 24
    property real iconSizeMedium: 32
    property real iconSizeLarge: 64
    property color primaryColor: "#ffffff"
    property color secondaryColor: "#b0ffffff"
    property color highlightColor: "#80c0ff"
    property color secondaryHighlightColor: "#6090c0"
    property color highlightBackgroundColor: "#4080c0"
    property color errorColor: "#ff4040"
    property color overlayBackgroundColor: "#000000"
    // Silica's opacity steps: faint 0.2, low 0.4, high 0.6, overlay 0.8.
    property real opacityFaint: 0.2
    property real opacityLow: 0.4
    property real opacityHigh: 0.6
    property real opacityOverlay: 0.8
    // The highlight backing Silica paints under a pressed or selected row.
    property real highlightBackgroundOpacity: 0.3
    property string fontFamily: "Sail Sans Pro"
    property string fontFamilyHeading: "Sail Sans Pro Light"
    // The underline rule's thickness. A MISSING stub property here is
    // silent: the binding would resolve to undefined, the Rectangle's
    // height would become 0, and the strip would render with an
    // invisible underline while the load test still passed.
    property real _lineWidth: 1

    function rgba(colour, alpha) { return colour }
}
