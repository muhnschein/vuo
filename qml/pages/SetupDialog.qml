import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0

/*
 * First run: the two things Vuo cannot start without.
 *
 * A Dialog rather than a Page, because setup ends somewhere new. Silica's back
 * gesture means "take me where I came from", and the first version of this
 * flow answered it by going somewhere else instead: you filled in the settings
 * page, swiped back, landed on the welcome screen for an instant and were then
 * thrown forward into the article list. Qt saw the same defect from the inside
 * and logged a binding loop on the welcome page's `status`, because that page
 * was navigating from inside its own status-change handler. Accepting a dialog
 * IS forward navigation, so the whole flow now runs one way and the handler is
 * gone with it.
 *
 * Only the server and the key are asked for. Everything else Settings offers
 * -- images, sync interval, when an article counts as read -- has a default
 * that is right for a new install, and asking about them before the first
 * article has been seen is asking someone to decide something they have no way
 * to judge yet.
 */
Dialog {
    id: dialog

    /// Raised once account written: root window fetches.
    ///
    /// Own signal, not root window handling `accepted`. QML signal handler =
    /// property; `onAccepted` on instantiation REPLACES one below, account
    /// silently never saved.
    signal configured()

    allowedOrientations: Orientation.All

    // Trimmed: Rust trims before storing; all-spaces form looks complete, saves
    // nothing.
    canAccept: serverField.text.trim().length > 0 && keyField.text.trim().length > 0

    // `acceptDestination` = entry list, set by root window (owns Component).
    // Replace, not push: welcome page put dialog in own place; stack one deep
    // throughout, article list ends as root, nothing behind to swipe back to.
    acceptDestinationAction: PageStackAction.Replace

    // Rust-backed: writes account file (mode 0600, outside SQLite mirror),
    // hands account to sync worker.
    Settings {
        id: settings
        onConnectionTested: {
            testResult.visible = true
            testResult.ok = ok
            testResult.detail = message
        }
    }

    // Worker answers on own thread; result waits in slot UI drains. Same poll
    // as settings page, same reason.
    Timer {
        id: noticePoll
        interval: 400
        repeat: true
        running: false
        onTriggered: {
            if (settings.pollNotice()) {
                noticePoll.running = false
            }
        }
    }

    // Stored defaults (images, sync interval, read threshold) into object so
    // save carries them, not `i32::default()`. Nothing shown; screen asks only
    // for account. Without it first run saves Strict images + "Manual only"
    // sync (zero = unloaded Rust field = first entry of both lists).
    Component.onCompleted: settings.reload()

    // One way, on accept: no field bound back to object it writes (see
    // SettingsPage.ready).
    onAccepted: {
        settings.serverUrl = serverField.text
        settings.apiKey = keyField.text
        settings.save()
        dialog.configured()
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height

        Column {
            id: column
            width: dialog.width
            spacing: Theme.paddingMedium

            DialogHeader {
                title: qsTr("Your Miniflux server")
                acceptText: qsTr("Start reading")
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
                text: qsTr("Your Miniflux server fetches your feeds, and Vuo shows them.")
            }

            TextField {
                id: serverField
                width: parent.width
                label: qsTr("Server address")
                placeholderText: qsTr("https://miniflux.example.com")
                inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
            }

            TextField {
                id: keyField
                width: parent.width
                label: qsTr("API key")
                // Password field on purpose: API key = credential,
                // shoulder-surfing real on phone. §4 prefers key auth:
                // revocable per device.
                echoMode: TextInput.Password
                inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
                text: qsTr("Create a key in Miniflux under Settings → API Keys.")
            }

            // Optional, offered here not only in Settings: mistyped key on
            // phone keyboard = likeliest failure; find out now, not from empty
            // list.
            Button {
                anchors.horizontalCenter: parent.horizontalCenter
                text: qsTr("Test connection")
                enabled: dialog.canAccept
                onClicked: {
                    testResult.visible = false
                    settings.serverUrl = serverField.text
                    settings.apiKey = keyField.text
                    settings.testConnection()
                    noticePoll.running = true
                }
            }

            Label {
                id: testResult
                property bool ok: false
                property string detail: ""
                visible: false
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                // `detail` = server text on failure, username on success.
                // Foreign either way (§9.3).
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: ok ? Theme.highlightColor : Theme.errorColor
                text: ok ? qsTr("Connected as %1").arg(detail)
                         : qsTr("Test failed: %1").arg(detail)
            }
        }

        VerticalScrollDecorator {}
    }
}
