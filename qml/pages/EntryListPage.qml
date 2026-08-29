import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    id: page

    property var model
    property var feedModel
    /// A fixed, translated label. Safe for PageHeader.
    property string scopeLabel: qsTr("Unread")
    /// A feed or category name when browsing one. FOREIGN TEXT: rendered only
    /// through an explicit Text.PlainText Label, never through PageHeader.
    property string title: ""
    /// 0 unread, 1 starred, 2 all, 3 feed, 4 category. See models::Scope.
    property int scopeKind: 0
    property int scopeId: 0

    Component.onCompleted: if (page.model) page.model.setScope(page.scopeKind, page.scopeId)

    allowedOrientations: Orientation.All

    SilicaListView {
        id: listView
        anchors.fill: parent
        model: page.model

        // PageHeader's own title label offers no supported way to force its
        // textFormat, and `page.title` can be a FEED NAME -- foreign text
        // chosen by the feed operator. §9.3: a crafted title in a rich-text
        // context is markup injection, and can pull a remote image that leaks
        // the device's IP on a list scroll. So the header carries a fixed
        // string and the name is rendered by a Label this file controls.
        header: Column {
            width: listView.width

            PageHeader {
                title: page.scopeLabel
                description: page.model && page.model.count > 0
                             ? qsTr("%n article(s)", "", page.model.count)
                             : ""
            }

            Label {
                visible: page.title.length > 0
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                textFormat: Text.PlainText
                text: page.title
                wrapMode: Text.Wrap
                maximumLineCount: 2
                elide: Text.ElideRight
                font.pixelSize: Theme.fontSizeLarge
                color: Theme.highlightColor
            }
        }

        PullDownMenu {
            MenuItem {
                text: qsTr("Settings")
                onClicked: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
            }
            MenuItem {
                text: qsTr("Feeds")
                onClicked: pageStack.push(Qt.resolvedUrl("FeedListPage.qml"),
                                          { model: page.feedModel,
                                            entryModel: page.model })
            }
            MenuItem {
                text: qsTr("Mark all as read")
                onClicked: remorse.execute(qsTr("Marking all as read"), function() {
                    page.model.markAllRead()
                })
            }
            MenuItem {
                text: qsTr("Refresh")
                // Asks the worker for a network sync. `refresh()` alone only
                // re-reads the local mirror, so on its own the pulley menu
                // never actually talked to the server.
                onClicked: page.model.requestSync()
            }
        }

        RemorsePopup { id: remorse }

        // A refresh that reached the network has to look different from one
        // that did not. `syncing` had exactly one consumer -- the cover -- so
        // on the page itself a working Refresh and a Refresh that did nothing
        // were pixel-identical, which is how the latter went unnoticed.
        BusyIndicator {
            anchors.centerIn: parent
            size: BusyIndicatorSize.Large
            running: page.model ? page.model.syncing : false
        }

        ViewPlaceholder {
            // Not while syncing: "Nothing to read / Pull down to refresh"
            // under a running spinner reads as a refusal.
            enabled: listView.count === 0
                     && !(page.model && page.model.syncing)
            text: qsTr("Nothing to read")
            hintText: qsTr("Pull down to refresh")
        }

        delegate: ListItem {
            id: item
            contentHeight: column.height + Theme.paddingMedium * 2

            Column {
                id: column
                x: Theme.horizontalPageMargin
                y: Theme.paddingMedium
                width: parent.width - Theme.horizontalPageMargin * 2
                spacing: Theme.paddingSmall

                Label {
                    width: parent.width
                    // §9.3: a feed-supplied title is foreign data. PlainText,
                    // always, explicitly.
                    textFormat: Text.PlainText
                    text: title
                    wrapMode: Text.Wrap
                    maximumLineCount: 3
                    elide: Text.ElideRight
                    font.pixelSize: Theme.fontSizeMedium
                    color: unread ? Theme.primaryColor : Theme.secondaryColor
                }

                Row {
                    spacing: Theme.paddingMedium

                    Label {
                        // Author names are foreign too.
                        textFormat: Text.PlainText
                        text: author
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.secondaryColor
                        visible: author.length > 0
                    }
                    Label {
                        // Generated locally from an integer, so no foreign
                        // data reaches this one -- but it is still explicit,
                        // because a rule with exceptions is not checkable.
                        textFormat: Text.PlainText
                        text: readingTime > 0 ? qsTr("%n min", "", readingTime) : ""
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.secondaryColor
                    }
                    Label {
                        textFormat: Text.PlainText
                        text: "★"
                        color: Theme.highlightColor
                        font.pixelSize: Theme.fontSizeExtraSmall
                        visible: starred
                    }
                }
            }

            onClicked: pageStack.push(Qt.resolvedUrl("ArticlePage.qml"), {
                entryId: entryId,
                entryTitle: title
            })

            menu: ContextMenu {
                MenuItem {
                    text: unread ? qsTr("Mark as read") : qsTr("Mark as unread")
                    onClicked: page.model.setRead(index, unread)
                }
                MenuItem {
                    text: starred ? qsTr("Remove favourite") : qsTr("Add favourite")
                    onClicked: page.model.setStarred(index, !starred)
                }
            }
        }

        VerticalScrollDecorator {}
    }
}
