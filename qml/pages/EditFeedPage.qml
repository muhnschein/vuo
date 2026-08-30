import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0
import "../components"

/*
 * Rename a feed and change the handful of its server-side settings that make
 * sense to touch from a phone.
 *
 * Deliberately NOT the full `FeedModificationRequest`. §3 leaves rewrite
 * rules, scraper rules and blocklists to the web UI, and the credential fields
 * (username, password, cookie, user agent, proxy URL) are left out on purpose:
 * a phone form that collects a site password in order to store it server-side
 * is not something this app should be offering.
 *
 * A Page rather than a Dialog. A Dialog's accept is all-or-nothing and gives
 * no place to report the SERVER'S answer -- and this form's save is a network
 * round trip that can fail, so the page has to survive long enough to say so.
 */
Page {
    id: page

    /// The FeedModel, and the row being edited.
    property var model
    property int row: -1

    /// Seeded from the row; these are what the form edits.
    property string feedTitle: ""
    property bool crawler: false
    property bool feedDisabled: false
    property bool hideGlobally: false

    /// Set once the form has been seeded, so seeding does not read as editing.
    property bool ready: false
    property bool saving: false
    /// The `updateSerial` seen when the save was sent, so the answer to THIS
    /// save is told apart from one left over from a previous page.
    property int sentSerial: -1

    allowedOrientations: Orientation.All

    Component.onCompleted: page.ready = true

    /// Send whatever has changed.
    ///
    /// `updateFeed` diffs against what the mirror holds and returns false when
    /// nothing moved, so calling this more often than necessary is free.
    function save() {
        if (!page.ready || !page.model) {
            return
        }
        page.sentSerial = page.model.updateSerial
        if (page.model.updateFeed(page.row, titleField.text, 0,
                                  page.crawler, page.feedDisabled,
                                  page.hideGlobally)) {
            page.saving = true
        }
    }

    // Typing sends on a pause, not per keystroke: a rename is a network round
    // trip, and one per character would be a request storm on a phone radio.
    Timer {
        id: typingSettled
        interval: 900
        onTriggered: page.save()
    }

    // Leaving flushes whatever the pause has not sent yet, so backing out of
    // the page cannot lose the last edit.
    onStatusChanged: if (status === PageStatus.Deactivating) {
        typingSettled.stop()
        page.save()
    }

    // The worker answers on the model's notice slot, which only a poll picks
    // up. The app-level timer polls the feed model only when the ENTRY model
    // reports a change, and a rejected save changes nothing -- so this page
    // does its own polling for as long as it is waiting.
    Timer {
        interval: 500
        repeat: true
        running: page.saving && page.status === PageStatus.Active
        onTriggered: if (page.model) page.model.pollSync()
    }

    /// Mirrors the model's serial so a handler can fire on it.
    property int updateSerial: page.model ? page.model.updateSerial : 0

    onUpdateSerialChanged: {
        if (!page.saving || page.updateSerial === page.sentSerial) {
            return
        }
        page.saving = false
        if (page.model.updateOk) {
            // Nothing to announce: the change is already on screen, and the
            // page stays open because the user did not ask to leave it.
            return
        } else {
            // The server's own words. NoticeBanner renders PlainText.
            notice.post(qsTr("Could not save: %1").arg(page.model.updateError), true, "")
        }
    }

    SilicaFlickable {
        anchors.fill: parent
        contentHeight: form.height + Theme.paddingLarge

        Column {
            id: form
            width: parent.width

            PageHeader { title: qsTr("Feed settings") }

            TextField {
                id: titleField
                width: parent.width
                label: qsTr("Name")
                placeholderText: qsTr("Name")
                text: page.feedTitle
                // A feed name is foreign text, but a TextField is an editor,
                // not a renderer: it has no rich-text mode to be injected
                // into. Nothing else on this page shows the name.
                inputMethodHints: Qt.ImhNoPredictiveText
                // Silica's EnterKey is an attached type, which QML cannot
                // stub, so using it would take this whole file out of
                // `make qml-load`'s reach. Same call as AddFeedPage.qml:28.
                Keys.onReturnPressed: titleField.focus = false
                // An empty name is a rejection, not an instruction: Miniflux
                // would accept it and leave a nameless row the user then
                // cannot identify in order to fix it. `updateFeed` drops it
                // too; this just stops the pointless round trip.
                onTextChanged: if (page.ready && text.trim().length > 0) {
                    typingSettled.restart()
                }
            }

            TextSwitch {
                text: qsTr("Fetch original content")
                description: qsTr("The server scrapes each article's own page "
                                  + "instead of using what the feed provides.")
                checked: page.crawler
                onCheckedChanged: if (page.ready && page.crawler !== checked) {
                    page.crawler = checked
                    page.save()
                }
            }

            TextSwitch {
                text: qsTr("Hide from unread")
                description: qsTr("Keep this feed's articles out of the "
                                  + "Unread and All lists. The feed itself "
                                  + "still shows them.")
                checked: page.hideGlobally
                onCheckedChanged: if (page.ready && page.hideGlobally !== checked) {
                    page.hideGlobally = checked
                    page.save()
                }
            }

            TextSwitch {
                text: qsTr("Pause updates")
                description: qsTr("The server stops refreshing this feed.")
                checked: page.feedDisabled
                onCheckedChanged: if (page.ready && page.feedDisabled !== checked) {
                    page.feedDisabled = checked
                    page.save()
                }
            }

            Item { width: 1; height: Theme.paddingLarge }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
                text: page.saving ? qsTr("Saving\u2026") : qsTr("Changes are saved automatically.")
            }
        }

        VerticalScrollDecorator {}
    }

    NoticeBanner {
        id: notice
        anchors.bottom: parent.bottom
    }
}
