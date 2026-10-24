import QtQuick 2.6

// Silica swipeable pager (C++ type: "Sailfish.Silica/PagedView 1.0" in
// plugins.qmltypes).
//
// Repeater = point of stub. Bare Item resolves type but never instantiates
// delegate, so every binding in entry list (all of
// components/EntryListView.qml) goes unchecked by `make qml-load`. All
// instantiated at once: wrong geometry, right for load test.
Item {
    id: root

    property var model: 0
    property Component delegate: null
    property int currentIndex: 0
    readonly property int count: repeater.count
    readonly property Item currentItem: repeater.count > currentIndex && currentIndex >= 0
                                        ? repeater.itemAt(currentIndex)
                                        : null
    readonly property Item contentItem: root
    property bool interactive: true
    readonly property bool dragging: false
    readonly property bool moving: false
    property int cacheSize: 0
    property real horizontalSpacing: 0
    property real verticalSpacing: 0

    function moveTo(index, transition) { root.currentIndex = index }
    function itemAt(index) { return repeater.itemAt(index) }

    Repeater {
        id: repeater
        model: root.model
        delegate: root.delegate
    }
}
