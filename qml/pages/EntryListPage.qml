import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

Page {
    id: page

    property var model
    property var feedModel
    /// Fixed translated label. Safe for PageHeader.
    property string scopeLabel: qsTr("Unread")
    /// Feed/category name when browsing one. FOREIGN TEXT: only via explicit
    /// Text.PlainText Label, never PageHeader.
    property string title: ""
    /// 0 unread, 1 starred, 2 all, 3 feed, 4 category. See models::Scope.
    property int scopeKind: 0
    property int scopeId: 0

    /// Strip scopes, strip order. Index-to-kind mapping lives HERE only: strip
    /// `currentIndex` DERIVED from `scopeKind`, not stored; highlighted tab
    /// can't drift from model.
    property var scopeTabKinds: [0, 1, 2]
    /// Selection mode: pushed over tab, on tab's own model, reader picks rows
    /// and acts. See EntryListView. No strip (one list; sideways swipe =
    /// leaving selection), no re-scoping (tab model already scoped).
    property bool selecting: false

    /// Feed/category pages pushed with name: keep PageHeader, no strip.
    /// `indexOf` -1 = that case; one expression drives strip and visibility.
    property bool showScopeTabs: !page.selecting
                                 && page.scopeTabKinds.indexOf(page.scopeKind) >= 0

    /// One EntryModel per tab, `scopeTabKinds` order. Empty for feed/category
    /// view (uses `model`).
    property var scopeModels: []

    /// Model pushed feed/category view scopes to itself.
    ///
    /// Passed on, not used here: to FeedListPage, which passes it back as
    /// `model` of feed page. NOT any of `scopeModels`: `setScope` overwrites,
    /// nothing restores; tab model would show feed rest of session.
    property var browseModel: null

    /// Model search opens into. Like `browseModel`, no tab shows it: search
    /// page re-scopes what it gets.
    property var searchModel: null

    /// Search mode: search field heads list, list shows matches across whole
    /// mirror. Entered by pushing EntryListPage with `searching`, scope 5, over
    /// `searchModel`.
    property bool searching: false

    /// Model whose refresh failures banner reports.
    ///
    /// NOT `currentModel`. Worker leaves failure in one-item slot; app poll
    /// drains it into first model polled, so only that model carries
    /// `syncError`, whatever page shows. Naming it lets feed view, Favourites,
    /// All report failed refresh.
    property var noticeModel: page.currentModel

    Component.onCompleted: page.applyScope()

    // Re-assert scope whenever page becomes visible.
    //
    // All BROWSE pages share `browseModel` (via FeedListPage); second feed
    // opened from first's pulley re-scopes object first shows. Scope only in
    // Component.onCompleted = stuck after back. Both hooks fire on first show
    // (scope applied twice) on purpose: `onCompleted` alone fails
    // back-navigation; `Activating` alone fails page never getting status
    // change. Cost: one extra query on open SQLite connection.
    //
    // Reload wanted anyway: marking read in article view changes mirror, not
    // these rows; without reload entry listed unread until next sync.
    onStatusChanged: if (status === PageStatus.Activating) page.applyScope()

    /// Scope browse model this feed/category view got.
    ///
    /// Tab scopes NOT set here: each tab model scoped once by owning list,
    /// never re-scoped (shared re-scoping can't keep neighbour tab populated).
    function applyScope() {
        // Not in selection mode either: tab model scoped once for app life;
        // `setScope` starts fresh list, drops read rows reader may be
        // selecting.
        if (page.model && !page.showScopeTabs && !page.selecting) {
            page.model.setScope(page.scopeKind, page.scopeId)
        }
    }

    /// Go to tab `index`, from strip tap.
    ///
    /// Strip = pager's sibling, not child of any list; tap reaches pager this
    /// way.
    function selectTab(index) {
        if (index === pager.currentIndex) {
            // Tap current tab = back to top.
            if (pager.currentItem) {
                pager.currentItem.scrollToTop()
            }
            return
        }
        pager.moveTo(index)
    }

    /// How far CURRENT tab scrolled past own top: positive = down, NEGATIVE
    /// while pulley pulled.
    ///
    /// `contentY - originY` off `currentItem` = Silica TabView
    /// (private/TabView.qml:51, private/TabItem.qml:47-49); strip's three
    /// behaviours below = its three uses.
    property real yOffset: pager.currentItem ? pager.currentItem.yOffset : 0

    /// Model page shows: visible tab's, or feed view's one model. Fallback for
    /// `noticeModel` (what banner reads).
    property var currentModel: page.showScopeTabs
                               ? (page.scopeModels[pager.currentIndex] || null)
                               : page.model

    allowedOrientations: Orientation.All

    /// Top band strip occupies.
    readonly property real stripBand: page.showScopeTabs ? scopeTabs.height : 0

    /*
     * The tab strip, pinned -- and the pulley menu still owns the top edge.
     *
     * The strip paints NOTHING behind its labels, and that is the whole point.
     * This window is transparent: the ambience is drawn in a separate
     * `WallpaperWindow` BEHIND the app (ApplicationWindow.qml:524), which is
     * why `_backgroundColor` is `#00000000` and why every attempt to give the
     * strip a matching fill produced a dark slab instead -- first
     * `Theme.overlayBackgroundColor` (`#000000`), then
     * `Sailfish.Silica.Background.ThemeBackground`, whose glass material has
     * no wallpaper to sample inside an app window and so rendered its
     * `_wallpaperOverlayColor` (`#99000000`) as a scrim. Both were reported
     * from the device as a black bar. A band that paints nothing at all is
     * the ambience, exactly, at zero cost.
     *
     * Which leaves only one thing to arrange: no ROW may be in the band
     * either. `viewport` below does that by clipping, and the two of them
     * together are what let the strip stay pinned without an opaque backing.
     *
     * `y` and `z` are Silica's, off `yOffset`: the strip rides down with the
     * pull (TabView.qml:72) and drops behind the pager while the pull is
     * happening (TabView.qml:71) so the opened menu paints over the tabs.
     *
     * The z gate is on `yOffset < 0` -- ACTIVELY BEING PULLED -- not on merely
     * sitting at the top. Gating it on "at the top" is what made every tab
     * untappable: at rest the strip was behind a full-page Flickable, input is
     * delivered in reverse paint order, and the Flickable took every press.
     */
    ScopeTabBar {
        id: scopeTabs

        anchors { left: parent.left; right: parent.right }
        y: Math.max(0, -page.yOffset)
        visible: page.showScopeTabs
        height: scopeTabs.visible ? scopeTabs.implicitHeight : 0
        z: page.yOffset < 0 ? -1 : 1

        hostPage: page
        titles: [qsTr("Unread"), qsTr("Favourites"), qsTr("All")]
        currentIndex: pager.currentIndex
        onTabClicked: page.selectTab(index)
    }

    /*
     * The tabs, side by side, swipeable.
     *
     * `PagedView` is public -- "Sailfish.Silica/PagedView 1.0" in
     * plugins.qmltypes -- and is the same class Silica's own TabView is built
     * on, so a swipe here has the system's own feel rather than a hand-rolled
     * drag threshold.
     *
     * The delegate deliberately does NOT touch `PagedView.isCurrentItem`.
     * That is an ATTACHED property, attached types cannot be written in a QML
     * stub, and using one would take this file and the whole entry list out of
     * `make qml-load`'s reach. `pager.currentIndex === index` says the same
     * thing with a plain binding.
     */
    /*
     * The window the tabs are seen through.
     *
     * Its rect is the page MINUS the strip's band, and it clips -- so a row
     * scrolled up into that band is cut at the strip's lower edge instead of
     * passing behind it. That is what lets the strip be transparent, and it
     * costs nothing at the top, where the list's own header already reserves
     * the band.
     *
     * The pager inside it is shifted back up by the same amount, so the
     * VIEWPORT still begins at the top of the SCREEN even though this item
     * does not. That matters: the pulley menu lives at negative content
     * coordinates, above `originY`, and both it and its resting indicator are
     * drawn relative to the top of the viewport. Anchoring the pager below the
     * strip instead -- which is what shipped last round -- is exactly what put
     * the menu under the tabs.
     *
     * Clipping is therefore switched OFF unless something is actually scrolled
     * past the top:
     *
     *   scrolled (yOffset > 0)  clip -- rows would otherwise enter the band
     *   at rest  (yOffset == 0) no clip -- the band holds the list's header
     *                           and nothing else, and the pulley's resting
     *                           indicator is drawn into the top 6px of the
     *                           viewport, which clipping would swallow
     *   pulling  (yOffset < 0)  no clip -- the menu comes down through the
     *                           band from the top edge of the screen
     *
     * Nothing is ever scrolled past the top at the moment the menu is being
     * pulled out, so no state needs both. Silica's TabItem makes the same
     * trade with the same reasoning, though it settles it once at construction
     * rather than per frame (private/TabItem.qml:63).
     *
     * Clipping also decides input, not just paint: Qt tests a clipping item's
     * own shape before descending into its children, so a tap in the band
     * cannot reach a row that has been clipped out of it.
     */
    Item {
        id: viewport

        x: 0
        width: page.width
        y: page.stripBand
        height: page.height - page.stripBand
        clip: page.yOffset > 0

        PagedView {
            id: pager

            // Shifted up by band: viewport starts at screen top though
            // `viewport` starts below strip.
            x: 0
            y: -page.stripBand
            width: viewport.width
            height: page.height

        // One page per tab, or exactly one without swiping for feed/category
        // view (pushed, left by back, not sideways swipe).
        model: page.showScopeTabs ? page.scopeTabKinds.length : 1
        interactive: page.showScopeTabs
        // Every tab alive and populated: swipe shows rows, not empty page
        // filling late.
        cacheSize: page.showScopeTabs ? page.scopeTabKinds.length : 1

        Component.onCompleted: {
            var index = page.scopeTabKinds.indexOf(page.scopeKind)
            if (page.showScopeTabs && index > 0) {
                pager.currentIndex = index
            }
        }

        // `currentIndex` driven imperatively, NOT bound.
        //
        // Swipe writes it from C++; written binding gone for good. Bound to
        // scope = works until first swipe, then tab taps stop moving pager.
        onCurrentIndexChanged: {
            if (page.showScopeTabs) {
                page.scopeKind = page.scopeTabKinds[pager.currentIndex]
            }
        }

        delegate: EntryListView {
            width: pager.width
            height: pager.height

            hostPage: page
            // Each tab own model, scoped once, never re-scoped.
            entryModel: page.showScopeTabs ? page.scopeModels[index] : page.model
            current: pager.currentIndex === index
            scopeKind: page.showScopeTabs ? page.scopeTabKinds[index] : page.scopeKind
            scopeId: page.showScopeTabs ? 0 : page.scopeId
            showScopeTabs: page.showScopeTabs
            tabIndex: page.showScopeTabs ? index : -1
            tabStripHeight: page.stripBand
            scopeLabel: page.scopeLabel
            title: page.title
            selecting: page.selecting
            searching: page.searching
            }
        }
    }

    // Docked, not in list header: notice mustn't move rows being read, visible
    // wherever scrolled. See components/NoticeBanner.qml.
    NoticeBanner {
        id: notice

        anchors.bottom: parent.bottom
        onActionTriggered: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
    }

    /// String changing exactly once per distinct failure.
    ///
    /// Posted from change handler, not bound to `syncError`: notice = event;
    /// binding re-posts on every re-evaluation, re-arms dismiss timer.
    /// `requestSync` clears both fields: retry gives empty token first, next
    /// failure real change even with same server text.
    property string _failureToken: page.noticeModel
                                  ? (page.noticeModel.syncErrorIsAuth
                                     ? "auth" : page.noticeModel.syncError)
                                  : ""

    on_FailureTokenChanged: {
        if (page._failureToken.length === 0) {
            notice.dismiss()
        } else if (page.noticeModel.syncErrorIsAuth) {
            notice.post(qsTr("The server rejected the API key."), true,
                        qsTr("Open settings"))
        } else {
            notice.post(qsTr("Refresh failed: %1").arg(page.noticeModel.syncError),
                        true, "")
        }
    }
}
