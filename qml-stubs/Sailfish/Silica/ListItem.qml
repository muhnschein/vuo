import QtQuick 2.6
Item {
    // As Silica's own: a list item is as wide as the list it is in. The
    // view's width rather than the parent's: a list's content item is only
    // sized once the view has been polished, which a test never waits for.
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
