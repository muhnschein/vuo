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

    /// Raised once the account has been written: the root window fetches.
    ///
    /// A signal of its own rather than the root window handling `accepted`.
    /// A QML signal handler is a property, so an `onAccepted` on the
    /// instantiation would REPLACE the one below rather than run beside it,
    /// and the account would silently never be saved.
    signal configured()

    allowedOrientations: Orientation.All

    // Trimmed, because the Rust side trims before it stores: a form filled
    // with spaces would look complete and save nothing.
    canAccept: serverField.text.trim().length > 0 && keyField.text.trim().length > 0

    // `acceptDestination` is the entry list, set by the root window, which
    // owns that Component. Replace rather than push: the welcome page put this
    // dialog in its own place, so the stack is one page deep throughout and
    // the article list ends up as the app's root, with nothing behind it to
    // swipe back into.
    acceptDestinationAction: PageStackAction.Replace

    // Backed by Rust: writes the account file (mode 0600, outside the SQLite
    // mirror) and hands the account to the sync worker.
    Settings {
        id: settings
        onConnectionTested: {
            testResult.visible = true
            testResult.ok = ok
            testResult.detail = message
        }
    }

    // The worker answers on its own thread, so its result waits in a slot the
    // UI drains. The same poll the settings page runs, for the same reason.
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

    // One way, on accept: nothing here binds a field back to the object it
    // writes, which is what made the settings page overwrite what it had just
    // loaded (see SettingsPage.ready).
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
                text: qsTr("Vuo reads from your own Miniflux instance. It never fetches feeds itself.")
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
                // Not a password field by accident: an API key is a
                // credential, and shoulder-surfing is a real threat on a
                // phone. §4 prefers key auth precisely so it can be revoked
                // per device.
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
                text: qsTr("Create a key in Miniflux under Settings → API Keys. A key can be revoked for this device alone.")
            }

            // Optional, and worth offering here rather than only in Settings:
            // a wrong key typed on a phone keyboard is the likeliest way this
            // screen goes wrong, and finding out now beats finding out from an
            // empty article list.
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
                // `detail` is the server's own text on failure and a username
                // on success. Foreign either way (§9.3).
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
