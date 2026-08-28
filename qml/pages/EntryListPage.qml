import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    id: page

    property var model
    property var feedModel
    property string title: qsTr("Unread")

    allowedOrientations: Orientation.All

    SilicaListView {
        id: listView
        anchors.fill: parent
        model: page.model

        header: PageHeader {
            title: page.title
            description: page.model && page.model.count > 0
                         ? qsTr("%n article(s)", "", page.model.count)
                         : ""
        }

        PullDownMenu {
            MenuItem {
                text: qsTr("Settings")
                onClicked: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
            }
            MenuItem {
                text: qsTr("Feeds")
                onClicked: pageStack.push(Qt.resolvedUrl("FeedListPage.qml"),
                                          { model: page.feedModel })
            }
            MenuItem {
                text: qsTr("Mark all as read")
                onClicked: remorse.execute(qsTr("Marking all as read"), function() {
                    page.model.markAllRead()
                })
            }
            MenuItem {
                text: qsTr("Refresh")
                onClicked: page.model.refresh()
            }
        }

        RemorsePopup { id: remorse }

        ViewPlaceholder {
            enabled: listView.count === 0
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
