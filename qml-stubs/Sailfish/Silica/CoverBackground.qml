import QtQuick 2.6
// Cover children go into content item that FILLS cover, like Silica's. Without
// fill: zero-sized item at origin, anything anchored to `parent` collapses
// silently (zero-height cover still compiles and instantiates).
Item {
    property real dimmedOpacity: 0.4
    // Whether cover is one shown. `Cover.Active` on device; see Cover.qml.
    // Stubbed Active so load test's cover runs same branch as visible one.
    property int status: Cover.Active
    default property alias __content: __p.data
    Item { id: __p; anchors.fill: parent }
}
