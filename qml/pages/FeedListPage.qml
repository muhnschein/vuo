import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    id: page
    property var model
    /// The shared entry model, so a per-feed view can be scoped without
    /// constructing a second one.
    property var entryModel

    allowedOrientations: Orientation.All

    SilicaListView {
        anchors.fill: parent
        model: page.model

        header: PageHeader { title: qsTr("Feeds") }

        PullDownMenu {
            MenuItem {
                text: qsTr("Add subscription")
                onClicked: pageStack.push(Qt.resolvedUrl("AddFeedPage.qml"),
                                          { model: page.model })
            }
        }

        ViewPlaceholder {
            enabled: page.model ? page.model.count === 0 : true
            text: qsTr("No feeds")
            hintText: qsTr("Subscribe from the pulley menu")
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
                        // FeedModel has exposed a per-feed unread count all
                        // along; this page never read it, so the list showed
                        // bare names and you had to open a feed to find out
                        // whether it had anything in it.
                        text: unreadCount > 0 ? unreadCount : ""
                        color: Theme.highlightColor
                        font.pixelSize: Theme.fontSizeSmall
                    }
                    Label {
                        id: feedTitle
                        anchors.left: parent.left
                        anchors.right: unreadBadge.left
                        anchors.rightMargin: unreadBadge.width > 0 ? Theme.paddingMedium : 0
                        // A feed name is chosen by the feed operator.
                        textFormat: Text.PlainText
                        text: title
                        truncationMode: TruncationMode.Fade
                        color: Theme.primaryColor
                    }
                }
                Label {
                    visible: errorMessage.length > 0
                    width: parent.width
                    // The server's own error text, relayed verbatim from a
                    // remote site. Plain text.
                    textFormat: Text.PlainText
                    text: errorMessage
                    wrapMode: Text.Wrap
                    maximumLineCount: 2
                    elide: Text.ElideRight
                    font.pixelSize: Theme.fontSizeExtraSmall
                    color: Theme.errorColor
                }
            }

            // The scope has to be passed as (kind, id) and the model has to
            // come with it: pushing `feedId` set a property that does not
            // exist and omitted `model` entirely, so the page opened empty.
            onClicked: pageStack.push(Qt.resolvedUrl("EntryListPage.qml"), {
                model: page.model ? page.entryModel : null,
                feedModel: page.model,
                scopeLabel: qsTr("Feed"),
                title: title,
                scopeKind: 3,
                scopeId: feedId
            })

            menu: ContextMenu {
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
