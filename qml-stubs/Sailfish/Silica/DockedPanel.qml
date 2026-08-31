import QtQuick 2.6
// Silica's edge-docked overlay panel. `show`/`hide` animate it in and out;
// `open` is the settled state.
Item {
    property bool open: false
    readonly property bool expanded: open
    property int dock: 0
    property bool modal: false
    // The panel's own backdrop. Silica defaults it to a PanelBackground; an
    // app may replace it with something opaque.
    property Component background: null
    property int animationDuration: 500
    readonly property real visibleSize: 0
    default property alias _data: holder.data
    function show(immediate) { open = true }
    function hide(immediate) { open = false }
    Item { id: holder; anchors.fill: parent }
}
