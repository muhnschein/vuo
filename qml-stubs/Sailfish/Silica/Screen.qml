pragma Singleton
import QtQuick 2.6
QtObject {
    property int width: 540
    property int height: 960
    property real sizeCategory: 1
    // Silica display-cutout rect. ScopeTabBar clears it like PageHeader; strip
    // not under notch.
    property rect topCutout: Qt.rect(0, 0, 0, 0)
}
