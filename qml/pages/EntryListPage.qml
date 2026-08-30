import QtQuick 2.6
import Sailfish.Silica 1.0
import Sailfish.Silica.Background 1.0 as Background
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

    /*
     * The tab strip, pinned -- and the pulley menu still owns the top edge.
     *
     * Three bindings, all of them Silica's own, and each one answering a
     * defect reported from the device.
     *
     * `y` rides down with the pull, `z` drops the strip behind the pager while
     * the pull is happening, and the backdrop opens a gap at its top edge
     * until the list is properly scrolled. Together they mean the pulley's
     * resting indicator shows above the tabs, and the opened menu comes down
     * over them from the top of the screen, as it does in every stock app.
     * TabView spells the same three out at private/TabView.qml:71-78.
     *
     * The z-flip is gated on `yOffset < 0` -- ACTIVELY BEING PULLED -- not on
     * merely sitting at the top. Gating it on "at the top" is what made every
     * tab untappable: at rest the strip was behind a full-page Flickable,
     * input is delivered in reverse paint order, and the Flickable took every
     * press.
     *
     * THE BACKDROP is why this arrangement is possible at all. A strip pinned
     * over a scrolling list has to be opaque or the rows read straight through
     * it, and it has to be opaque in the ambience's own background or it reads
     * as a slab -- the previous attempt filled it with
     * `Theme.overlayBackgroundColor` layered under
     * `__silica_applicationwindow_instance._backgroundColor`, which measures
     * on-device as #000000 under #00000000: a black band across the top of the
     * screen. The window colour is transparent because the wallpaper is not in
     * this window at all; ApplicationWindow puts it in a separate
     * `WallpaperWindow` behind (ApplicationWindow.qml:524) and the app window
     * is see-through to it.
     *
     * `Sailfish.Silica.Background.ThemeBackground` is the item that paints
     * that same ambience background, aligned to the screen rather than to
     * itself -- it takes `transformItem` from the window's own rotating item.
     * It is the public face of what Silica's `BackgroundRectangle` does for
     * TabView (private/TabView.qml:74-81), which is in Sailfish.Silica.private
     * and therefore not available here.
     *
     * On importing a module outside Sailfish.Silica: `Sailfish.Silica.Background`
     * is a normal module with its own qmldir, and ThemeBackground is a public
     * entry in it, not one of its `internal` lines. It is not on Harbour's
     * allowed-imports list, which is a cost this app can pay -- docs/scope.md
     * targets Chum and OpenRepos, and docs/packaging.md calls Harbour "a
     * stretch goal at best". `Sailfish.Silica.private` would NOT be acceptable
     * on the same terms: it is explicitly private, unversioned, and the
     * reasons in ScopeTabBar.qml's header still stand.
     */
    ScopeTabBar {
        id: scopeTabs

        anchors { left: parent.left; right: parent.right }
        // Rides down with the pull, so the menu coming down has room above it.
        // TabView.qml:72, which writes the same thing as `Math.max(0, -yOffset)`.
        y: Math.max(0, -page.yOffset)
        visible: page.showScopeTabs
        height: scopeTabs.visible ? scopeTabs.implicitHeight : 0
        // Behind the pager ONLY while the pull is actually happening, so the
        // menu paints over the tabs. TabView.qml:71. At every other moment the
        // strip is in front, which is what makes a tap reach it.
        z: page.yOffset < 0 ? -1 : 1

        hostPage: page
        titles: [qsTr("Unread"), qsTr("Favourites"), qsTr("All")]
        currentIndex: pager.currentIndex
        onTabClicked: page.selectTab(index)

        // The ambience's own background, so rows scrolling past cannot be read
        // through the tabs and the strip still looks like the wallpaper.
        //
        // The top edge stays open until the list is scrolled clear of it: the
        // pulley's resting indicator is drawn into the very top of the
        // viewport, and covering it would hide the one hint that there is a
        // menu. Silica opens exactly the same gap, of exactly this size, for
        // exactly this reason (TabView.qml:78).
        Background.ThemeBackground {
            anchors.fill: parent
            anchors.topMargin: page.yOffset > Theme.paddingSmall
                               ? 0 : Theme.paddingSmall
            z: -1
        }
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
    PagedView {
        id: pager

        // Full-page on purpose. The pulley menu lives at negative content
        // coordinates, above `originY`, and is revealed at the top of the
        // VIEWPORT -- so the viewport has to begin at the top of the SCREEN
        // for the menu to come down from the top edge, which is where
        // SailfishOS puts it. The strip is pinned over this and each list
        // reserves the space it occupies in its own header.
        anchors.fill: parent

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
            tabStripHeight: page.showScopeTabs ? scopeTabs.implicitHeight : 0
            scopeLabel: page.scopeLabel
            title: page.title
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
