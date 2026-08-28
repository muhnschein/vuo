import QtQuick 2.6
Item {
    property Component initialPage
    property var cover
    property int allowedOrientations: 0
    property int defaultAllowedOrientations: 0
    property Item pageStack
    property string applicationName
    default property alias __content: __placeholder.data
    Item { id: __placeholder }
}
