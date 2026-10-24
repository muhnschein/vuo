import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    id: page
    property var model
    /// BROWSE entry model: one per-feed view scopes to itself.
    ///
    /// Never tab's model. `setScope` overwrites, nothing restores: Unread tab
    /// would list feed until restart. See harbour-vuo.qml `browseEntries`.
    property var entryModel
    /// Model carrying refresh failures. Passed straight on; see
    /// EntryListPage.noticeModel.
    property var noticeModel: null

    // Re-read on way in.
    //
    // `FeedModel.pollSync` driven by app timer, fires only when ENTRY models
    // changed; feed settings changed elsewhere (web UI, crawler-column
    // migration default) sit stale until something unrelated moves. Editor
    // seeded from these rows: stale here = stale there.
    onStatusChanged: if (status === PageStatus.Activating && page.model) {
        page.model.refresh()
    }

    allowedOrientations: Orientation.All

    SilicaListView {
        anchors.fill: parent
        model: page.model

        header: PageHeader { title: qsTr("Feeds") }

        PullDownMenu {
            MenuItem {
                text: qsTr("Add feed")
                onClicked: pageStack.push(Qt.resolvedUrl("AddFeedPage.qml"),
                                          { model: page.model })
            }
        }

        ViewPlaceholder {
            enabled: page.model ? page.model.count === 0 : true
            text: qsTr("No feeds")
            hintText: qsTr("Pull down to add a feed")
        }

        delegate: ListItem {
            contentHeight: col.height + Theme.paddingMedium * 2

            Column {
                id: col
                x: Theme.horizontalPageMargin
                y: Theme.paddingMedium
                width: parent.width - Theme.horizontalPageMargin * 2

                Item {
                    width: parent.width
                    height: feedTitle.height

                    Label {
                        id: unreadBadge
                        anchors.right: parent.right
                        anchors.verticalCenter: feedTitle.verticalCenter
                        // Per-feed unread count: whether feed has anything,
                        // without opening it.
                        text: unreadCount > 0 ? unreadCount : ""
                        color: Theme.highlightColor
                        font.pixelSize: Theme.fontSizeSmall
                    }
                    Image {
                        id: favicon

                        // Centred on title line box, like entry list; see
                        // components/EntryListView.qml.
                        anchors.left: parent.left
                        anchors.verticalCenter: feedTitle.verticalCenter
                        width: Theme.fontSizeMedium
                        height: Theme.fontSizeMedium
                        sourceSize.width: Theme.fontSizeMedium
                        sourceSize.height: Theme.fontSizeMedium
                        fillMode: Image.PreserveAspectFit
                        // `data:` URI built in Rust from mirror bytes; drawing
                        // fetches nothing, scroll can't leak device IP (§9.3).
                        source: feedIcon
                        asynchronous: true
                        // Mirror stores icon in arrival format; device has
                        // handlers for only some. Collapse on failure: missing
                        // handler = missing icon, not broken-image glyph.
                        visible: feedIcon.length > 0 && status === Image.Ready
                    }
                    Label {
                        id: feedTitle
                        // Gutter same width with or without icon: mixed list
                        // not ragged.
                        anchors.left: parent.left
                        anchors.leftMargin: favicon.width + Theme.paddingMedium
                        anchors.right: unreadBadge.left
                        anchors.rightMargin: unreadBadge.width > 0 ? Theme.paddingMedium : 0
                        // Feed name chosen by feed operator.
                        textFormat: Text.PlainText
                        text: title
                        truncationMode: TruncationMode.Fade
                        color: Theme.primaryColor
                    }
                }
                Label {
                    visible: errorMessage.length > 0
                    // Indented to title, not icon gutter: one row. Width off
                    // indent too, else long error runs past right margin.
                    x: favicon.width + Theme.paddingMedium
                    width: parent.width - x
                    // Server's own error text, verbatim from remote site. Plain
                    // text.
                    textFormat: Text.PlainText
                    text: errorMessage
                    wrapMode: Text.Wrap
                    maximumLineCount: 2
                    elide: Text.ElideRight
                    font.pixelSize: Theme.fontSizeExtraSmall
                    color: Theme.errorColor
                }
            }

            // Scope as (kind, id), with model; else page opens empty.
            onClicked: pageStack.push(Qt.resolvedUrl("EntryListPage.qml"), {
                model: page.entryModel,
                // Passed on: feed view's pulley can open Feeds again, feed
                // opened THERE has model too.
                browseModel: page.entryModel,
                feedModel: page.model,
                noticeModel: page.noticeModel,
                scopeLabel: qsTr("Feed"),
                title: title,
                scopeKind: 3,
                scopeId: feedId
            })

            menu: ContextMenu {
                MenuItem {
                    text: qsTr("Feed settings")
                    // Seeded from row, not re-read on other page: delegate has
                    // current values; second query = second chance to disagree.
                    onClicked: pageStack.push(Qt.resolvedUrl("EditFeedPage.qml"), {
                        model: page.model,
                        row: index,
                        feedTitle: title,
                        categoryId: categoryId,
                        crawler: crawler,
                        feedDisabled: feedDisabled,
                        hideGlobally: hideGlobally
                    })
                }
                MenuItem {
                    text: qsTr("Mark feed as read")
                    onClicked: page.model.markFeedRead(index)
                }
                MenuItem {
                    text: qsTr("Unsubscribe")
                    onClicked: remorseAction(qsTr("Unsubscribing"), function() {
                        page.model.unsubscribe(index)
                    })
                }
            }
        }

        VerticalScrollDecorator {}
    }
}
