import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One tab's worth of the entry list.
 *
 * Its own file because there are now THREE of these -- one per scope tab --
 * living inside a PagedView so the tabs can be swiped between. Each holds its
 * own scroll position and its own pulley menu, and each captures the scope it
 * was built for, so an action armed on one tab cannot be resolved against the
 * tab the user has since swiped to.
 */
SilicaListView {
    id: listView

    /// The EntryListPage this list belongs to. Never assumed non-null:
    /// `make qml-load` instantiates this file standalone.
    property Item hostPage: null
    /// The shared EntryModel, bound only while this tab is the current one.
    property var entryModel
    /// The scope THIS tab shows, captured so an action armed here cannot be
    /// resolved against whatever tab the user has swiped to since.
    property int scopeKind: 0
    property int scopeId: 0
    /// True for the tab PagedView is settled on.
    property bool current: true
    property bool showScopeTabs: false
    /// Which tab this list is, for the strip's highlight. -1 when there is no
    /// strip (a feed or category view).
    property int tabIndex: -1

    // Scope this tab's own model, once. A feed or category view is scoped by
    // the page instead, because its scope is a parameter of the push.
    Component.onCompleted: if (listView.showScopeTabs && listView.entryModel) {
        listView.entryModel.setScope(listView.scopeKind, listView.scopeId)
    }
    property string scopeLabel: ""
    property string title: ""

    // Clipped while scrolling, un-clipped while the pulley is out.
    //
    // Clipping is what keeps rows scrolled past the top from painting over
    // the strip -- Silica buys the same thing with an opaque background
    // filled from a colour that is not public API. But the pulley menu
    // lives in the flickable's content ABOVE the viewport, so clipping
    // also trims the menu at the strip's bottom edge, which is the other
    // half of the menu opening "underneath" the tabs. Silica's TabItem
    // makes exactly this trade the other way round and for the same
    // reason: `clip: !flickable.pullDownMenu` (private/TabItem.qml:63).
    // Gating on the pull means neither case is ever wrong: nothing is
    // scrolled past the top at the moment the menu is being pulled out.
    // Unconditional now. The list spans the whole page, so the pulley menu --
    // which lives at negative content coordinates, above `originY` -- is
    // revealed INSIDE these bounds rather than needing to paint outside them.
    clip: true
    // Always bound, never re-bound on a swipe.
    //
    // Each tab has its OWN EntryModel, fixed to one scope for the life of the
    // page. Sharing one re-scoped model meant a neighbour could not be live
    // while it was being swiped towards -- so the incoming tab arrived blank
    // and only filled once the swipe settled. Its rows are already loaded and
    // laid out before the finger moves now.
    model: listView.entryModel

    // PageHeader's own title label offers no supported way to force its
    // textFormat, and `listView.title` can be a FEED NAME -- foreign text
    // chosen by the feed operator. §9.3: a crafted title in a rich-text
    // context is markup injection, and can pull a remote image that leaks
    // the device's IP on a list scroll. So the header carries a fixed
    // string and the name is rendered by a Label this file controls.
    // An Item with explicit `y` bindings, NOT a Column.
    //
    // A Column here silently mislaid the tab strip: measured on the device's
    // own Qt 5.6, the header Column reported height 12 while the strip inside
    // it was 110 tall and still sitting at y 0. The strip's height arrives a
    // beat after the header is built (it depends on `showScopeTabs`, which is
    // a binding, and on font metrics), and a Qt 5.6 positioner inside a
    // ListView header does not re-position its children when that happens --
    // so the strip drew on top of the first row and the list's `originY` was
    // 12 instead of 122. The same Column outside a header lays out correctly,
    // which is what made this worth writing down. Explicit `y` bindings
    // re-evaluate whenever what they depend on changes, so the late height is
    // not a problem.
    header: Item {
        id: head

        width: listView.width
        height: nameLabel.y + (nameLabel.visible ? nameLabel.height : 0)

        // The pulley's resting indicator is drawn 6px into the top of the
        // VIEWPORT (measured: its HighlightBar sits at y 180 within a 244-tall
        // menu whose own y is -244). Anything that must appear BELOW that line
        // has to be inside the scrolled content rather than pinned outside it
        // -- which is why the strip is here and not a page-level sibling.
        // Pinning it outside put the indicator under the tabs and let an
        // opened pulley paint straight over them. This gap is what the
        // indicator shows through.
        property real indicatorGap: listView.showScopeTabs ? Theme.paddingMedium : 0

        ScopeTabBar {
            id: strip

            y: head.indicatorGap
            width: parent.width
            visible: listView.showScopeTabs
            height: visible ? implicitHeight : 0

            hostPage: listView.hostPage
            titles: [qsTr("Unread"), qsTr("Favourites"), qsTr("All")]
            currentIndex: listView.tabIndex
            onTabClicked: if (listView.hostPage) listView.hostPage.selectTab(index)
        }

        // Only for the scopes the strip does not cover.
        //
        // Retiring it there also drops a binding that was about to start
        // lying: `count` is rows HELD, and reload caps the query at 500, so an
        // "All" scope on any real mirror would have read "500 articles" for
        // ever.
        PageHeader {
            id: pageTitle

            y: strip.y + strip.height
            width: parent.width
            visible: !listView.showScopeTabs
            height: visible ? implicitHeight : 0
            title: listView.scopeLabel
            description: listView.entryModel && listView.entryModel.count > 0
                         ? qsTr("%n article(s)", "", listView.entryModel.count)
                         : ""
        }

        // PageHeader's own title label offers no supported way to force its
        // textFormat, and `listView.title` can be a FEED NAME -- foreign text
        // chosen by the feed operator. §9.3: a crafted title in a rich-text
        // context is markup injection, and can pull a remote image that leaks
        // the device's IP on a list scroll. So the header above carries a
        // fixed string and the name is rendered by this Label.
        Label {
            id: nameLabel

            y: pageTitle.y + pageTitle.height
            visible: listView.title.length > 0
            x: Theme.horizontalPageMargin
            width: parent.width - Theme.horizontalPageMargin * 2
            textFormat: Text.PlainText
            text: listView.title
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
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/SettingsPage.qml"))
        }
        MenuItem {
            text: qsTr("Feeds")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/FeedListPage.qml"),
                                      { model: listView.hostPage ? listView.hostPage.feedModel : null,
                                        entryModel: listView.entryModel })
        }
        MenuItem {
            text: qsTr("Mark all as read")
            // Hidden on All. markAllRead's non-feed branch expands the
            // scope with i64::MAX as the limit over a SELECT that pulls
            // the full `content` column, then queues one outbox row per
            // entry with no already-read filter. On Unread that set is
            // bounded by the unread count; on All it is every article ever
            // synced. A mitigation, not a fix -- the Rust wants an id-only
            // projection and an already-read filter before All gets it.
            visible: listView.scopeKind !== 2
            onClicked: {
                // Capture the scope NOW, not when the countdown fires.
                // `markAllRead` would read whatever scope the shared model is
                // in at that later moment, and swiping to another tab
                // re-scopes it without a page change -- so arming this on
                // Unread and swiping to All within the countdown would have
                // marked every article in the mirror.
                var kind = listView.scopeKind
                var id = listView.scopeId
                var model = listView.entryModel
                remorse.execute(qsTr("Marking all as read"), function() {
                    model.markAllReadIn(kind, id)
                })
            }
        }
        MenuItem {
            text: qsTr("Refresh")
            // Asks the worker for a network sync. `refresh()` alone only
            // re-reads the local mirror, so on its own the pulley menu
            // never actually talked to the server.
            onClicked: listView.entryModel.requestSync()
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
        running: listView.entryModel ? listView.entryModel.syncing : false
    }

    ViewPlaceholder {
        // Not while syncing: "Nothing to read / Pull down to refresh"
        // under a running spinner reads as a refusal.
        enabled: listView.count === 0
                 && !(listView.entryModel && listView.entryModel.syncing)
        // "Nothing to read / Pull down to refresh" is an Unread string.
        // Under a Favourites tab it reads as a refusal, and refreshing
        // does not create favourites.
        text: listView.scopeKind === 1 ? qsTr("No favourites")
                                   : qsTr("Nothing to read")
        hintText: listView.scopeKind === 1
                  ? qsTr("Star an article to keep it here")
                  : qsTr("Pull down to refresh")
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

            // Line 1: the feed's icon, then the headline.
            Row {
                width: parent.width
                spacing: Theme.paddingSmall

                Image {
                    id: favicon
                    // Sized to the headline and aligned to its FIRST line, so
                    // a title that wraps to three lines does not leave the
                    // icon stranded in the middle of the block.
                    // Zero-width when there is nothing to show, so the title
                    // reclaims the space instead of being indented past a gap.
                    width: favicon.visible ? Theme.fontSizeMedium : 0
                    height: Theme.fontSizeMedium
                    y: Math.round((titleLabel.font.pixelSize - height) / 2)
                    sourceSize.width: Theme.fontSizeMedium
                    sourceSize.height: Theme.fontSizeMedium
                    fillMode: Image.PreserveAspectFit
                    // A `data:` URI built in Rust from bytes already in the
                    // mirror -- no network fetch happens here, so a list
                    // scroll cannot leak the device's IP (§9.3).
                    source: feedIcon
                    asynchronous: true
                    // The mirror stores whatever format the icon arrived in
                    // and the device ships handlers for only some of them.
                    // Collapsing on failure keeps a missing handler to a
                    // missing icon rather than a broken-image glyph.
                    visible: feedIcon.length > 0 && status === Image.Ready
                }

                Label {
                    id: titleLabel
                    width: parent.width - (favicon.visible
                                           ? favicon.width + Theme.paddingSmall
                                           : 0)
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
            }

            // Line 2: feed | age | reading time | star, one size down.
            Label {
                width: parent.width
                // Every part of this is either foreign (the feed's name) or
                // generated locally, and it is assembled in JavaScript rather
                // than as a Row of Labels so the separators collapse cleanly
                // when a part is missing. PlainText, explicitly, because the
                // feed name is in it.
                textFormat: Text.PlainText
                font.pixelSize: Theme.fontSizeExtraSmall
                color: item.highlighted ? Theme.secondaryHighlightColor
                                        : Theme.secondaryColor
                truncationMode: TruncationMode.Fade
                text: {
                    var parts = []
                    if (feedName.length > 0) {
                        parts.push(feedName)
                    }
                    if (published > 0) {
                        // Silica's own elapsed formatter, so "vor 6 Stunden" is
                        // worded and localised exactly as the rest of the
                        // system words it. `published` is epoch SECONDS.
                        parts.push(Format.formatDate(new Date(published * 1000),
                                                     Formatter.DurationElapsed))
                    }
                    if (readingTime > 0) {
                        parts.push(qsTr("%n min read", "", readingTime))
                    }
                    if (starred) {
                        parts.push("\u2605")
                    }
                    return parts.join("  |  ")
                }
            }
        }

        onClicked: pageStack.push(Qt.resolvedUrl("../pages/ArticlePage.qml"), {
            entryId: entryId,
            entryTitle: title
        })

        menu: ContextMenu {
            MenuItem {
                text: unread ? qsTr("Mark as read") : qsTr("Mark as unread")
                onClicked: listView.entryModel.setRead(index, unread)
            }
            MenuItem {
                text: starred ? qsTr("Remove favourite") : qsTr("Add favourite")
                onClicked: listView.entryModel.setStarred(index, !starred)
            }
            MenuItem {
                text: qsTr("Open in browser")
                // Hidden rather than disabled: an entry with no link is rare
                // enough that a permanently dead row would read as a bug in
                // the app rather than a gap in the feed.
                visible: url.length > 0
                onClicked: Qt.openUrlExternally(url)
            }
        }
    }

    VerticalScrollDecorator {}
}