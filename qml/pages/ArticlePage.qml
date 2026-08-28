import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0

/*
 * The article view.
 *
 * The body is a flat list of render blocks produced in Rust, not a rich-text
 * blob and not a WebView. Sailfish's Qt vintage supports only a subset of HTML
 * in Text, and a WebView is heavy and awkward inside a list; a block list also
 * gives lazy image loading and font scaling for free (§5).
 *
 * Note the delegate picks between LOCAL Components. It never assembles QML
 * from a string, and nothing derived from server data selects code to run
 * (§9.3).
 */
Page {
    id: page

    property int entryId: 0
    property string entryTitle: ""

    allowedOrientations: Orientation.All

    ArticleModel { id: article }

    Component.onCompleted: article.load(page.entryId)
    Component.onDestruction: article.clear()

    SilicaListView {
        id: blocks
        anchors.fill: parent
        model: article

        header: Column {
            width: blocks.width

            // The article's own title is foreign data, and PageHeader gives no
            // supported way to force its internal label's textFormat. So the
            // header carries a fixed string and the title is rendered by a
            // Label this file controls -- explicitly as PlainText (§9.3).
            PageHeader { title: qsTr("Article") }

            Label {
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                text: page.entryTitle
                font.pixelSize: Theme.fontSizeLarge
                color: Theme.highlightColor
            }

            Label {
                visible: article.blockedImages > 0
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                wrapMode: Text.Wrap
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryHighlightColor
                text: qsTr("%n image(s) are not proxied by your server and were not loaded.",
                           "", article.blockedImages)
            }
        }

        footer: Label {
            visible: article.truncated
            x: Theme.horizontalPageMargin
            width: blocks.width - Theme.horizontalPageMargin * 2
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            font.pixelSize: Theme.fontSizeExtraSmall
            color: Theme.secondaryHighlightColor
            // Saying so is the point: a silent truncation reads as "this is
            // the whole article" when it is not.
            text: qsTr("This article was too large to display in full.")
        }

        PullDownMenu {
            MenuItem {
                text: qsTr("Open in browser")
                onClicked: article.openInBrowser()
            }
            MenuItem {
                text: qsTr("Fetch original content")
                onClicked: article.fetchOriginal()
            }
        }

        delegate: Loader {
            width: blocks.width
            // `blockKind` is a fixed vocabulary produced by Rust, and this
            // maps it to one of a fixed set of local Components. A server
            // cannot introduce a new value that selects new code.
            sourceComponent: {
                switch (blockKind) {
                case "heading":   return headingBlock
                case "paragraph": return paragraphBlock
                case "list_item": return listItemBlock
                case "code":      return codeBlock
                case "image":     return imageBlock
                case "table":     return preformattedBlock
                case "rule":      return ruleBlock
                default:          return null
                }
            }
        }

        VerticalScrollDecorator {}
    }

    // ---- block delegates -------------------------------------------------

    Component {
        id: headingBlock
        Label {
            x: Theme.horizontalPageMargin + quoteDepth * Theme.paddingLarge
            width: page.width - x - Theme.horizontalPageMargin
            topPadding: Theme.paddingLarge
            wrapMode: Text.Wrap
            // Rust produced this markup and escaped every character of foreign
            // text into it. StyledText is safe here and ONLY here.
            textFormat: Text.StyledText
            text: styledText
            color: Theme.highlightColor
            font.pixelSize: level <= 2 ? Theme.fontSizeLarge : Theme.fontSizeMedium
            font.bold: true
        }
    }

    Component {
        id: paragraphBlock
        Label {
            x: Theme.horizontalPageMargin + quoteDepth * Theme.paddingLarge
            width: page.width - x - Theme.horizontalPageMargin
            topPadding: Theme.paddingMedium
            wrapMode: Text.Wrap
            textFormat: Text.StyledText
            text: styledText
            color: quoteDepth > 0 ? Theme.secondaryColor : Theme.primaryColor
            font.pixelSize: Theme.fontSizeSmall
            linkColor: Theme.highlightColor
            onLinkActivated: Qt.openUrlExternally(link)
        }
    }

    Component {
        id: listItemBlock
        Row {
            x: Theme.horizontalPageMargin + (quoteDepth + indent) * Theme.paddingLarge
            width: page.width - x - Theme.horizontalPageMargin
            spacing: Theme.paddingSmall
            topPadding: Theme.paddingSmall

            Label {
                textFormat: Text.PlainText
                text: marker
                color: Theme.secondaryColor
                font.pixelSize: Theme.fontSizeSmall
            }
            Label {
                width: parent.width - Theme.paddingLarge
                wrapMode: Text.Wrap
                textFormat: Text.StyledText
                text: styledText
                color: Theme.primaryColor
                font.pixelSize: Theme.fontSizeSmall
                linkColor: Theme.highlightColor
                onLinkActivated: Qt.openUrlExternally(link)
            }
        }
    }

    Component {
        id: codeBlock
        Rectangle {
            x: Theme.horizontalPageMargin
            width: page.width - Theme.horizontalPageMargin * 2
            height: codeText.height + Theme.paddingMedium * 2
            color: Theme.rgba(Theme.highlightBackgroundColor, 0.1)
            radius: Theme.paddingSmall

            Label {
                id: codeText
                x: Theme.paddingMedium
                y: Theme.paddingMedium
                width: parent.width - Theme.paddingMedium * 2
                // Code is verbatim by definition; rendering it as markup would
                // both corrupt it and reintroduce injection.
                textFormat: Text.PlainText
                text: styledText
                wrapMode: Text.WrapAnywhere
                font.family: "monospace"
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.primaryColor
            }
        }
    }

    Component {
        id: preformattedBlock
        Label {
            x: Theme.horizontalPageMargin
            width: page.width - Theme.horizontalPageMargin * 2
            topPadding: Theme.paddingMedium
            textFormat: Text.PlainText
            text: styledText
            wrapMode: Text.Wrap
            font.family: "monospace"
            font.pixelSize: Theme.fontSizeExtraSmall
            color: Theme.secondaryColor
        }
    }

    Component {
        id: imageBlock
        Column {
            x: Theme.horizontalPageMargin
            width: page.width - Theme.horizontalPageMargin * 2
            topPadding: Theme.paddingMedium
            spacing: Theme.paddingSmall

            // An un-proxied third-party image is NOT loaded. The placeholder
            // names the host and offers to load it; nothing is fetched until
            // the user says so (§9.3).
            BackgroundItem {
                visible: needsConsent
                width: parent.width
                height: Theme.itemSizeLarge
                onClicked: article.allowImagesFrom(index)

                Rectangle {
                    anchors.fill: parent
                    color: Theme.rgba(Theme.highlightBackgroundColor, 0.15)
                    radius: Theme.paddingSmall
                }
                Label {
                    anchors.centerIn: parent
                    width: parent.width - Theme.paddingLarge * 2
                    horizontalAlignment: Text.AlignHCenter
                    wrapMode: Text.Wrap
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeExtraSmall
                    color: Theme.secondaryHighlightColor
                    text: qsTr("Tap to load image (your server did not proxy it)")
                }
            }

            Image {
                visible: !needsConsent
                width: parent.width
                fillMode: Image.PreserveAspectFit
                asynchronous: true
                // Capped so a hostile image cannot exhaust memory during
                // decode; the source URL was already validated as http(s) in
                // Rust.
                sourceSize.width: page.width
                source: needsConsent ? "" : imageSource
                onStatusChanged: if (status === Image.Error) visible = false
            }

            Label {
                visible: imageAlt.length > 0
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                textFormat: Text.PlainText
                text: imageAlt
                wrapMode: Text.Wrap
                font.pixelSize: Theme.fontSizeExtraSmall
                color: Theme.secondaryColor
            }
        }
    }

    Component {
        id: ruleBlock
        Separator {
            x: Theme.horizontalPageMargin
            width: page.width - Theme.horizontalPageMargin * 2
            color: Theme.secondaryColor
            horizontalAlignment: Qt.AlignHCenter
        }
    }
}
