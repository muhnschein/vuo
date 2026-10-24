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

    /// EntryListPage owning this list. Never assumed non-null: `make qml-load`
    /// instantiates file standalone.
    property Item hostPage: null
    /// Shared EntryModel, bound only while this tab current.
    property var entryModel
    /// Scope THIS tab shows, captured: action armed here can't resolve against
    /// tab user swiped to since.
    property int scopeKind: 0
    property int scopeId: 0
    /// True for tab PagedView settled on.
    property bool current: true
    property bool showScopeTabs: false
    /// Which tab this list is. -1 = no strip.
    property int tabIndex: -1
    /// Vertical room page's pinned strip needs. List is full-page (pulley comes
    /// from top of SCREEN) and reserves strip band in own header, so at rest
    /// band holds spacer, not row.
    property real tabStripHeight: 0

    /// How far list scrolled past own top: positive = down, NEGATIVE while
    /// pulley pulled.
    ///
    /// `contentY - originY` = Silica TabItem's expression
    /// (private/TabItem.qml:47-49). Page reads it off front list, like TabView
    /// reads `yOffset` off `currentItem` (private/TabView.qml:51), drives whole
    /// strip from it.
    readonly property real yOffset: listView.contentY - listView.originY

    Component.onCompleted: {
        // Scope tab's own model once. Feed/category view scoped by page
        // instead: scope is push parameter.
        if (listView.showScopeTabs && listView.entryModel) {
            listView.entryModel.setScope(listView.scopeKind, listView.scopeId)
        }
        // Search page opens on empty field, so not on last search's results
        // either: model outlives page.
        if (listView.searching && listView.entryModel) {
            listView.entryModel.setSearch("")
        }
        // No current row on search page. ListView gives focus to current row,
        // picks row 0 when rows arrive and it has none, unless told -1
        // explicitly (this). Search field in this view's header; without it
        // each typing pause (new results) steals focus, closes keyboard.
        if (listView.searching) {
            listView.currentIndex = -1
        }
    }

    /// True once user moved list themselves.
    property bool _moved: false
    onMovementStarted: listView._moved = true

    // Keep list at own top until user first moves it.
    //
    // Header reserves strip band; header height unknown at first layout (from
    // strip, from font metrics). ListView does NOT shift `contentY` when header
    // grows later, so list starts scrolled past top by band (real Silica:
    // `contentY` 0 vs `originY` -110; first article behind strip, pulley a
    // scroll away).
    //
    // Gated on `_moved`, not `moving`: only fixes start position. Later
    // `originY` change (e.g. orientation flip) must keep user's scroll.
    onOriginYChanged: if (!listView._moved) {
        listView.contentY = listView.originY
    }
    property string scopeLabel: ""
    property string title: ""

    /// Selection mode: tap selects row instead of opening, header counts
    /// selection, pulley offers actions. Entered by pushing EntryListPage with
    /// `selecting` over tab's OWN model (same rows reader saw). Swipe back
    /// cancels, as everywhere on platform; no Cancel item.
    property bool selecting: false
    /// Selected rows' entry ids, object used as set. REPLACED on every change,
    /// not mutated: delegate bindings re-evaluate (`var` holding same object
    /// announces nothing).
    property var selectedIds: ({})
    property int selectedCount: 0

    /// Search mode: header has search field; model (scope-5 EntryModel no tab
    /// shows) lists matches.
    property bool searching: false
    /// Search field contents. Passed to model shortly after typing stops
    /// (`searchDelay`), not per key: each search reads every article body in
    /// mirror.
    property string searchText: ""

    Timer {
        id: searchDelay
        interval: 300
        onTriggered: if (listView.entryModel) {
            listView.entryModel.setSearch(listView.searchText)
        }
    }
    onSearchTextChanged: searchDelay.restart()

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

    /// Select every row, or none.
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

    /// Mark selection read/unread, one write, leave.
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

    // Everything drawn stays in own bounds = whole page. Pulley lives at
    // negative content coords above `originY`; full-page lets it reveal from
    // top of SCREEN, not under tab strip.
    //
    // Keeping rows OUT of strip band = page's job: page clips them there only
    // while something scrolled past top. See `viewport` in EntryListPage.
    clip: true
    // Always bound, never re-bound on swipe.
    //
    // Each tab owns EntryModel, fixed to one scope for page life. Shared
    // re-scoped model can't be live while swiped towards (incoming tab blank
    // until swipe settles). Rows loaded and laid out before finger moves.
    model: listView.entryModel

    // List holds WINDOW onto mirror, not all; see models::PAGE_SIZE. Window end
    // asks next page.
    //
    // `atYEnd`, not delegate counting own index: delegate trigger fires
    // mid-scroll, during rebuild after reload, once per delegate. This fires on
    // transition; `loadMore` no-op when nothing more, so bottom bounce on fully
    // loaded list = one property read.
    onAtYEndChanged: if (listView.atYEnd && listView.entryModel
                         && listView.entryModel.hasMore) {
        listView.entryModel.loadMore()
    }

    // Item with explicit `y` bindings, NOT Column.
    //
    // Column here mislaid children silently: device Qt 5.6, header Column
    // height 12 while child 110 tall at y 0. Child height can arrive after
    // header built; Qt 5.6 positioner in ListView header doesn't re-position.
    // Same Column outside header fine. Explicit `y` bindings re-evaluate.
    header: Item {
        id: head

        width: listView.width
        height: searchField.y + (searchField.visible ? searchField.height : 0)

        // Room for tab strip, pinned by PAGE, not scrolled with list: mustn't
        // slide away on scroll or travel sideways on swipe. Reserves its band;
        // first row starts below.
        Item {
            id: stripSpace

            width: 1
            height: listView.tabStripHeight
        }

        // Only for scopes strip doesn't cover.
        //
        // Also drops lying binding: `count` = rows HELD, reload caps query at
        // 500, so "All" on real mirror reads "500 articles" forever.
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

        // PageHeader title label: no supported way to force textFormat;
        // `listView.title` can be FEED NAME (foreign, operator-chosen). §9.3:
        // crafted title in rich text = markup injection, remote image can leak
        // device IP on scroll. So header carries fixed string; name rendered by
        // this Label.
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

        SearchField {
            id: searchField
            objectName: "searchField"

            y: nameLabel.y + (nameLabel.visible ? nameLabel.height : 0)
            width: parent.width
            visible: listView.searching
            height: visible ? implicitHeight : 0
            //: Placeholder in the empty search field.
            placeholderText: qsTr("Search articles")
            // Predictive input holds word until committed; search would trail
            // one word behind screen.
            inputMethodHints: Qt.ImhNoPredictiveText
            onTextChanged: listView.searchText = searchField.text
            // Return = "done typing": search now without waiting pause, hide
            // keyboard so results get screen. Keys, not Silica EnterKey (as
            // AddFeedPage.qml): attached type QML can't stub.
            Keys.onReturnPressed: searchField.submit()
            Keys.onEnterPressed: searchField.submit()

            /// Keyboard raised for reader once yet.
            property bool _focused: false

            function submit() {
                searchDelay.stop()
                if (listView.entryModel) {
                    listView.entryModel.setSearch(listView.searchText)
                }
                searchField.focus = false
            }

            /// Raise keyboard once page in place and app in front (not mid
            /// slide-in), first time only: back from article lands on results,
            /// not keyboard. Both conditions: field focused while app in
            /// background raises nothing.
            function raiseKeyboard() {
                if (listView.searching && !searchField._focused && listView.hostPage
                        && listView.hostPage.status === PageStatus.Active
                        && Qt.application.active) {
                    searchField._focused = true
                    searchField.forceActiveFocus()
                }
            }

            Connections {
                target: listView.hostPage
                onStatusChanged: searchField.raiseKeyboard()
            }
            Connections {
                target: Qt.application
                onActiveChanged: searchField.raiseKeyboard()
            }
        }
    }

    PullDownMenu {
        id: pulley
        objectName: "pulley"

        // Not on search page: all items are about tab's list; menu would be
        // empty.
        visible: !listView.searching

        /// Set by Refresh, acted on once menu off screen.
        property bool refreshPending: false

        // Refresh resets model; reset re-lays out view this menu draws inside.
        // From tap, lands mid close animation, interrupts it (menu springs
        // open, refreshes, then lets list back). So tap ARMS refresh; this runs
        // it once menu gone.
        //
        // Only Refresh needs it. Others push page or open remorse popup;
        // neither touches this view.
        onActiveChanged: {
            if (!pulley.active && pulley.refreshPending) {
                pulley.refreshPending = false
                listView.entryModel.requestSync()
            }
        }

        // ------------------------------------------------ selection mode
        // Selection actions. Nothing destructive, no remorse: wrong read mark =
        // one tap to unread.
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
            visible: !listView.selecting && !listView.searching
            text: qsTr("Settings")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/SettingsPage.qml"))
        }
        MenuItem {
            visible: !listView.selecting && !listView.searching
            text: qsTr("Feeds")
            // `browseModel`, NOT this list's own: opening feed re-scopes model
            // it gets; tab's model would show feed rest of session.
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/FeedListPage.qml"),
                                      { model: listView.hostPage ? listView.hostPage.feedModel : null,
                                        entryModel: listView.hostPage ? listView.hostPage.browseModel : null,
                                        noticeModel: listView.hostPage ? listView.hostPage.noticeModel : null })
        }
        MenuItem {
            // Platform way to act on many rows (Gallery, Email): page of same
            // rows, tap selects, then pulley. Over tab's own model: exactly
            // rows reader saw.
            visible: !listView.selecting && !listView.searching && listView.count > 0
            text: qsTr("Select articles")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/EntryListPage.qml"), {
                model: listView.entryModel,
                feedModel: listView.hostPage ? listView.hostPage.feedModel : null,
                noticeModel: listView.hostPage ? listView.hostPage.noticeModel : null,
                scopeKind: listView.scopeKind,
                scopeId: listView.scopeId,
                scopeLabel: qsTr("Select articles"),
                selecting: true
            })
        }
        MenuItem {
            text: qsTr("Mark all as read")
            // Hidden on All. markAllRead non-feed branch expands scope with
            // i64::MAX limit over SELECT pulling full `content`, then one
            // outbox row per entry, no already-read filter. Unread: bounded by
            // unread count; All: every article ever synced. Mitigation, not
            // fix: Rust needs id-only projection + already-read filter before
            // All gets it.
            visible: !listView.selecting && !listView.searching
                     && listView.scopeKind !== 2
            onClicked: {
                // Capture scope NOW, not when countdown fires. `markAllRead`
                // reads shared model's scope at that moment; swipe re-scopes
                // without page change, so arm on Unread + swipe to All = every
                // article marked.
                var kind = listView.scopeKind
                var id = listView.scopeId
                var model = listView.entryModel
                remorse.execute(qsTr("Marking all as read"), function() {
                    model.markAllReadIn(kind, id)
                })
            }
        }
        MenuItem {
            // Whole mirror, main lists only. Search offered in feed reads as
            // searching that feed.
            visible: !listView.selecting && !listView.searching
                     && listView.showScopeTabs
            //: Pulley menu item. A verb: it opens the search page.
            text: qsTr("Search", "menu item")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/EntryListPage.qml"), {
                model: listView.hostPage ? listView.hostPage.searchModel : null,
                searchModel: listView.hostPage ? listView.hostPage.searchModel : null,
                browseModel: listView.hostPage ? listView.hostPage.browseModel : null,
                feedModel: listView.hostPage ? listView.hostPage.feedModel : null,
                noticeModel: listView.hostPage ? listView.hostPage.noticeModel : null,
                // models::Scope::Search
                scopeKind: 5,
                //: The search page's title. A noun.
                scopeLabel: qsTr("Search", "page title"),
                searching: true
            })
        }
        MenuItem {
            visible: !listView.selecting && !listView.searching
            text: qsTr("Refresh")
            // Armed here, run after menu closes: see `pulley.onActiveChanged`.
            // `requestSync` asks worker for network sync; `refresh()` only
            // re-reads local mirror.
            onClicked: pulley.refreshPending = true
        }
    }

    RemorsePopup { id: remorse }

    // Refresh that reached network must look different from one that didn't.
    // Without this, page shows working Refresh and no-op Refresh identically
    // (`syncing` only used by cover).
    BusyIndicator {
        anchors.centerIn: parent
        size: BusyIndicatorSize.Large
        running: listView.entryModel ? listView.entryModel.syncing : false
    }

    ViewPlaceholder {
        objectName: "placeholder"
        // Not while syncing: "Nothing to read / Pull down to refresh" under
        // spinner reads as refusal.
        //
        // Not before model READY (scoped + loaded). Gated on view count alone =
        // enabled at construction (all models empty), then fades over arriving
        // rows: "Nothing to read" flash every launch. Model count and `ready`
        // change together in one reload; no such moment.
        //
        // Conditional, not `&&` chain: no model (load test instantiates
        // standalone) → chain yields `undefined`, bool property refuses it.
        enabled: listView.entryModel
                 ? (listView.entryModel.ready
                    && listView.entryModel.count === 0
                    && !listView.entryModel.syncing)
                 : false
        // "Nothing to read / Pull down to refresh" = Unread string. Under
        // Favourites reads as refusal; refresh doesn't create favourites. Empty
        // search says only what is searched (hint below), under field reader
        // about to type in.
        text: listView.searching
              ? (listView.searchText.trim().length === 0 ? ""
                                                         : qsTr("No articles found"))
              : (listView.scopeKind === 1 ? qsTr("No favourites")
                                          : qsTr("Nothing to read"))
        hintText: listView.searching
                  // Said either way: only mirror searched; article on server
                  // but not device not found. No surprise.
                  ? qsTr("Vuo searches the title and text of every article on this device, and the names of your feeds.")
                  : (listView.scopeKind === 1
                     ? qsTr("Articles you add to favourites appear here")
                     : qsTr("Pull down to refresh"))
    }

    // Search results grouped by match place, titles first (model order). Each
    // group shows page of results, "Load more" under it (see delegate); header
    // says how many of how many.
    section.property: listView.searching ? "matchKind" : ""
    section.delegate: SectionHeader {
        objectName: "sectionHeader"
        text: listView.sectionTitle(section)
    }

    /// Group's result count in whole mirror.
    function groupTotal(kind) {
        if (!listView.entryModel) {
            return 0
        }
        if (kind === "title") {
            return listView.entryModel.titleMatches
        }
        return kind === "feed" ? listView.entryModel.feedMatches
                               : listView.entryModel.textMatches
    }

    /// How many list shows.
    function groupShown(kind) {
        if (!listView.entryModel) {
            return 0
        }
        if (kind === "title") {
            return listView.entryModel.titleShown
        }
        return kind === "feed" ? listView.entryModel.feedShown
                               : listView.entryModel.textShown
    }

    /// How many group's next "Load more" adds; 0 = loads rest.
    function groupNextLoad(kind) {
        if (!listView.entryModel) {
            return 0
        }
        if (kind === "title") {
            return listView.entryModel.titleNextLoad
        }
        return kind === "feed" ? listView.entryModel.feedNextLoad
                               : listView.entryModel.textNextLoad
    }

    /// "Load more" text: how many it adds (no more than left), or loads rest.
    function loadMoreText(kind) {
        var next = listView.groupNextLoad(kind)
        if (next <= 0) {
            //: Under a group of search results: shows all the rest of the
            //: group.
            return qsTr("Load all results")
        }
        var left = listView.groupTotal(kind) - listView.groupShown(kind)
        //: Under a group of search results: shows %n more of the group.
        return qsTr("Load %n more", "", Math.min(next, left))
    }

    /// Header over one search result group.
    function sectionTitle(kind) {
        var total = listView.groupTotal(kind)
        var shown = listView.groupShown(kind)
        if (shown < total) {
            if (kind === "title") {
                //: Search results: the group of articles whose title matched,
                //: when only some of them are shown. %1 is how many are
                //: shown, %2 how many there are.
                return qsTr("In titles (%1 of %2)").arg(shown).arg(total)
            }
            if (kind === "feed") {
                //: As "In titles (%1 of %2)", for the feeds whose name
                //: matched.
                return qsTr("Feeds (%1 of %2)").arg(shown).arg(total)
            }
            //: As "In titles (%1 of %2)", for the articles whose text matched.
            return qsTr("In article text (%1 of %2)").arg(shown).arg(total)
        }
        if (kind === "title") {
            //: Search results: the group of articles whose title matched.
            //: %1 is how many there are.
            return qsTr("In titles (%1)").arg(total)
        }
        if (kind === "feed") {
            //: Search results: the group of feeds whose name matched. %1 is
            //: how many there are.
            return qsTr("Feeds (%1)").arg(total)
        }
        //: Search results: the group of articles whose text matched.
        //: %1 is how many there are.
        return qsTr("In article text (%1)").arg(total)
    }

    /// Open feed search found, like tap in feed list: its articles over browse
    /// model; see FeedListPage.qml.
    function openFeed(feedId, name) {
        var host = listView.hostPage
        pageStack.push(Qt.resolvedUrl("../pages/EntryListPage.qml"), {
            model: host ? host.browseModel : null,
            browseModel: host ? host.browseModel : null,
            feedModel: host ? host.feedModel : null,
            noticeModel: host ? host.noticeModel : null,
            scopeLabel: qsTr("Feed"),
            title: name,
            // models::Scope::Feed
            scopeKind: 3,
            scopeId: feedId
        })
    }

    /// Search-marked StyledText from Rust; marks in theme highlight colour +
    /// bold (bold alone hard to see on faded read row). Text escaped in Rust:
    /// every `<b>` = Rust mark.
    function marked(styled) {
        return styled.replace(/<b>/g, "<b><font color=\"" + Theme.highlightColor + "\">")
                     .replace(/<\/b>/g, "</font></b>")
    }

    /// Escape text for StyledText label. Only text this file builds (age,
    /// reading time) next to Rust markup.
    function escaped(text) {
        return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    }

    /// Measures detail line; shorten before overflow.
    FontMetrics {
        id: detailMetrics
        font.pixelSize: Theme.fontSizeExtraSmall
    }

    /// Title line box; favicon centred on it.
    FontMetrics {
        id: titleMetrics
        font.pixelSize: Theme.fontSizeMedium
    }

    delegate: Item {
        id: row

        width: listView.width
        height: item.height + moreButton.height

        /// Row = FEED search found by name, not article: drawn like feed list,
        /// opens feed.
        readonly property bool isFeed: listView.searching && matchKind === "feed"

        /// Row = last of its search result group; group's "Load more" goes
        /// here.
        readonly property bool lastInGroup: listView.searching
                                            && row.ListView.nextSection !== row.ListView.section

        ListItem {
            id: item
            objectName: "listItem"
            width: parent.width
            contentHeight: column.height + Theme.paddingMedium * 2
            // Long-press menu = other way to act on one row; in selection mode
            // tap already is. Actions are article's; feed has none.
            showMenuOnPressAndHold: !listView.selecting && !row.isFeed

            /// Row in selection. Read off `selectedIds` (replaced per change,
            /// so re-evaluates).
            readonly property bool selected: listView.selecting
                                             && listView.selectedIds[entryId] === true

            // Silica selected-row mark: pressed highlight backing, kept on. No
            // checkbox column (would shift text sideways on entering mode).
            Rectangle {
                anchors.fill: parent
                visible: item.selected
                color: Theme.rgba(Theme.highlightBackgroundColor, Theme.highlightBackgroundOpacity)
            }

            /// Icon column width, icon or not.
            ///
            /// Fixed gutter: every row's text starts on same vertical line. Sizing
            /// to icon made mixed lists ragged.
            ///
            /// `paddingMedium` after icon, not `paddingSmall`: 6px vs 32px icon
            /// read as touching. 12px = next Silica step.
            readonly property real gutter: Theme.fontSizeMedium + Theme.paddingMedium

            Row {
                id: column

                x: Theme.horizontalPageMargin
                y: Theme.paddingMedium
                width: parent.width - Theme.horizontalPageMargin * 2
                spacing: 0
                // Read row steps back whole (icon, title, detail), not just title
                // colour (too subtle on device). Read rows stay until refresh;
                // telling them apart is list's job.
                opacity: unread ? 1.0 : Theme.opacityLow

                Item {
                    id: iconGutter

                    width: item.gutter
                    height: 1

                    Image {
                        // Centred on title's FIRST LINE BOX: three-line headline
                        // doesn't strand icon mid block.
                        //
                        // Line box taller than pixel size; glyphs sit low.
                        // `font.pixelSize`-based centring always gives y 0 (icon
                        // itself `fontSizeMedium` tall) = too high.
                        //
                        // SailSansPro-Light.ttf, fontSizeMedium (32), pixelRatio 1:
                        // ascent 31.49, descent 8.74, line box 40.22, capital ink
                        // 10.40..31.49, centre 20.94. Line box centring puts icon
                        // centre at 20.0, within pixel of ink (pixel-size centring:
                        // 16.0, 4.9px high, worse when pixel ratio > 1).
                        y: Math.round((titleMetrics.height - height) / 2)
                        width: Theme.fontSizeMedium
                        height: Theme.fontSizeMedium
                        sourceSize.width: Theme.fontSizeMedium
                        sourceSize.height: Theme.fontSizeMedium
                        fillMode: Image.PreserveAspectFit
                        // `data:` URI built in Rust from mirror bytes; no
                        // network fetch, scroll can't leak device IP (§9.3).
                        source: feedIcon
                        asynchronous: true
                        // Mirror stores icon in arrival format; device has
                        // handlers for only some. Collapse on failure: missing
                        // handler = missing icon, not broken-image glyph.
                        visible: feedIcon.length > 0 && status === Image.Ready
                    }
                }

                Column {
                    width: parent.width - item.gutter
                    spacing: Theme.paddingSmall

                    Label {
                        id: titleLabel
                        objectName: "titleLabel"

                        width: parent.width
                               - (feedUnread.visible ? feedUnread.width + Theme.paddingMedium : 0)
                        // §9.3: feed title foreign. PlainText explicitly, except
                        // search result (Rust escaped + marked terms): safe as
                        // StyledText.
                        textFormat: listView.searching ? Text.StyledText : Text.PlainText
                        text: listView.searching ? listView.marked(titleStyled) : title
                        wrapMode: Text.Wrap
                        maximumLineCount: 3
                        elide: Text.ElideRight
                        font.pixelSize: Theme.fontSizeMedium
                        color: unread ? Theme.primaryColor : Theme.secondaryColor
                    }

                    Label {
                        id: detailLabel
                        objectName: "detailLabel"

                        width: parent.width
                        // Feed row = name + count, like feed list; date and
                        // reading time aren't a feed's.
                        visible: !row.isFeed
                        // Built in JavaScript, not Row of Labels: separators
                        // collapse when part missing, whole line shortened as
                        // one. PlainText explicitly: feed name foreign. Except
                        // search result: name Rust-escaped and marked (see
                        // `styled`).
                        textFormat: listView.searching ? Text.StyledText : Text.PlainText
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: item.highlighted ? Theme.secondaryHighlightColor
                                                : Theme.secondaryColor
                        truncationMode: TruncationMode.Fade

                        /// Build line, shorten step by step until fits.
                        ///
                        /// Long feed name or elapsed phrase (both vary by
                        /// language) would run off right edge.
                        ///
                        /// Order asked for on device: trim FEED NAME first, down
                        /// to still-recognisable length; only then abbreviate
                        /// other fields. Name says where article came from; losing
                        /// its tail cheaper than losing whole field.
                        function build() {
                            var date = published > 0 ? new Date(published * 1000) : null
                            var age = date ? Format.formatDate(date, Formatter.DurationElapsed) : ""
                            var shortAge = date ? Format.formatDate(date, Formatter.DurationElapsedShort)
                                                : ""
                            var readLong = readingTime > 0 ? qsTr("%n min read", "", readingTime) : ""
                            var readShort = readingTime > 0 ? qsTr("%n min", "", readingTime) : ""

                            // Each rung keeps fields fuller than one below. Within
                            // rung, name trimmed fully before next rung.
                            var rungs = [
                                [age, readLong],
                                [age, readShort],
                                [shortAge, readShort],
                                [shortAge, ""]
                            ]

                            // Short enough name still identifiable, long enough
                            // most names survive whole.
                            var minimumName = 14

                            for (var i = 0; i < rungs.length; ++i) {
                                var fitted = detailLabel.fitName(feedName, rungs[i], minimumName)
                                if (fitted !== null) {
                                    return fitted
                                }
                            }
                            // Nothing fits even at bottom rung with shortest
                            // name; `truncationMode` fades rest.
                            return detailLabel.join([feedName.substring(0, minimumName),
                                                     shortAge, ""])
                        }

                        /// Widest version of rung whose name fits, or null if
                        /// shortest name overflows.
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

                        /// Fitted line as StyledText for search result: feed name
                        /// with terms bold if name not shortened (shortening could
                        /// cut bold run, so plain then); rest escaped.
                        function styled(line) {
                            if (feedName.length > 0 && line.indexOf(feedName) === 0) {
                                return listView.marked(feedNameStyled)
                                       + listView.escaped(line.substring(feedName.length))
                            }
                            return listView.escaped(line)
                        }

                        text: detailLabel.width <= 0 ? ""
                              : (listView.searching ? detailLabel.styled(detailLabel.build())
                                                    : detailLabel.build())
                    }

                    Label {
                        // Result found in article text: lines around match, shows
                        // why it's here. StyledText built and escaped in Rust
                        // (§9.3), terms bold.
                        objectName: "excerptLabel"
                        width: parent.width
                        visible: listView.searching && excerpt.length > 0
                        textFormat: Text.StyledText
                        text: listView.searching ? listView.marked(excerpt) : ""
                        wrapMode: Text.Wrap
                        maximumLineCount: 3
                        elide: Text.ElideRight
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: item.highlighted ? Theme.secondaryHighlightColor
                                                : Theme.secondaryColor
                    }
                }
            }

            // Feed unread count where feed list puts it: end of name's first
            // line.
            Label {
                id: feedUnread
                objectName: "feedUnread"

                visible: row.isFeed && unreadCount > 0
                anchors.right: parent.right
                anchors.rightMargin: Theme.horizontalPageMargin
                y: column.y + Math.round((titleMetrics.height - height) / 2)
                text: visible ? unreadCount : ""
                color: Theme.highlightColor
                font.pixelSize: Theme.fontSizeSmall
            }

            onClicked: {
                if (row.isFeed) {
                    listView.openFeed(feedId, title)
                } else if (listView.selecting) {
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
                    // Hidden, not disabled: entry without link rare; permanently
                    // dead row reads as app bug, not feed gap.
                    visible: url.length > 0
                    onClicked: Qt.openUrlExternally(url)
                }
            }
        }

        // Under last row of group with more than shown: inserts more under row
        // (ten, then twenty, then rest). See models::SEARCH_LOAD_STEPS.
        BackgroundItem {
            id: moreButton
            objectName: "loadMore"

            y: item.height
            width: parent.width
            visible: row.lastInGroup
                     && listView.groupShown(matchKind) < listView.groupTotal(matchKind)
            height: visible ? Theme.itemSizeSmall : 0

            Label {
                objectName: "loadMoreLabel"
                anchors.centerIn: parent
                text: listView.loadMoreText(matchKind)
                color: moreButton.highlighted ? Theme.highlightColor : Theme.primaryColor
            }

            onClicked: listView.entryModel.loadMoreIn(matchKind)
        }
    }

    VerticalScrollDecorator {}
}