import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0
import "../components"

/*
 * The article view.
 *
 * The body is a flat list of render blocks produced in Rust, not a rich-text
 * blob and not a WebView. Sailfish's Qt vintage supports only a subset of HTML
 * in Text, and a WebView is heavy and awkward inside a list; a block list also
 * gives lazy image loading and font scaling for free (§5).
 *
 * The article's own SITE is one swipe to the right, as an attached page (see
 * SitePage.qml). That is the one place a WebView is used, and it fetches
 * nothing until the reader actually goes there.
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

    /// Cleared when the reader takes charge of the read state themselves.
    ///
    /// Without this, marking an article unread and then re-opening it later
    /// would silently mark it read again — so "leave this for later" would
    /// survive exactly until the next tap on the row.
    property bool autoMarkArmed: false

    Component.onCompleted: {
        article.load(page.entryId)
        // Arm only for something that is actually unread. `markRead` is
        // idempotent anyway, but not arming keeps the intent honest.
        page.autoMarkArmed = article.markReadDelayMs >= 0 && !article.isRead
    }
    Component.onDestruction: article.clear()

    /// Whether the site page has been attached to the right. Once: the page
    /// becomes Active again every time the reader swipes back from the site,
    /// and attaching a second copy then would restart the site's load.
    property bool _siteAttached: false

    // The site, to the right, for an entry that has a link. Attached once
    // the page is Active, which is when a page is on the stack to attach to;
    // an entry without a link gets no forward indicator and nothing to swipe
    // to, rather than a page that says it has nothing to show.
    onStatusChanged: if (status === PageStatus.Active && !page._siteAttached
                         && article.articleUrl.length > 0) {
        page._siteAttached = true
        pageStack.pushAttached(Qt.resolvedUrl("SitePage.qml"), { url: article.articleUrl })
    }

    // Mark the article read once it has been open long enough.
    //
    // `running` is gated on the page being the visible one AND the app being
    // active, so the countdown measures the article being on screen rather
    // than the phone being in a pocket. It does not survive the screen
    // blanking while the app stays foregrounded — Silica gives QML no signal
    // for that, and the article is still "open" by any definition the app has.
    Timer {
        id: autoMarkRead
        interval: Math.max(1, article.markReadDelayMs)
        repeat: false
        running: page.autoMarkArmed
                 && article.markReadDelayMs >= 0
                 && page.status === PageStatus.Active
                 && Qt.application.active
        onTriggered: {
            if (page.autoMarkArmed) {
                article.markRead()
                page.autoMarkArmed = false
            }
        }
    }

    // The worker writes a scraped body into the mirror and bumps the signal;
    // without this the OPEN article never re-read it, which is why "Fetch
    // original content" looked like it did nothing. Only runs while the page
    // is showing.
    Timer {
        interval: 1000
        repeat: true
        running: page.status === PageStatus.Active
        onTriggered: article.pollSync()
    }

    SilicaListView {
        id: blocks
        anchors.fill: parent
        model: article

        // Keep delegates alive a little beyond the viewport.
        //
        // A ListView destroys delegates that scroll out of range and rebuilds
        // them on the way back, which for this page meant every image was
        // re-resolved and re-decoded each time it re-entered view -- so
        // scrolling back up through an article you had already read jumped
        // around exactly as it had on the way down.
        //
        // This was four screens, which is nine screens of live delegates once
        // both directions are counted, and a live image delegate holds a
        // DECODED pixmap -- several megabytes each at this width. An article
        // that is mostly pictures held tens of them at once. One screen either
        // side is enough now that a rebuilt delegate no longer jumps: the
        // `height` binding below reserves the right shape from the tag's own
        // ratio before any pixel arrives, and `cache: true` means the pixmap
        // usually comes back from Qt's cache rather than the network.
        cacheBuffer: Math.round(blocks.height)

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

            // What the article's own state is. Neither of these could be seen
            // anywhere in this view before: read/unread and starred lived only
            // in the entry list's context menu, so a reader who had opened an
            // article could not tell whether it was starred, let alone star it.
            Row {
                x: Theme.horizontalPageMargin
                spacing: Theme.paddingMedium

                Label {
                    text: article.isRead ? qsTr("Read") : qsTr("Unread")
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeExtraSmall
                    color: article.isRead ? Theme.secondaryColor : Theme.highlightColor
                }
                Label {
                    visible: article.isStarred
                    text: qsTr("★ Favourite")
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeExtraSmall
                    color: Theme.highlightColor
                }
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
                text: article.isRead ? qsTr("Mark as unread") : qsTr("Mark as read")
                // Disarm: once the reader has said what they want, a timer
                // must not overrule them a few seconds later.
                onClicked: {
                    page.autoMarkArmed = false
                    article.toggleRead()
                }
            }
            MenuItem {
                text: article.isStarred ? qsTr("Remove favourite")
                                        : qsTr("Add favourite")
                onClicked: article.toggleStarred()
            }
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
                onClicked: {
                    article.fetchOriginal()
                    // Say something the moment it is asked for. The scrape is
                    // a round trip to a server that then fetches a third-party
                    // page, which can take seconds; with no acknowledgement at
                    // all the menu item read as broken.
                    if (article.fetching) {
                        notice.post(qsTr("Asking the server for the original article…"),
                                    false, "")
                    }
                }
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
                        id: picture
                        visible: !needsConsent && status !== Image.Error
                        width: parent.width
                        // A definite height at ALL times, which this did not
                        // have. With only a width set, PreserveAspectFit
                        // leaves implicitHeight at 0 until the pixels arrive
                        // -- so every row was flat until its image decoded and
                        // then sprang to full size, re-flowing everything
                        // below it under the reader's thumb.
                        //
                        // Once loaded the real ratio is used. Before that the
                        // <img> tag's own width/height gives the right shape
                        // outright, and where the feed offered none, a square
                        // is reserved: wrong by some amount, but wrong by a
                        // BOUNDED amount and only once. The bound is real --
                        // `article.rs`'s MAX_IMAGE_RATIO clamps what the tag
                        // may claim, so a 1 x 20000 px `<img>` can no longer
                        // reserve a block taller than the article.
                        height: {
                            if (status === Image.Ready && implicitWidth > 0) {
                                return width * (implicitHeight / implicitWidth)
                            }
                            return width * (imageRatio > 0 ? imageRatio : 1)
                        }
                        fillMode: Image.PreserveAspectFit
                        asynchronous: true
                        cache: true
                        // BOTH dimensions, which is what actually bounds the
                        // decode. Qt only downscales a source that exceeds the
                        // size asked for, so a width cap alone left the height
                        // free: a 1000 x 30000 px image -- 30 megapixels, about
                        // 120 MB decoded -- came through the proxy at full
                        // resolution, and the comment here used to claim the
                        // opposite. With both set and PreserveAspectFit, Qt
                        // scales the source to fit INSIDE the box, so the
                        // decoded pixmap is bounded whatever shape arrives.
                        //
                        // Four screens tall is past anything meant to be read
                        // on a phone; beyond it the image is drawn upscaled and
                        // soft, which is the right way to lose that argument.
                        // The URL was validated as http(s) in Rust before it
                        // ever reached QML.
                        sourceSize.width: block.width
                        sourceSize.height: block.width * 4
                        source: needsConsent ? "" : imageSource
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

    NoticeBanner {
        id: notice
        anchors.bottom: parent.bottom
    }

    /// The `FETCH_*` constants from crates/vuo-shim/src/article.rs.
    ///
    /// Repeated here rather than imported: qmetaobject 0.2.10 on Qt 5.6 has no
    /// `qml_register_enum` (see lib.rs:28), so an integer is the only thing
    /// that crosses. Naming them here keeps the branch below readable and puts
    /// the one place they could drift out of step in plain sight.
    readonly property int fetchIdle: 0
    readonly property int fetchOk: 1
    readonly property int fetchEmpty: 2
    readonly property int fetchUnchanged: 3
    readonly property int fetchFailed: 4
    readonly property int fetchAuth: 5

    /// Mirrors the model's property, because a Page cannot declare a change
    /// handler for a property that lives on `article`.
    property int fetchStatus: article.fetchStatus

    /// Report a finished scrape, then acknowledge it.
    ///
    /// A change handler, not a binding: the scrape's result is an EVENT the
    /// user started, and clearing it here is what stops the same result being
    /// reported again the next time the property happens to be re-read.
    onFetchStatusChanged: {
        if (page.fetchStatus === page.fetchIdle) {
            return
        }
        if (page.fetchStatus === page.fetchOk) {
            notice.post(qsTr("Loaded the original article."), false, "")
        } else if (page.fetchStatus === page.fetchEmpty) {
            // The stored article is deliberately left alone in this case, so
            // say why nothing changed rather than letting it read as a no-op.
            notice.post(qsTr("The server could not extract the original article."),
                        true, "")
        } else if (page.fetchStatus === page.fetchUnchanged) {
            notice.post(qsTr("This feed already carries the full article."), false, "")
        } else if (page.fetchStatus === page.fetchAuth) {
            notice.post(qsTr("The server rejected the API key."), true, "")
        } else {
            // The server's own words, so PlainText -- which is what
            // NoticeBanner guarantees.
            notice.post(qsTr("Could not fetch the original article: %1")
                            .arg(article.fetchMessage), true, "")
        }
        article.clearFetchStatus()
    }
}
