import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0

Page {
    id: page
    allowedOrientations: Orientation.All

    // Set once stored account read into controls below.
    //
    // Values flow one way: into controls in `Component.onCompleted`, back only
    // on real user change. No two-way bindings: all `settings` properties share
    // ONE notify signal, so any write re-evaluates every control's binding, and
    // unpopulated control writes its default over loaded value.
    property bool ready: false

    // Rust-backed: reads/writes account file (mode 0600, outside SQLite mirror)
    // and media/sync preferences.
    Settings {
        id: settings
        onConnectionTested: {
            testResult.visible = true
            testResult.ok = ok
            testResult.detail = message
        }
    }

    // Worker answers on own thread; result left in slot UI drains. Runs only
    // while page showing and after user asked for test.
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

    // Read stored account, push into controls; else page opens blank. See
    // Settings::reload.
    Component.onCompleted: {
        settings.reload()
        serverField.text = settings.serverUrl
        keyField.text = settings.apiKey
        imagesCombo.currentIndex = settings.mediaPolicy
        refreshCombo.currentIndex = settings.syncIntervalIndex
        markReadCombo.currentIndex = settings.markReadDelayIndex
        retentionCombo.currentIndex = settings.retentionIndex
        wifiOnlySwitch.checked = settings.wifiOnly
        notifySwitch.checked = settings.notifyNewArticles
        caSwitch.checked = settings.useCustomCa
        page.ready = true
    }

    // Save on leaving: half-typed key not written per keystroke.
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
                // Password field on purpose: API key = credential,
                // shoulder-surfing real on phone. §4 prefers key auth:
                // revocable per device.
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
                text: qsTr("Create a key in Miniflux under Settings → API Keys.")
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
                // `detail` = server text on failure, username on success.
                // Foreign either way.
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: ok ? Theme.highlightColor : Theme.errorColor
                text: ok ? qsTr("Connected as %1").arg(detail)
                         : qsTr("Test failed: %1").arg(detail)
            }

            SectionHeader { text: qsTr("Reading") }

            ComboBox {
                id: markReadCombo
                width: parent.width
                label: qsTr("Mark as read when opened")
                menu: ContextMenu {
                    MenuItem { text: qsTr("Never") }
                    MenuItem { text: qsTr("Immediately") }
                    MenuItem { text: qsTr("After 5 seconds") }
                    MenuItem { text: qsTr("After 15 seconds") }
                    MenuItem { text: qsTr("After 30 seconds") }
                }
                onCurrentIndexChanged: if (page.ready) settings.markReadDelayIndex = currentIndex
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
                text: qsTr("By default, Miniflux proxies only http:// images. Loading an image directly tells its website your IP address and when you read the article.")
            }

            SectionHeader { text: qsTr("Synchronisation") }

            // Worker keeps cadence itself while app open or on cover: one
            // process (Harbour rule), no outside timer.
            ComboBox {
                id: refreshCombo
                width: parent.width
                label: qsTr("Sync with server")
                menu: ContextMenu {
                    MenuItem { text: qsTr("Manual only") }
                    MenuItem { text: qsTr("Every 15 minutes") }
                    MenuItem { text: qsTr("Every 30 minutes") }
                    MenuItem { text: qsTr("Hourly") }
                    MenuItem { text: qsTr("Every 6 hours") }
                }
                onCurrentIndexChanged: if (page.ready) settings.syncIntervalIndex = currentIndex
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
                // Two cadences easy to confuse; only one set here.
                text: qsTr("Vuo syncs only while it is open or on the cover. How often Miniflux checks your feeds is set on the server.")
            }

            // Only for work Vuo starts BY ITSELF. Reader-requested (pulley
            // Refresh, cover's, adding feed) goes out on any connection;
            // description says only automatic syncs wait.
            TextSwitch {
                id: wifiOnlySwitch
                text: qsTr("Only sync on Wi-Fi")
                description: qsTr("Automatic syncs wait for Wi-Fi. A refresh that you start yourself runs on any connection.")
                onClicked: settings.wifiOnly = checked
            }

            // Read by root window, which raises notification; worker only
            // syncs, knows nothing of it. Off until reader enables; see
            // `Account::notify_new_articles`.
            TextSwitch {
                id: notifySwitch
                text: qsTr("Notify about new articles")
                onClicked: settings.notifyNewArticles = checked
            }

            // Mirror = server cache; only this control shrinks it. Without it
            // phone keeps every article body ever seen.
            //
            // "Keep everything" = index 0, default: update must not start
            // deleting reader's articles by itself.
            ComboBox {
                id: retentionCombo
                width: parent.width
                label: qsTr("Keep read articles")
                menu: ContextMenu {
                    MenuItem { text: qsTr("Forever") }
                    MenuItem { text: qsTr("For a month") }
                    MenuItem { text: qsTr("For three months") }
                    MenuItem { text: qsTr("For six months") }
                    MenuItem { text: qsTr("For a year") }
                }
                onCurrentIndexChanged: if (page.ready) settings.retentionIndex = currentIndex
            }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
                // Saying what survives matters more than what goes: reader must
                // know favourites safe before picking anything but "Forever".
                text: qsTr("Read articles older than this are deleted from this phone, but not from your Miniflux server. Favourites and unread articles are always kept.")
            }


            SectionHeader { text: qsTr("Advanced") }

            TextSwitch {
                id: caSwitch
                text: qsTr("Use a custom CA certificate")
                // Only https has handshake for CA to apply to; on http://
                // instance (e.g. over VPN) nothing to act on, shown unavailable
                // rather than maybe-needed.
                enabled: serverField.text.indexOf("https:") === 0
                description: enabled
                    ? qsTr("Copy the certificate to ~/.local/share/harbour-vuo/harbour-vuo/ca.pem.")
                    : qsTr("A certificate is used only with an https:// server address.")
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
                text: qsTr("%n change(s) are waiting to be sent to the server.",
                           "", settings.pendingActions)
            }
        }

        VerticalScrollDecorator {}
    }
}
