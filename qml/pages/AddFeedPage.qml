import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Subscribing. §3 keeps add/remove in scope and leaves rewrite rules, scraper
 * rules, blocklists and integrations to the web UI -- so this page has exactly
 * two fields.
 */
Dialog {
    id: dialog
    property var model
    property string feedUrl: ""

    canAccept: feedUrl.trim().length > 0

    Column {
        width: parent.width

        DialogHeader { acceptText: qsTr("Subscribe") }

        TextField {
            width: parent.width
            label: qsTr("Feed or site address")
            placeholderText: qsTr("https://example.com/feed.xml")
            inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
            text: dialog.feedUrl
            onTextChanged: dialog.feedUrl = text
            // Silica EnterKey = attached type, QML can't stub; would make file
            // unverifiable off-device. Keys.onReturnPressed = plain Qt Quick,
            // same for virtual keyboard return.
            Keys.onReturnPressed: dialog.accept()
        }
    }

    onAccepted: model.subscribe(feedUrl.trim())
}
