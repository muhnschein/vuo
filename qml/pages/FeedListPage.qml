import QtQuick 2.6
import Sailfish.Silica 1.0

Page {
    id: page
    property var model

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

                Label {
                    width: parent.width
                    // A feed name is chosen by the feed operator.
                    textFormat: Text.PlainText
                    text: title
                    truncationMode: TruncationMode.Fade
                    color: Theme.primaryColor
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

            onClicked: pageStack.push(Qt.resolvedUrl("EntryListPage.qml"),
                                      { title: title, feedId: feedId })

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
