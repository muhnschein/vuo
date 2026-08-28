import QtQuick 2.6
Item {
    property string label
    property string description
    property int currentIndex: 0
    property string value
    property Item menu
    default property alias __content: __placeholder.data
    Item { id: __placeholder }
}
