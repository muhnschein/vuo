import QtQuick 2.6
Item {
    property int allowedOrientations: 0
    property bool canAccept: true
    property string acceptDestination
    property int acceptDestinationAction: 0
    default property alias __content: __placeholder.data
    Item { id: __placeholder }
    signal accepted()
    signal rejected()
    function accept() {}
    function reject() {}
}
