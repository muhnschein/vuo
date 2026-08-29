import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0

Page {
    id: page
    allowedOrientations: Orientation.All

    // Set once the stored account has been read into the controls below.
    //
    // Every control used to both bind its value to `settings` AND write back
    // on change. That is a two-way binding on a QObject whose properties all
    // share ONE notify signal, so any write re-evaluated every other control's
    // binding -- and a control that had not been populated yet would write its
    // own default straight back over what was loaded. Values now flow one way,
    // in `Component.onCompleted`, and back only on a real user change.
    property bool ready: false

    // Backed by Rust: reads and writes the account file (mode 0600, outside
    // the SQLite mirror) and the media/sync preferences.
    Settings {
        id: settings
        onConnectionTested: {
            testResult.visible = true
            testResult.ok = ok
            testResult.detail = message
        }
    }

    // The worker answers on its own thread, so its result is left in a slot
    // the UI drains. Only runs while this page is showing, and only after the
    // user has actually asked for a test.
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

    // Read the stored account, then push it into the controls. Nothing called
    // this before, so the page always opened blank -- see Settings::reload.
    Component.onCompleted: {
        settings.reload()
        serverField.text = settings.serverUrl
        keyField.text = settings.apiKey
        imagesCombo.currentIndex = settings.mediaPolicy
        refreshCombo.currentIndex = settings.syncIntervalIndex
        wifiSwitch.checked = settings.wifiOnly
        caSwitch.checked = settings.useCustomCa
        page.ready = true
    }

    // Save on leaving, so a half-typed key is not written on every keystroke.
    Component.onDestruction: if (page.ready) settings.save()

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: column.height

        Column {
            id: column
            width: page.width
            spacing: Theme.paddingMedium

            PageHeader { title: qsTr("Settings") }

            SectionHeader { text: qsTr("Account") }

            TextField {
                id: serverField
                width: parent.width
                label: qsTr("Server address")
                placeholderText: qsTr("https://miniflux.example.com")
                inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
                onTextChanged: if (page.ready) settings.serverUrl = text
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
                onTextChanged: if (page.ready) settings.apiKey = text
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

            Button {
                anchors.horizontalCenter: parent.horizontalCenter
                text: qsTr("Test connection")
                onClicked: {
                    testResult.visible = false
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
                // on success. Foreign either way.
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: ok ? Theme.highlightColor : Theme.errorColor
                text: ok ? qsTr("Connected as %1").arg(detail)
                         : qsTr("Test failed: %1").arg(detail)
            }

            SectionHeader { text: qsTr("Images") }

            ComboBox {
                id: imagesCombo
                width: parent.width
                label: qsTr("Images not proxied by your server")
                menu: ContextMenu {
                    MenuItem { text: qsTr("Never load") }
                    MenuItem { text: qsTr("Ask each site") }
                    MenuItem { text: qsTr("Always load") }
                }
                onCurrentIndexChanged: if (page.ready) settings.mediaPolicy = currentIndex
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
                // The actual fix is a server setting, and saying so is more
                // useful than silently degrading.
                text: qsTr("Miniflux proxies plain-http images only by default, so most images arrive unproxied. Loading them directly tells those sites your IP address and when you read. Ask your server administrator to set MEDIA_PROXY_MODE=all for full protection.")
            }

            SectionHeader { text: qsTr("Synchronisation") }

            ComboBox {
                id: refreshCombo
                width: parent.width
                label: qsTr("Background refresh")
                menu: ContextMenu {
                    MenuItem { text: qsTr("Manual only") }
                    MenuItem { text: qsTr("Every 15 minutes") }
                    MenuItem { text: qsTr("Every 30 minutes") }
                    MenuItem { text: qsTr("Hourly") }
                    MenuItem { text: qsTr("Every 6 hours") }
                }
                onCurrentIndexChanged: if (page.ready) settings.syncIntervalIndex = currentIndex
            }

            TextSwitch {
                id: wifiSwitch
                text: qsTr("Only on Wi-Fi")
                // `onClicked`, not `onCheckedChanged`: Silica toggles `checked`
                // itself and then emits this, so only a real tap writes back.
                onClicked: settings.wifiOnly = checked
            }

            SectionHeader { text: qsTr("Advanced") }

            TextSwitch {
                id: caSwitch
                text: qsTr("Use a custom CA certificate")
                // Only https does a handshake for a CA to apply to, so on an
                // http:// instance -- one reached over a VPN, say -- this
                // setting has nothing to act on and is shown as unavailable
                // rather than as something that might be needed.
                enabled: serverField.text.indexOf("https:") === 0
                description: enabled
                    ? qsTr("For a self-hosted server with a private certificate authority. Place the certificate at ~/.local/share/harbour-vuo/ca.pem. Certificate verification is never disabled, and there is no option to disable it.")
                    : qsTr("Only applies to an https:// server. This one is not encrypted by TLS, so no certificate is used.")
                onClicked: settings.useCustomCa = checked
            }

            Label {
                visible: settings.pendingActions > 0
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.highlightColor
                text: qsTr("%n change(s) waiting to be sent to the server.",
                           "", settings.pendingActions)
            }
        }

        VerticalScrollDecorator {}
    }
}
