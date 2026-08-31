import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

Page {
    id: page

    property var model
    property var feedModel
    /// A fixed, translated label. Safe for PageHeader.
    property string scopeLabel: qsTr("Unread")
    /// A feed or category name when browsing one. FOREIGN TEXT: rendered only
    /// through an explicit Text.PlainText Label, never through PageHeader.
    property string title: ""
    /// 0 unread, 1 starred, 2 all, 3 feed, 4 category. See models::Scope.
    property int scopeKind: 0
    property int scopeId: 0

    /// The scopes the tab strip offers, in strip order. The index-to-kind
    /// mapping lives HERE and nowhere else: the strip's `currentIndex` is
    /// DERIVED from `scopeKind` rather than stored beside it, so the
    /// highlighted tab cannot drift out of step with what the model shows.
    property var scopeTabKinds: [0, 1, 2]
    /// Feed and category pages are pushed with a name to show, so they keep
    /// the PageHeader and get no strip. `indexOf` returning -1 is exactly that
    /// case, so one expression drives both the strip and its visibility.
    property bool showScopeTabs: page.scopeTabKinds.indexOf(page.scopeKind) >= 0

    /// One EntryModel per tab, in `scopeTabKinds` order. Empty for a feed or
    /// category view, which uses `model` instead.
    property var scopeModels: []

    Component.onCompleted: page.applyScope()

    // Re-assert this page's scope whenever it becomes the visible one.
    //
    // Every list page shares ONE EntryModel -- FeedListPage hands its own
    // `entryModel` straight to the page it pushes -- so opening a feed
    // re-scopes the very object the Unread page is showing. Setting the scope
    // only in Component.onCompleted meant that scope then stuck: coming back
    // from a feed left the Unread page listing that feed's entries under an
    // "Unread" header, and no amount of navigating fixed it because no page
    // ever set the scope again. Restarting the app was the only way out.
    // Both hooks fire when a page is first shown, so the scope is applied
    // twice there. That is deliberate: `onCompleted` alone cannot survive the
    // back-navigation above, and relying on `Activating` alone would leave a
    // page that somehow never got a status change showing nothing at all. The
    // cost is one extra query against an already-open SQLite connection.
    //
    // The reload it causes is wanted for its own sake, too: marking an article
    // read from the article view changes the mirror but not this page's rows,
    // so without a reload on the way back the entry would still be listed as
    // unread until the next sync.
    onStatusChanged: if (status === PageStatus.Activating) page.applyScope()

    /// Scope the single model a feed or category view uses.
    ///
    /// The tab scopes are NOT set here: each tab's model is scoped once, by
    /// the list that owns it, and never re-scoped. Re-scoping a shared model
    /// was what made a neighbouring tab impossible to keep populated.
    function applyScope() {
        if (page.model && !page.showScopeTabs) {
            page.model.setScope(page.scopeKind, page.scopeId)
        }
    }

    /// Move to the tab at `index`, from a tap on the strip.
    ///
    /// The strip is a sibling of the pager, not a child of any one list, so
    /// this is how a tap on it reaches the pager that owns them all.
    function selectTab(index) {
        if (index === pager.currentIndex) {
            // Tapping the tab you are already on goes back to the top.
            if (pager.currentItem) {
                pager.currentItem.scrollToTop()
            }
            return
        }
        pager.moveTo(index)
    }

    /// How far the CURRENT tab is scrolled past its own top: positive when
    /// scrolled down, NEGATIVE while the pulley is being pulled out.
    ///
    /// `contentY - originY` read off `currentItem` is exactly what Silica's
    /// TabView does (private/TabView.qml:51, private/TabItem.qml:47-49), and
    /// all three of the strip's behaviours below are its three uses of it.
    property real yOffset: pager.currentItem ? pager.currentItem.yOffset : 0

    /// The model the page-level furniture (the notice banner) speaks for.
    property var currentModel: page.showScopeTabs
                               ? (page.scopeModels[pager.currentIndex] || null)
                               : page.model

    allowedOrientations: Orientation.All

    /// The band across the top of the page that the strip occupies.
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

            // Shifted back up by the band, so the viewport starts at the top
            // of the screen even though `viewport` starts below the strip.
            x: 0
            y: -page.stripBand
            width: viewport.width
            height: page.height

        // One page per tab -- or exactly one, with no swiping, for a feed or
        // category view, which is reached by pushing a page and left by going
        // back rather than by swiping sideways.
        model: page.showScopeTabs ? page.scopeTabKinds.length : 1
        interactive: page.showScopeTabs
        // Every tab stays alive and populated, so swiping towards one shows
        // its rows rather than an empty page that fills a moment later.
        cacheSize: page.showScopeTabs ? page.scopeTabKinds.length : 1

        Component.onCompleted: {
            var index = page.scopeTabKinds.indexOf(page.scopeKind)
            if (page.showScopeTabs && index > 0) {
                pager.currentIndex = index
            }
        }

        // `currentIndex` is driven imperatively, NOT bound.
        //
        // A swipe writes this property from C++, and a QML binding that is
        // written to is gone for good -- so binding it to the scope would work
        // exactly until the first swipe, after which tapping a tab would stop
        // moving the pager.
        onCurrentIndexChanged: {
            if (page.showScopeTabs) {
                page.scopeKind = page.scopeTabKinds[pager.currentIndex]
            }
        }

        delegate: EntryListView {
            width: pager.width
            height: pager.height

            hostPage: page
            // Each tab has its own model, scoped once and never re-scoped.
            entryModel: page.showScopeTabs ? page.scopeModels[index] : page.model
            current: pager.currentIndex === index
            scopeKind: page.showScopeTabs ? page.scopeTabKinds[index] : page.scopeKind
            scopeId: page.showScopeTabs ? 0 : page.scopeId
            showScopeTabs: page.showScopeTabs
            tabIndex: page.showScopeTabs ? index : -1
            tabStripHeight: page.stripBand
            scopeLabel: page.scopeLabel
            title: page.title
            }
        }
    }

    // Docked, not in a list's header: a notice must not move the rows the
    // user is reading, and it has to be visible wherever they have scrolled
    // to. See components/NoticeBanner.qml.
    NoticeBanner {
        id: notice

        anchors.bottom: parent.bottom
        onActionTriggered: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
    }

    /// One string that changes exactly once per distinct failure.
    ///
    /// The banner is posted from a change handler rather than bound to
    /// `syncError`, because a notice is an event: a binding would re-post the
    /// same failure on every re-evaluation, and re-arm its dismiss timer with
    /// it. `requestSync` clears both fields, so a retry produces an empty
    /// token first and the next failure is a genuine change even when the
    /// server says the same thing twice.
    property string _failureToken: page.currentModel
                                  ? (page.currentModel.syncErrorIsAuth
                                     ? "auth" : page.currentModel.syncError)
                                  : ""

    on_FailureTokenChanged: {
        if (page._failureToken.length === 0) {
            notice.dismiss()
        } else if (page.currentModel.syncErrorIsAuth) {
            notice.post(qsTr("The server rejected the API key."), true,
                        qsTr("Open settings"))
        } else {
            notice.post(qsTr("Refresh failed: %1").arg(page.currentModel.syncError),
                        true, "")
        }
    }
}
