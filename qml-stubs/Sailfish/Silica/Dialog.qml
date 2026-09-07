import QtQuick 2.6
Item {
    property int allowedOrientations: 0
    property bool canAccept: true
    // `var`, not `string`: Silica takes a Component, an Item or a url here,
    // and the root window hands this one the entry list's Component.
    property var acceptDestination
    property int acceptDestinationAction: 0
    // A Dialog IS a Page in Silica, and the pages Vuo pushes read these.
    property Item pageStack
    property int status: 1
    default property alias __content: __placeholder.data
    Item { id: __placeholder }
    signal accepted()
    signal rejected()
    function accept() {}
    function reject() {}
}
