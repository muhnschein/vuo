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
    property int categoryId: 0
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

    CategoryModel { id: categories }

    Component.onCompleted: {
        categories.refresh()
        page.ready = true
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
            pageStack.pop()
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
            }

            ComboBox {
                id: categoryCombo
                width: parent.width
                label: qsTr("Category")
                // A feed with no category is possible in the mirror (the id is
                // 0 when the server sent none), and there is nothing sensible
                // to preselect for it.
                currentIndex: categories.rowForId(page.categoryId)
                menu: ContextMenu {
                    Repeater {
                        model: categories
                        MenuItem {
                            // A category name is foreign text. MenuItem gives
                            // no way to force its internal label's textFormat,
                            // so the name is shown by a Label this file owns
                            // and the item's own text is left empty.
                            Label {
                                anchors.centerIn: parent
                                width: parent.width - Theme.horizontalPageMargin * 2
                                horizontalAlignment: Text.AlignHCenter
                                textFormat: Text.PlainText
                                text: title
                                truncationMode: TruncationMode.Fade
                                color: Theme.primaryColor
                            }
                        }
                    }
                }
                onCurrentIndexChanged: if (page.ready) {
                    page.categoryId = categories.idAt(categoryCombo.currentIndex)
                }
            }

            TextSwitch {
                text: qsTr("Fetch original content")
                description: qsTr("The server scrapes each article's own page "
                                  + "instead of using what the feed provides.")
                checked: page.crawler
                onCheckedChanged: if (page.ready) page.crawler = checked
            }

            TextSwitch {
                text: qsTr("Hide from unread")
                description: qsTr("Keep this feed's articles out of the "
                                  + "Unread and All lists. The feed itself "
                                  + "still shows them.")
                checked: page.hideGlobally
                onCheckedChanged: if (page.ready) page.hideGlobally = checked
            }

            TextSwitch {
                text: qsTr("Pause updates")
                description: qsTr("The server stops refreshing this feed.")
                checked: page.feedDisabled
                onCheckedChanged: if (page.ready) page.feedDisabled = checked
            }

            Item { width: 1; height: Theme.paddingLarge }

            Button {
                anchors.horizontalCenter: parent.horizontalCenter
                text: page.saving ? qsTr("Saving…") : qsTr("Save")
                enabled: !page.saving && titleField.text.trim().length > 0
                onClicked: {
                    page.feedTitle = titleField.text
                    page.sentSerial = page.model ? page.model.updateSerial : 0
                    var sent = page.model
                               ? page.model.updateFeed(page.row, page.feedTitle,
                                                       page.categoryId, page.crawler,
                                                       page.feedDisabled, page.hideGlobally)
                               : false
                    if (sent) {
                        page.saving = true
                    } else {
                        // `updateFeed` returns false when nothing actually
                        // changed, which is not an error -- it is a Save the
                        // user did not need to press.
                        pageStack.pop()
                    }
                }
            }
        }

        VerticalScrollDecorator {}
    }

    NoticeBanner {
        id: notice
        anchors.bottom: parent.bottom
    }
}
