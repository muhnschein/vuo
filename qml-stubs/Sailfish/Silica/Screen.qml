pragma Singleton
import QtQuick 2.6
QtObject {
    property int width: 540
    property int height: 960
    property real sizeCategory: 1
    // Silica's display-cutout rect. ScopeTabBar clears it the way PageHeader
    // does, so the strip does not sit under a notch.
    property rect topCutout: Qt.rect(0, 0, 0, 0)
}
