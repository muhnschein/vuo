import QtQuick 2.6
import Sailfish.Silica 1.0

CoverBackground {
    id: cover

    property int unreadCount: 0
    property bool syncing: false

    Column {
        anchors.centerIn: parent
        width: parent.width - Theme.paddingLarge * 2
        spacing: Theme.paddingMedium

        Label {
            anchors.horizontalCenter: parent.horizontalCenter
            textFormat: Text.PlainText
            text: cover.unreadCount > 0 ? cover.unreadCount : "—"
            font.pixelSize: Theme.fontSizeHuge
            color: Theme.primaryColor
        }

        Label {
            anchors.horizontalCenter: parent.horizontalCenter
            textFormat: Text.PlainText
            text: qsTr("unread")
            font.pixelSize: Theme.fontSizeSmall
            color: Theme.secondaryColor
        }
    }

    BusyIndicator {
        anchors.centerIn: parent
        running: cover.syncing
        size: BusyIndicatorSize.Medium
    }

    CoverActionList {
        CoverAction {
            iconSource: "image://theme/icon-cover-refresh"
            onTriggered: cover.refresh()
        }
    }

    signal refresh()
}
