import QtQuick 2.6
Item {
    property int allowedOrientations: 0
    property bool canAccept: true
    // `var`, not `string`: Silica takes Component, Item or url; root window
    // passes entry list's Component.
    property var acceptDestination
    property int acceptDestinationAction: 0
    // Dialog IS Page in Silica; Vuo's pushed pages read these.
    property Item pageStack
    property int status: 1
    default property alias __content: __placeholder.data
    Item { id: __placeholder }
    signal accepted()
    signal rejected()
    function accept() {}
    function reject() {}
}
