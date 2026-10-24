import QtQuick 2.6
Item {
    // Like Silica: list item as wide as its list. View width, not parent's:
    // list's content item sized only after view polished; test never waits.
    width: ListView.view ? ListView.view.width : (parent ? parent.width : 0)
    property real contentHeight: 80
    height: contentHeight
    property Item menu
    property bool down: false
    property bool highlighted: false
    property bool showMenuOnPressAndHold: true
    default property alias __content: __p.data
    Item { id: __p; width: parent.width; height: parent.contentHeight }
    signal clicked()
    function remorseAction(text, action, timeout) {}
    function remorseDelete(action) {}
    function showMenu() {}
}
