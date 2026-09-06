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
    /// How much vertical room the page's pinned strip needs. This list is
    /// full-page -- so that the pulley menu comes down from the top of the
    /// SCREEN -- and reserves the strip's band in its own header instead, so
    /// that at rest the band holds this spacer rather than a row.
    property real tabStripHeight: 0

    /// How far this list is scrolled past its own top: positive scrolled down,
    /// NEGATIVE while the pulley is being pulled out.
    ///
    /// `contentY - originY` is the expression Silica's own TabItem uses
    /// (private/TabItem.qml:47-49). The page reads it off whichever list is in
    /// front, exactly as TabView reads `yOffset` off `currentItem`
    /// (private/TabView.qml:51), and drives the whole strip from it.
    readonly property real yOffset: listView.contentY - listView.originY

    // Scope this tab's own model, once. A feed or category view is scoped by
    // the page instead, because its scope is a parameter of the push.
    Component.onCompleted: if (listView.showScopeTabs && listView.entryModel) {
        listView.entryModel.setScope(listView.scopeKind, listView.scopeId)
    }

    /// True once the user has moved this list themselves.
    property bool _moved: false
    onMovementStarted: listView._moved = true

    // Keep the list at its own top until the user first moves it.
    //
    // The header reserves the strip's band, and a header's height is not known
    // at the first layout -- it comes from the strip, whose height comes from
    // font metrics. A ListView does NOT shift `contentY` when its header grows
    // afterwards, so the list came up scrolled past its own top by exactly the
    // band: measured on real Silica, `contentY` 0 against `originY` -110, with
    // the first article hidden behind the strip and the pulley a scroll away.
    //
    // Gated on `_moved` rather than on `moving`, so this can only ever fix the
    // starting position. A later `originY` change -- an orientation flip, say
    // -- must not throw away where the user had scrolled to.
    onOriginYChanged: if (!listView._moved) {
        listView.contentY = listView.originY
    }
    property string scopeLabel: ""
    property string title: ""

    /// Selection mode: a tap on a row selects it rather than opening it, the
    /// header counts the selection, and the pulley offers what to do with
    /// it. Entered by pushing an EntryListPage with `selecting` set over
    /// the tab's OWN model, so the rows are the ones the reader was just
    /// looking at; swiping back is how it is cancelled, as everywhere on
    /// the platform, so there is no Cancel item.
    property bool selecting: false
    /// The selected rows' entry ids, an object used as a set. REPLACED on
    /// every change rather than mutated, so the bindings that read it in
    /// the delegates are re-evaluated: a `var` holding the same object
    /// does not announce what happened inside it.
    property var selectedIds: ({})
    property int selectedCount: 0

    function toggleSelected(id) {
        var next = {}
        var n = 0
        for (var k in listView.selectedIds) {
            if (Number(k) !== id) {
                next[k] = true
                n++
            }
        }
        if (listView.selectedIds[id] !== true) {
            next[id] = true
            n++
        }
        listView.selectedIds = next
        listView.selectedCount = n
    }

    /// Select every row on the list, or none.
    function selectAll(on) {
        var next = {}
        var n = 0
        if (on && listView.entryModel) {
            for (var row = 0; row < listView.count; row++) {
                next[listView.entryModel.entryIdAt(row)] = true
                n++
            }
        }
        listView.selectedIds = next
        listView.selectedCount = n
    }

    /// Mark the selection read or unread, in one write, and leave.
    function applyRead(read) {
        var ids = []
        for (var k in listView.selectedIds) {
            ids.push(Number(k))
        }
        if (ids.length > 0 && listView.entryModel) {
            listView.entryModel.setReadMany(ids, read)
        }
        pageStack.pop()
    }

    // Everything this list draws stays inside its own bounds, which are the
    // whole page. The pulley menu lives at negative content coordinates,
    // above `originY`, so being full-page is what lets it be revealed from
    // the top of the SCREEN rather than from under the tab strip.
    //
    // Keeping rows OUT of the strip's band is a separate job, and not this
    // item's: the page clips them there, only while something is scrolled
    // past the top. See `viewport` in EntryListPage.
    clip: true
    // Always bound, never re-bound on a swipe.
    //
    // Each tab has its OWN EntryModel, fixed to one scope for the life of the
    // page. Sharing one re-scoped model meant a neighbour could not be live
    // while it was being swiped towards -- so the incoming tab arrived blank
    // and only filled once the swipe settled. Its rows are already loaded and
    // laid out before the finger moves now.
    model: listView.entryModel

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
        // band it occupies so the first row starts below it.
        Item {
            id: stripSpace

            width: 1
            height: listView.tabStripHeight
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
            description: listView.selecting
                         ? qsTr("%n selected", "", listView.selectedCount)
                         : (listView.entryModel && listView.entryModel.count > 0
                            ? qsTr("%n article(s)", "", listView.entryModel.count)
                            : "")
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
        // ------------------------------------------------ selection mode
        // What to do with the selection. Nothing here is destructive, so
        // no remorse: a row marked read by mistake is one tap from unread.
        MenuItem {
            visible: listView.selecting
            text: listView.selectedCount < listView.count ? qsTr("Select all")
                                                          : qsTr("Deselect all")
            onClicked: listView.selectAll(listView.selectedCount < listView.count)
        }
        MenuItem {
            visible: listView.selecting && listView.selectedCount > 0
            text: qsTr("Mark as unread")
            onClicked: listView.applyRead(false)
        }
        MenuItem {
            visible: listView.selecting && listView.selectedCount > 0
            text: qsTr("Mark as read")
            onClicked: listView.applyRead(true)
        }

        // ------------------------------------------------------ the list
        MenuItem {
            visible: !listView.selecting
            text: qsTr("Settings")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/SettingsPage.qml"))
        }
        MenuItem {
            visible: !listView.selecting
            text: qsTr("Feeds")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/FeedListPage.qml"),
                                      { model: listView.hostPage ? listView.hostPage.feedModel : null,
                                        entryModel: listView.entryModel })
        }
        MenuItem {
            // The platform's way to act on many rows at once, as the Gallery
            // and Email apps do it: a page of the same rows where a tap
            // selects, then the pulley. Over the tab's own model, so the
            // rows are exactly the ones the reader was looking at.
            visible: !listView.selecting && listView.count > 0
            text: qsTr("Select articles")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/EntryListPage.qml"), {
                model: listView.entryModel,
                feedModel: listView.hostPage ? listView.hostPage.feedModel : null,
                scopeKind: listView.scopeKind,
                scopeId: listView.scopeId,
                scopeLabel: qsTr("Select articles"),
                selecting: true
            })
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
            visible: !listView.selecting && listView.scopeKind !== 2
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
            visible: !listView.selecting
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
        //
        // And not before the model is READY -- scoped and loaded. Gated on
        // the view's count alone this was enabled at construction, when
        // every model still held nothing, and then faded out over the rows
        // that arrived a moment later: "Nothing to read" flashing across the
        // list at every launch. The model's own count and its `ready` flag
        // change together, in the one reload, so there is no such moment.
        //
        // A conditional rather than an `&&` chain: with no model -- this file
        // is instantiated standalone by the load test -- the chain yields
        // `undefined`, which a bool property refuses.
        enabled: listView.entryModel
                 ? (listView.entryModel.ready
                    && listView.entryModel.count === 0
                    && !listView.entryModel.syncing)
                 : false
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

    /// The title's line box, which is what the favicon is centred on.
    FontMetrics {
        id: titleMetrics
        font.pixelSize: Theme.fontSizeMedium
    }

    delegate: ListItem {
        id: item
        contentHeight: column.height + Theme.paddingMedium * 2
        // The long-press menu is the other way to act on one row, and in
        // selection mode the tap already is that.
        showMenuOnPressAndHold: !listView.selecting

        /// Whether this row is in the selection. Read off `selectedIds`,
        /// which is replaced on every change so this re-evaluates.
        readonly property bool selected: listView.selecting
                                         && listView.selectedIds[entryId] === true

        // Silica's own mark for a selected row: the highlight backing it
        // paints under a pressed one, kept on. No checkbox column, which
        // would shift every row's text sideways on entering the mode.
        Rectangle {
            anchors.fill: parent
            visible: item.selected
            color: Theme.rgba(Theme.highlightBackgroundColor, Theme.highlightBackgroundOpacity)
        }

        /// The icon column's width, whether or not there IS an icon.
        ///
        /// A fixed gutter, so every row's text starts on the same vertical
        /// line: sizing it to the icon meant rows with an icon were indented
        /// and rows without were not, and a list mixing the two looked ragged.
        ///
        /// `paddingMedium` after the icon, not `paddingSmall`: at 6px against a
        /// 32px icon the gap read as the icon touching the headline. 12px is
        /// the next step Silica offers.
        readonly property real gutter: Theme.fontSizeMedium + Theme.paddingMedium

        Row {
            id: column

            x: Theme.horizontalPageMargin
            y: Theme.paddingMedium
            width: parent.width - Theme.horizontalPageMargin * 2
            spacing: 0
            // A read row steps back as a whole -- icon, title and detail line
            // together -- rather than only swapping the title's colour, which
            // on the device was too little to tell the two apart at a glance.
            // Now that a read row stays on the list until the reader
            // refreshes, telling them apart is what the list is for.
            opacity: unread ? 1.0 : Theme.opacityLow

            Item {
                id: iconGutter

                width: item.gutter
                height: 1

                Image {
                    // Centred on the title's FIRST LINE BOX, so a headline
                    // that wraps to three lines does not strand the icon in
                    // the middle of the block.
                    //
                    // Measured against `font.pixelSize` this was always y 0 --
                    // the icon is itself `fontSizeMedium` tall, so the
                    // expression could only ever yield zero -- and zero is too
                    // high, which is what the device reported. A line box is
                    // taller than the pixel size, and the glyphs sit low in it.
                    //
                    // From SailSansPro-Light.ttf itself, at fontSizeMedium (32)
                    // and pixelRatio 1: ascent 31.49, descent 8.74, so the line
                    // box is 40.22 and the capital ink runs 10.40..31.49 with
                    // its centre at 20.94. The old y put the icon's centre at
                    // 16.0 -- 4.9px high, and more on a device whose pixel
                    // ratio is above 1. Centring on the line box puts it at
                    // 20.0, within a pixel of the ink.
                    y: Math.round((titleMetrics.height - height) / 2)
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

        onClicked: {
            if (listView.selecting) {
                listView.toggleSelected(entryId)
            } else {
                pageStack.push(Qt.resolvedUrl("../pages/ArticlePage.qml"), {
                    entryId: entryId,
                    entryTitle: title
                })
            }
        }

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