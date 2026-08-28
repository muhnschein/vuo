import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0

Page {
    id: page
    allowedOrientations: Orientation.All

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

    // Save on leaving, so a half-typed key is not written on every keystroke.
    Component.onDestruction: settings.save()

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
                width: parent.width
                label: qsTr("Server address")
                placeholderText: qsTr("https://miniflux.example.com")
                inputMethodHints: Qt.ImhUrlCharactersOnly | Qt.ImhNoAutoUppercase
                text: settings.serverUrl
                onTextChanged: settings.serverUrl = text
            }

            TextField {
                width: parent.width
                label: qsTr("API key")
                // Not a password field by accident: an API key is a
                // credential, and shoulder-surfing is a real threat on a
                // phone. §4 prefers key auth precisely so it can be revoked
                // per device.
                echoMode: TextInput.Password
                inputMethodHints: Qt.ImhNoAutoUppercase | Qt.ImhNoPredictiveText
                text: settings.apiKey
                onTextChanged: settings.apiKey = text
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
                onClicked: settings.testConnection()
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
                         : qsTr("Could not connect: %1").arg(detail)
            }

            SectionHeader { text: qsTr("Images") }

            ComboBox {
                width: parent.width
                label: qsTr("Images not proxied by your server")
                currentIndex: settings.mediaPolicy
                menu: ContextMenu {
                    MenuItem { text: qsTr("Never load") }
                    MenuItem { text: qsTr("Ask each site") }
                    MenuItem { text: qsTr("Always load") }
                }
                onCurrentIndexChanged: settings.mediaPolicy = currentIndex
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
                width: parent.width
                label: qsTr("Background refresh")
                currentIndex: settings.syncIntervalIndex
                menu: ContextMenu {
                    MenuItem { text: qsTr("Manual only") }
                    MenuItem { text: qsTr("Every 15 minutes") }
                    MenuItem { text: qsTr("Every 30 minutes") }
                    MenuItem { text: qsTr("Hourly") }
                    MenuItem { text: qsTr("Every 6 hours") }
                }
                onCurrentIndexChanged: settings.syncIntervalIndex = currentIndex
            }

            TextSwitch {
                text: qsTr("Only on Wi-Fi")
                checked: settings.wifiOnly
                onCheckedChanged: settings.wifiOnly = checked
            }

            SectionHeader { text: qsTr("Advanced") }

            TextSwitch {
                text: qsTr("Use a custom CA certificate")
                description: qsTr("For a self-hosted server with a private certificate authority. Place the certificate at ~/.local/share/harbour-vuo/ca.pem. Certificate verification is never disabled, and there is no option to disable it.")
                checked: settings.useCustomCa
                onCheckedChanged: settings.useCustomCa = checked
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
