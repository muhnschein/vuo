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
        markReadCombo.currentIndex = settings.markReadDelayIndex
        retentionCombo.currentIndex = settings.retentionIndex
        wifiOnlySwitch.checked = settings.wifiOnly
        notifySwitch.checked = settings.notifyNewArticles
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
                // `detail` is the server's own text on failure and a username
                // on success. Foreign either way.
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

            // The worker keeps this cadence itself, while the app is open or
            // on the cover: Vuo is one process, as Harbour requires, so there
            // is no timer outside it to do so.
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
                // The two cadences are easy to confuse, and only one of them
                // is set here.
                text: qsTr("Vuo syncs only while it is open or on the cover. How often Miniflux checks your feeds is set on the server.")
            }

            // Consulted only for the work Vuo starts BY ITSELF. Anything the
            // reader asks for -- the pulley's Refresh, the cover's, adding a
            // feed -- goes out whatever the connection, which is why the
            // description below says only the automatic syncs wait.
            TextSwitch {
                id: wifiOnlySwitch
                text: qsTr("Only sync on Wi-Fi")
                description: qsTr("Automatic syncs wait for Wi-Fi. A refresh that you start yourself runs on any connection.")
                onClicked: settings.wifiOnly = checked
            }

            // Read by the root window, which raises the notification; the
            // worker only syncs, and knows nothing about it. Off until the
            // reader turns it on -- see `Account::notify_new_articles`.
            TextSwitch {
                id: notifySwitch
                text: qsTr("Notify about new articles")
                onClicked: settings.notifyNewArticles = checked
            }

            // The mirror is a cache of the server, and nothing ever removed
            // anything from it: a phone that had read a year of feeds still
            // held every article body it had ever seen. This is the only
            // control that shrinks it.
            //
            // "Keep everything" is index 0 and the default, because that is
            // what every version before this one did. An update does not start
            // deleting the reader's articles on its own.
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
                // Saying exactly what survives matters more than saying what
                // goes: a reader deciding this needs to know their favourites
                // are safe before they pick anything but "Forever".
                text: qsTr("Read articles older than this are deleted from this phone, but not from your Miniflux server. Favourites and unread articles are always kept.")
            }

            // "Only on Wi-Fi" used to sit here, and was removed because it
            // was a control for absent CODE: nothing in the sync path read
            // `wifi_only`, so the switch moved a value nobody consulted. The
            // stored field was kept "for whenever the behaviour is actually
            // implemented", which is what the switch above now is -- it sits
            // in the Synchronisation section, beside the cadence it qualifies,
            // rather than back down here.

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
