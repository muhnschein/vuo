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
    /// Which tab this list is. -1 when there is no strip.
    property int tabIndex: -1
    /// How much vertical room the page's pinned strip needs, gap included.
    property real tabStripHeight: 0

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
    /// How far this list has been pulled past its top, as a positive number.
    ///
    /// `contentY - originY` is the expression Silica's own TabItem uses
    /// (private/TabItem.qml:47-49). The page reads it off whichever list is in
    /// front, exactly as Silica reads `yOffset` off `currentItem`
    /// (private/TabView.qml:51).
    readonly property real pullDistance: listView.contentY < listView.originY
                                         ? listView.originY - listView.contentY
                                         : 0
    /// True while nothing has been scrolled past the top.
    readonly property bool atTop: listView.contentY <= listView.originY

    // An Item with explicit `y` bindings, NOT a Column.
    //
    // A Column here silently mislaid its children: measured on the device's
    // own Qt 5.6, a header Column reported height 12 while an item inside it
    // was 110 tall and still sitting at y 0. A child's height can arrive a
    // beat after the header is built, and a Qt 5.6 positioner inside a
    // ListView header does not re-position when that happens. The same Column
    // outside a header lays out correctly, which is what made this worth
    // writing down. Explicit `y` bindings re-evaluate.
    header: Item {
        id: head

        width: listView.width
        height: nameLabel.y + (nameLabel.visible ? nameLabel.height : 0)

        // Room for the tab strip, which is pinned by the PAGE rather than
        // scrolled with this list -- it must not slide away under a scroll,
        // and it must not travel sideways with a swipe. This reserves the
        // space it occupies, including the gap above it that the pulley's
        // resting indicator shows through.
        Item {
            id: stripSpace

            width: 1
            height: listView.showScopeTabs ? listView.tabStripHeight : 0
        }

        // Only for the scopes the strip does not cover.
        //
        // Retiring it there also drops a binding that was about to start
        // lying: `count` is rows HELD, and reload caps the query at 500, so an
        // "All" scope on any real mirror would have read "500 articles" for
        // ever.
        PageHeader {
            id: pageTitle

            y: stripSpace.height
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

    /// Measures the detail line, so it can be shortened before it overflows.
    FontMetrics {
        id: detailMetrics
        font.pixelSize: Theme.fontSizeExtraSmall
    }

    delegate: ListItem {
        id: item
        contentHeight: column.height + Theme.paddingMedium * 2

        /// The icon column's width, whether or not there IS an icon.
        ///
        /// A fixed gutter, so every row's text starts on the same vertical
        /// line: sizing it to the icon meant rows with an icon were indented
        /// and rows without were not, and a list mixing the two looked ragged.
        readonly property real gutter: Theme.fontSizeMedium + Theme.paddingSmall

        Row {
            id: column

            x: Theme.horizontalPageMargin
            y: Theme.paddingMedium
            width: parent.width - Theme.horizontalPageMargin * 2
            spacing: 0

            Item {
                id: iconGutter

                width: item.gutter
                height: 1

                Image {
                    // Aligned to the title's FIRST line, so a headline that
                    // wraps to three lines does not strand the icon in the
                    // middle of the block.
                    y: Math.round((titleLabel.font.pixelSize - height) / 2)
                    width: Theme.fontSizeMedium
                    height: Theme.fontSizeMedium
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
            }

            Column {
                width: parent.width - item.gutter
                spacing: Theme.paddingSmall

                Label {
                    id: titleLabel

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

                Label {
                    id: detailLabel

                    width: parent.width
                    // Assembled in JavaScript rather than as a Row of Labels
                    // so the separators collapse cleanly when a part is
                    // missing, and so the whole line can be shortened as one
                    // thing when it will not fit. PlainText, explicitly: the
                    // feed's name is in it and that is foreign text.
                    textFormat: Text.PlainText
                    font.pixelSize: Theme.fontSizeExtraSmall
                    color: item.highlighted ? Theme.secondaryHighlightColor
                                            : Theme.secondaryColor
                    truncationMode: TruncationMode.Fade

                    /// Build the line, shortening it a step at a time until
                    /// it fits.
                    ///
                    /// A long feed name or a long elapsed phrase -- both vary
                    /// by language -- used to run straight off the right edge.
                    ///
                    /// The order is the one asked for on the device: trim the
                    /// FEED NAME first, down to a length it is still
                    /// recognisable at, and only when that is not enough start
                    /// abbreviating the other fields. The name is what says
                    /// where an article came from, so losing its tail costs
                    /// less than losing a whole field.
                    function build() {
                        var date = published > 0 ? new Date(published * 1000) : null
                        var age = date ? Format.formatDate(date, Formatter.DurationElapsed) : ""
                        var shortAge = date ? Format.formatDate(date, Formatter.DurationElapsedShort)
                                            : ""
                        var readLong = readingTime > 0 ? qsTr("%n min read", "", readingTime) : ""
                        var readShort = readingTime > 0 ? qsTr("%n min", "", readingTime) : ""

                        // Each rung keeps the fields fuller than the one below
                        // it. Within a rung the name is trimmed as far as it
                        // will go before dropping to the next.
                        var rungs = [
                            [age, readLong],
                            [age, readShort],
                            [shortAge, readShort],
                            [shortAge, ""]
                        ]

                        // Short enough that a name is still identifiable, long
                        // enough that most feed names survive whole.
                        var minimumName = 14

                        for (var i = 0; i < rungs.length; ++i) {
                            var fitted = detailLabel.fitName(feedName, rungs[i], minimumName)
                            if (fitted !== null) {
                                return fitted
                            }
                        }
                        // Nothing fits even at the bottom rung with the
                        // shortest name; `truncationMode` fades what is left.
                        return detailLabel.join([feedName.substring(0, minimumName),
                                                 shortAge, ""])
                    }

                    /// The widest version of this rung whose name still fits,
                    /// or null when even the shortest name overflows it.
                    function fitName(name, rung, minimum) {
                        var candidate = detailLabel.join([name, rung[0], rung[1]])
                        if (detailMetrics.advanceWidth(candidate) <= detailLabel.width) {
                            return candidate
                        }
                        var trimmed = name
                        while (trimmed.length > minimum) {
                            trimmed = trimmed.substring(0, trimmed.length - 1)
                            candidate = detailLabel.join(
                                [trimmed + "\u2026", rung[0], rung[1]])
                            if (detailMetrics.advanceWidth(candidate) <= detailLabel.width) {
                                return candidate
                            }
                        }
                        return null
                    }

                    function join(parts) {
                        var kept = []
                        for (var i = 0; i < parts.length; ++i) {
                            if (parts[i] && parts[i].length > 0) {
                                kept.push(parts[i])
                            }
                        }
                        if (starred) {
                            kept.push("\u2605")
                        }
                        return kept.join("  \u00b7  ")
                    }

                    text: detailLabel.width > 0 ? detailLabel.build() : ""
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