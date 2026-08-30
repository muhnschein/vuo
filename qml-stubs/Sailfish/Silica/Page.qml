import QtQuick 2.6
Item {
    property int allowedOrientations: 0
    property int orientation: 0
    property bool backNavigation: true
    property bool forwardNavigation: true
    property Item pageStack
    property int status: 1
    property bool isPortrait: true
    property bool isLandscape: false
}
