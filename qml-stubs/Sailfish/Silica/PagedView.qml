import QtQuick 2.6

// Silica's swipeable pager (a C++ type: "Sailfish.Silica/PagedView 1.0" in
// plugins.qmltypes).
//
// The Repeater is the point of this stub. A bare Item would resolve the type
// but never instantiate the delegate, so every binding inside the entry list
// -- the whole of components/EntryListView.qml -- would stop being checked by
// `make qml-load`. Instantiating all of them at once is wrong geometry and
// exactly right for a load test.
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
