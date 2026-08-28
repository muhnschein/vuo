import QtQuick 2.6
Item {
    property real contentHeight: 80
    property Item menu
    property bool down: false
    property bool highlighted: false
    property bool showMenuOnPressAndHold: true
    default property alias __content: __p.data
    Item { id: __p }
    signal clicked()
    function remorseAction(text, action, timeout) {}
    function remorseDelete(action) {}
    function showMenu() {}
}
