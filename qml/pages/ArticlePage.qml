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
                // openInBrowser RETURNS the URL rather than launching it: Rust
                // has no business knowing how this platform opens a browser.
                // Discarding the return value made the menu item do nothing.
                onClicked: {
                    var target = article.openInBrowser()
                    if (target.length > 0) {
                        Qt.openUrlExternally(target)
                    }
                }
            }
            MenuItem {
                text: qsTr("Fetch original content")
                onClicked: article.fetchOriginal()
            }
        }

        // NOT a Loader.
        //
        // A Loader's sourceComponent is instantiated in the scope where the
        // Component was DECLARED, not where the Loader sits, so the delegate's
        // model roles (blockKind, styledText, ...) are simply not visible
        // inside it. The article body rendered completely blank. Passing every
        // role through as a Loader property and reaching for it via `parent`
        // works but is a trap for the next person.
        //
        // A single delegate with one visible child per block kind keeps the
        // roles in scope, which is what a Qt 5.6-era Silica app would do
        // anyway: there are no required properties and no Controls 2 here.
        delegate: Item {
            id: block
            width: blocks.width
            height: content.height

            Column {
                id: content
                width: parent.width

                Label {
                    visible: blockKind === "heading"
                    height: visible ? implicitHeight + Theme.paddingLarge : 0
                    x: Theme.horizontalPageMargin + quoteDepth * Theme.paddingLarge
                    width: block.width - x - Theme.horizontalPageMargin
                    wrapMode: Text.Wrap
                    // Rust produced this markup and escaped every character of
                    // foreign text into it. StyledText is safe here, and only
                    // where the text came from that one function.
                    textFormat: Text.StyledText
                    text: styledText
                    color: Theme.highlightColor
                    font.pixelSize: level <= 2 ? Theme.fontSizeLarge : Theme.fontSizeMedium
                    font.bold: true
                }

                Label {
                    visible: blockKind === "paragraph"
                    height: visible ? implicitHeight + Theme.paddingMedium : 0
                    x: Theme.horizontalPageMargin + quoteDepth * Theme.paddingLarge
                    width: block.width - x - Theme.horizontalPageMargin
                    wrapMode: Text.Wrap
                    textFormat: Text.StyledText
                    text: styledText
                    color: quoteDepth > 0 ? Theme.secondaryColor : Theme.primaryColor
                    font.pixelSize: Theme.fontSizeSmall
                    linkColor: Theme.highlightColor
                    onLinkActivated: Qt.openUrlExternally(link)
                }

                Row {
                    visible: blockKind === "list_item"
                    height: visible ? itemText.implicitHeight + Theme.paddingSmall : 0
                    x: Theme.horizontalPageMargin + (quoteDepth + indent) * Theme.paddingLarge
                    width: block.width - x - Theme.horizontalPageMargin
                    spacing: Theme.paddingSmall

                    Label {
                        textFormat: Text.PlainText
                        text: marker
                        color: Theme.secondaryColor
                        font.pixelSize: Theme.fontSizeSmall
                    }
                    Label {
                        id: itemText
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

                Rectangle {
                    visible: blockKind === "code" || blockKind === "table"
                    height: visible ? codeText.implicitHeight + Theme.paddingMedium * 2 : 0
                    x: Theme.horizontalPageMargin
                    width: block.width - Theme.horizontalPageMargin * 2
                    color: Theme.rgba(Theme.highlightBackgroundColor, 0.1)
                    radius: Theme.paddingSmall

                    Label {
                        id: codeText
                        x: Theme.paddingMedium
                        y: Theme.paddingMedium
                        width: parent.width - Theme.paddingMedium * 2
                        // Verbatim by definition. Rendering code as markup
                        // would both corrupt it and reintroduce injection.
                        textFormat: Text.PlainText
                        text: styledText
                        wrapMode: Text.WrapAnywhere
                        font.family: "monospace"
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.primaryColor
                    }
                }

                Column {
                    visible: blockKind === "image"
                    height: visible ? implicitHeight : 0
                    x: Theme.horizontalPageMargin
                    width: block.width - Theme.horizontalPageMargin * 2
                    spacing: Theme.paddingSmall

                    // An un-proxied third-party image is NOT loaded. The
                    // placeholder names the host it would have contacted, so
                    // "load images" is an informed choice rather than a
                    // shrug (§9.3).
                    BackgroundItem {
                        visible: needsConsent
                        width: parent.width
                        height: visible ? Theme.itemSizeLarge : 0
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
                            // The host is foreign text.
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.fontSizeExtraSmall
                            color: Theme.secondaryHighlightColor
                            text: qsTr("Tap to load images from %1").arg(imageHost)
                        }
                    }

                    Image {
                        visible: !needsConsent
                        width: parent.width
                        fillMode: Image.PreserveAspectFit
                        asynchronous: true
                        // Capped so a hostile image cannot exhaust memory
                        // during decode; the URL was validated as http(s) in
                        // Rust before it ever reached QML.
                        sourceSize.width: block.width
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

                Separator {
                    visible: blockKind === "rule"
                    height: visible ? Theme.paddingLarge : 0
                    x: Theme.horizontalPageMargin
                    width: block.width - Theme.horizontalPageMargin * 2
                    color: Theme.secondaryColor
                    horizontalAlignment: Qt.AlignHCenter
                }
            }
        }

        VerticalScrollDecorator {}
    }
}
