import QtQuick 2.6
Flickable {
    property bool quickScroll: true
    property Item pullDownMenu
    property Item pushUpMenu
    default property alias __content: __placeholder.data
    Item { id: __placeholder }
}
