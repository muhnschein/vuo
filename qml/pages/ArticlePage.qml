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

    /// Cleared when reader sets read state themselves.
    ///
    /// Without it, marking unread then re-opening silently marks read again:
    /// "leave for later" lasts until next row tap.
    property bool autoMarkArmed: false

    Component.onCompleted: {
        article.load(page.entryId)
        // Arm only for unread. `markRead` idempotent anyway; not arming keeps
        // intent honest.
        page.autoMarkArmed = article.markReadDelayMs >= 0 && !article.isRead
    }
    Component.onDestruction: article.clear()

    /// Site page attached right yet. Once: page Active again on every swipe
    /// back from site; second copy restarts site load.
    property bool _siteAttached: false

    // Site to right for entry with link. Attached once page Active (on stack to
    // attach to). No link = no forward indicator, nothing to swipe to (not
    // "nothing to show" page).
    onStatusChanged: if (status === PageStatus.Active && !page._siteAttached
                         && article.articleUrl.length > 0) {
        page._siteAttached = true
        pageStack.pushAttached(Qt.resolvedUrl("SitePage.qml"), { url: article.articleUrl })
    }

    // Mark read once open long enough.
    //
    // `running` gated on page visible AND app active: counts article on screen,
    // not phone in pocket. Screen blank with app foreground doesn't stop it:
    // Silica gives QML no signal; article still "open".
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

    // Worker writes scraped body to mirror, leaves outcome on signal; without
    // this OPEN article never re-reads it ("Fetch original content" looks
    // dead).
    //
    // ONLY WHILE SCRAPE IN FLIGHT. Reading = app's longest state; polling while
    // page shown = wakeup per second for minutes. `Article::pollSync` does
    // nothing unless `take_fetch_outcome` has something for open entry;
    // `fetching` true exactly between request and outcome. No lost case, zero
    // cost when unused.
    Timer {
        interval: 1000
        repeat: true
        running: article.fetching
                 && page.status === PageStatus.Active
                 && Qt.application.active
        onTriggered: article.pollSync()
    }

    SilicaListView {
        id: blocks
        anchors.fill: parent
        model: article

        // Keep delegates alive slightly beyond viewport.
        //
        // ListView destroys out-of-range delegates, rebuilds on return: images
        // re-resolved, re-decoded; scrolling back jumped.
        //
        // Live image delegate holds DECODED pixmap (megabytes each at this
        // width); bigger buffer = tens held at once in picture-heavy article.
        // One screen each side enough: `height` binding below reserves shape
        // from tag ratio before pixels; `cache: true` usually brings pixmap
        // from Qt cache, not network.
        cacheBuffer: Math.round(blocks.height)

        header: Column {
            width: blocks.width

            // Article title foreign; PageHeader can't force internal label
            // textFormat. Header carries fixed string; title in own Label,
            // PlainText explicitly (§9.3).
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

            // Article's own state: read/unread and starred, visible and
            // settable from article, not only entry list context menu.
            Row {
                x: Theme.horizontalPageMargin
                spacing: Theme.paddingMedium

                Label {
                    text: article.isRead
                          //: The state of the one article on screen.
                          ? qsTr("Read")
                          //: The state of the one article on screen, so
                          //: singular: not the plural name of the Unread tab.
                          : qsTr("Unread")
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
            // Saying so = point: silent truncation reads as whole article.
            text: qsTr("This article was too large to display in full.")
        }

        PullDownMenu {
            MenuItem {
                text: article.isRead ? qsTr("Mark as unread") : qsTr("Mark as read")
                // Disarm: reader decided; timer mustn't overrule seconds later.
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
                // openInBrowser RETURNS URL, doesn't launch: Rust shouldn't
                // know how platform opens browser. Discarding return = dead
                // menu item.
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
                    // Acknowledge at once. Scrape = round trip to server that
                    // fetches third-party page, can take seconds; silence reads
                    // as broken.
                    if (article.fetching) {
                        notice.post(qsTr("Fetching the original article…"),
                                    false, "")
                    }
                }
            }
        }

        // NOT Loader.
        //
        // Loader's sourceComponent instantiated in scope where Component
        // DECLARED, not where Loader sits: delegate roles (blockKind,
        // styledText, ...) invisible inside → blank body. Passing roles as
        // Loader properties via `parent` works but traps next person.
        //
        // Single delegate, one visible child per block kind, keeps roles in
        // scope (Qt 5.6-era Silica: no required properties, no Controls 2).
        //
        // SO EVERY BLOCK BUILDS ALL OF THEM; `visible: false` only stops
        // DRAWING. `Text` still parses and lays out its `text` (implicit
        // sizing). Four Labels each built full StyledText layout over same
        // `styledText` per block: Jolla, long text-only article, 30 MB
        // resident, not freed on leaving page. Every `text` below gated on
        // block kind, not just hidden.
        //
        // EVERY `height` too, never on `visible`. `visible` = EFFECTIVE
        // visibility, false on page stack hid (any push over it, image viewer
        // included). Gated on `visible`, blocks collapse when viewer opens,
        // list loses content, reader returns to top. Kind doesn't change while
        // page hidden.
        delegate: Item {
            id: block
            width: blocks.width
            height: content.height

            Column {
                id: content
                width: parent.width

                Label {
                    visible: blockKind === "heading"
                    height: blockKind === "heading" ? implicitHeight + Theme.paddingLarge : 0
                    x: Theme.horizontalPageMargin + quoteDepth * Theme.paddingLarge
                    width: block.width - x - Theme.horizontalPageMargin
                    wrapMode: Text.Wrap
                    // Rust produced markup, escaped all foreign text.
                    // StyledText safe here, only for text from that one
                    // function.
                    textFormat: Text.StyledText
                    // Gated on kind, not `visible`. See note above delegate for
                    // both reasons.
                    text: blockKind === "heading" ? styledText : ""
                    color: Theme.highlightColor
                    font.pixelSize: level <= 2 ? Theme.fontSizeLarge : Theme.fontSizeMedium
                    font.bold: true
                }

                Label {
                    visible: blockKind === "paragraph"
                    height: blockKind === "paragraph" ? implicitHeight + Theme.paddingMedium : 0
                    x: Theme.horizontalPageMargin + quoteDepth * Theme.paddingLarge
                    width: block.width - x - Theme.horizontalPageMargin
                    wrapMode: Text.Wrap
                    textFormat: Text.StyledText
                    text: blockKind === "paragraph" ? styledText : ""
                    color: quoteDepth > 0 ? Theme.secondaryColor : Theme.primaryColor
                    font.pixelSize: Theme.fontSizeSmall
                    linkColor: Theme.highlightColor
                    onLinkActivated: Qt.openUrlExternally(link)
                }

                Row {
                    visible: blockKind === "list_item"
                    height: blockKind === "list_item" ? itemText.implicitHeight + Theme.paddingSmall : 0
                    x: Theme.horizontalPageMargin + (quoteDepth + indent) * Theme.paddingLarge
                    width: block.width - x - Theme.horizontalPageMargin
                    spacing: Theme.paddingSmall

                    Label {
                        textFormat: Text.PlainText
                        text: blockKind === "list_item" ? marker : ""
                        color: Theme.secondaryColor
                        font.pixelSize: Theme.fontSizeSmall
                    }
                    Label {
                        id: itemText
                        width: parent.width - Theme.paddingLarge
                        wrapMode: Text.Wrap
                        textFormat: Text.StyledText
                        text: blockKind === "list_item" ? styledText : ""
                        color: Theme.primaryColor
                        font.pixelSize: Theme.fontSizeSmall
                        linkColor: Theme.highlightColor
                        onLinkActivated: Qt.openUrlExternally(link)
                    }
                }

                Rectangle {
                    visible: blockKind === "code" || blockKind === "table"
                    height: blockKind === "code" || blockKind === "table"
                            ? codeText.implicitHeight + Theme.paddingMedium * 2 : 0
                    x: Theme.horizontalPageMargin
                    width: block.width - Theme.horizontalPageMargin * 2
                    color: Theme.rgba(Theme.highlightBackgroundColor, 0.1)
                    radius: Theme.paddingSmall

                    Label {
                        id: codeText
                        x: Theme.paddingMedium
                        y: Theme.paddingMedium
                        width: parent.width - Theme.paddingMedium * 2
                        // Verbatim by definition. As markup: corrupts code,
                        // reintroduces injection.
                        textFormat: Text.PlainText
                        text: blockKind === "code" || blockKind === "table"
                              ? styledText : ""
                        wrapMode: Text.WrapAnywhere
                        font.family: "monospace"
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.primaryColor
                    }
                }

                Column {
                    visible: blockKind === "image"
                    height: blockKind === "image" ? implicitHeight : 0
                    x: Theme.horizontalPageMargin
                    width: block.width - Theme.horizontalPageMargin * 2
                    spacing: Theme.paddingSmall

                    // Un-proxied third-party image NOT loaded. Placeholder
                    // names host it would contact: "load images" informed
                    // choice (§9.3).
                    BackgroundItem {
                        visible: needsConsent
                        width: parent.width
                        height: needsConsent ? Theme.itemSizeLarge : 0
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
                            // Host is foreign text.
                            textFormat: Text.PlainText
                            font.pixelSize: Theme.fontSizeExtraSmall
                            color: Theme.secondaryHighlightColor
                            text: needsConsent
                                  ? qsTr("Tap to load images from %1").arg(imageHost)
                                  : ""
                        }
                    }

                    Image {
                        id: picture
                        visible: !needsConsent && status !== Image.Error
                        width: parent.width
                        // Definite height ALWAYS. Width only +
                        // PreserveAspectFit = implicitHeight 0 until pixels;
                        // row flat until decode then springs, re-flowing below
                        // under thumb.
                        //
                        // Loaded: real ratio. Before: <img> tag width/height
                        // gives shape; none offered = square reserved, wrong by
                        // BOUNDED amount, once. Bound real: `article.rs`
                        // MAX_IMAGE_RATIO clamps tag claims; 1 x 20000 px
                        // `<img>` can't reserve block taller than article.
                        height: {
                            if (status === Image.Ready && implicitWidth > 0) {
                                return width * (implicitHeight / implicitWidth)
                            }
                            return width * (imageRatio > 0 ? imageRatio : 1)
                        }
                        fillMode: Image.PreserveAspectFit
                        asynchronous: true
                        cache: true
                        // BOTH dimensions, actually bounds decode. Qt only
                        // downscales source exceeding requested size; width cap
                        // alone leaves height free (1000 x 30000 px = 30 MP,
                        // ~120 MB decoded, at full res). Both +
                        // PreserveAspectFit: scaled to fit INSIDE box, pixmap
                        // bounded whatever shape.
                        //
                        // Four screens tall past anything readable on phone;
                        // beyond, drawn upscaled and soft (right way to lose).
                        // URL validated http(s) in Rust before QML.
                        sourceSize.width: block.width
                        sourceSize.height: block.width * 4
                        source: blockKind === "image" && !needsConsent
                                ? imageSource : ""

                        // Tap zooms, unless feed made image link: then tap
                        // follows link like linked word. Link validated http(s)
                        // in Rust.
                        MouseArea {
                            anchors.fill: parent
                            enabled: picture.status === Image.Ready
                            onClicked: {
                                if (imageLink.length > 0) {
                                    Qt.openUrlExternally(imageLink)
                                } else {
                                    // Page pushed over this can replace site
                                    // attached right; re-attach on return.
                                    // pushAttached replaces attached page,
                                    // never adds.
                                    page._siteAttached = false
                                    pageStack.push(Qt.resolvedUrl("ImageViewerPage.qml"),
                                                   { source: imageSource, alt: imageAlt,
                                                     previewWidth: block.width })
                                }
                            }
                        }
                    }

                    Label {
                        visible: blockKind === "image" && imageAlt.length > 0
                        width: parent.width
                        horizontalAlignment: Text.AlignHCenter
                        textFormat: Text.PlainText
                        text: blockKind === "image" ? imageAlt : ""
                        wrapMode: Text.Wrap
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.secondaryColor
                    }
                }

                Separator {
                    visible: blockKind === "rule"
                    height: blockKind === "rule" ? Theme.paddingLarge : 0
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

    /// `FETCH_*` constants from crates/vuo-shim/src/article.rs.
    ///
    /// Repeated, not imported: qmetaobject 0.2.10 on Qt 5.6 lacks
    /// `qml_register_enum` (see lib.rs:28); only integer crosses. Named here:
    /// branch readable, drift point visible.
    readonly property int fetchIdle: 0
    readonly property int fetchOk: 1
    readonly property int fetchEmpty: 2
    readonly property int fetchUnchanged: 3
    readonly property int fetchFailed: 4
    readonly property int fetchAuth: 5

    /// Mirrors model property: Page can't declare change handler for property
    /// on `article`.
    property int fetchStatus: article.fetchStatus

    /// Report finished scrape, then acknowledge.
    ///
    /// Change handler, not binding: result = user-started EVENT; clearing here
    /// stops re-report on next re-read.
    onFetchStatusChanged: {
        if (page.fetchStatus === page.fetchIdle) {
            return
        }
        if (page.fetchStatus === page.fetchOk) {
            notice.post(qsTr("The original article was loaded."), false, "")
        } else if (page.fetchStatus === page.fetchEmpty) {
            // Stored article left alone here on purpose; say why nothing
            // changed, not silent no-op.
            notice.post(qsTr("The server could not extract the original article."),
                        true, "")
        } else if (page.fetchStatus === page.fetchUnchanged) {
            notice.post(qsTr("This feed already contains the full article."), false, "")
        } else if (page.fetchStatus === page.fetchAuth) {
            notice.post(qsTr("The server rejected the API key."), true, "")
        } else {
            // Server's own words, so PlainText (NoticeBanner guarantees).
            notice.post(qsTr("Could not fetch the original article: %1")
                            .arg(article.fetchMessage), true, "")
        }
        article.clearFetchStatus()
    }
}
