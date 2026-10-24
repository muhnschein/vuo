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

    /// FeedModel and row being edited.
    property var model
    property int row: -1

    /// Seeded from row; form edits these.
    property string feedTitle: ""
    property bool crawler: false
    property bool feedDisabled: false
    property bool hideGlobally: false

    /// Set once form seeded: seeding isn't editing.
    property bool ready: false
    property bool saving: false
    /// `updateSerial` at send: answer to THIS save told apart from previous
    /// page's leftover.
    property int sentSerial: -1

    allowedOrientations: Orientation.All

    Component.onCompleted: page.ready = true

    /// Send whatever changed.
    ///
    /// `updateFeed` diffs against mirror, returns false when nothing moved;
    /// extra calls free.
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

    // Typing sends on pause, not per key: rename = network round trip; per
    // character = request storm on phone radio.
    Timer {
        id: typingSettled
        interval: 900
        onTriggered: page.save()
    }

    // Leaving flushes unsent edit; backing out can't lose last change.
    onStatusChanged: if (status === PageStatus.Deactivating) {
        typingSettled.stop()
        page.save()
    }

    // Worker answers on model's notice slot, only poll sees it. App timer polls
    // feed model only when ENTRY model changes; rejected save changes nothing.
    // So page polls itself while waiting.
    Timer {
        interval: 500
        repeat: true
        running: page.saving && page.status === PageStatus.Active
        onTriggered: if (page.model) page.model.pollSync()
    }

    /// Mirrors model serial so handler can fire on it.
    property int updateSerial: page.model ? page.model.updateSerial : 0

    onUpdateSerialChanged: {
        if (!page.saving || page.updateSerial === page.sentSerial) {
            return
        }
        page.saving = false
        if (page.model.updateOk) {
            // Nothing to announce: change already on screen; page stays (user
            // didn't ask to leave).
            return
        } else {
            // Server's own words. NoticeBanner renders PlainText.
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
                // Feed name foreign, but TextField = editor, not renderer: no
                // rich-text mode to inject. Nothing else here shows name.
                inputMethodHints: Qt.ImhNoPredictiveText
                // Silica EnterKey = attached type, QML can't stub; would take
                // file out of `make qml-load`. Same as AddFeedPage.qml:28.
                Keys.onReturnPressed: titleField.focus = false
                // Empty name = rejection, not instruction: Miniflux accepts it,
                // leaves nameless row user can't identify to fix. `updateFeed`
                // drops it too; this skips pointless round trip.
                onTextChanged: if (page.ready && text.trim().length > 0) {
                    typingSettled.restart()
                }
            }

            TextSwitch {
                text: qsTr("Fetch original content")
                description: qsTr("The server downloads each article from its "
                                  + "website instead of using the text in the feed.")
                checked: page.crawler
                onCheckedChanged: if (page.ready && page.crawler !== checked) {
                    page.crawler = checked
                    page.save()
                }
            }

            // Miniflux `hide_globally`. Vuo Unread and All omit feed (see
            // `EntryFilter`), like Miniflux unread list; Favourites and feed's
            // own page still show articles.
            TextSwitch {
                //: The tab names, as this catalog translates them.
                text: qsTr("Hide from Unread and All")
                checked: page.hideGlobally
                onCheckedChanged: if (page.ready && page.hideGlobally !== checked) {
                    page.hideGlobally = checked
                    page.save()
                }
            }

            TextSwitch {
                text: qsTr("Do not refresh this feed")
                checked: page.feedDisabled
                onCheckedChanged: if (page.ready && page.feedDisabled !== checked) {
                    page.feedDisabled = checked
                    page.save()
                }
            }

            // Bottom padding: last switch not flush with flickable edge.
            Item { width: 1; height: Theme.paddingLarge }
        }

        VerticalScrollDecorator {}
    }

    NoticeBanner {
        id: notice
        anchors.bottom: parent.bottom
    }
}
