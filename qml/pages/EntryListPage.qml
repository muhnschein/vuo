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
    /// The strip lives inside each list's header now, so this is how a tap in
    /// one tab reaches the pager that owns them all.
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

    /// The gap above the strip that the pulley's resting indicator shows
    /// through. Measured: the menu's HighlightBar pokes ~6px below the content
    /// top, so anything smaller hides it.
    readonly property real indicatorGap: Theme.paddingMedium

    /// How far the CURRENT tab has been pulled past its top, and whether it is
    /// still at the top at all. Read off `currentItem`, exactly as Silica's
    /// TabView reads its own `yOffset` (private/TabView.qml:51).
    property real pullDistance: pager.currentItem ? pager.currentItem.pullDistance : 0
    property bool atTop: pager.currentItem ? pager.currentItem.atTop : true

    /// The model the page-level furniture (the notice banner) speaks for.
    property var currentModel: page.showScopeTabs
                               ? (page.scopeModels[pager.currentIndex] || null)
                               : page.model

    allowedOrientations: Orientation.All

    /*
     * The tabs, side by side, swipeable.
     *
     * `PagedView` is public -- "Sailfish.Silica/PagedView 1.0" in
     * plugins.qmltypes -- and is the same class Silica's own TabView is built
     * on, so a swipe here has the system's own feel rather than a hand-rolled
     * drag threshold.
     *
     * Full-page on purpose. The pulley menu lives at negative content
     * coordinates, above `originY`, and its resting indicator is drawn into
     * the top of the VIEWPORT -- so the viewport has to start at the top of
     * the screen for that indicator to appear above the tab strip rather than
     * under it. The strip is part of each list's header for the same reason.
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
        // coordinates, above `originY`, and its resting indicator is drawn
        // into the top of the VIEWPORT -- so the viewport has to start at the
        // top of the screen for that indicator to appear ABOVE the strip
        // rather than under it. The strip is pinned on top of this, and each
        // list reserves the space it occupies in its own header.
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
            tabStripHeight: page.indicatorGap + scopeTabs.implicitHeight
            scopeLabel: page.scopeLabel
            title: page.title
        }
    }

    /*
     * The tab strip, pinned.
     *
     * Pinned by the PAGE rather than carried in each list's header, because a
     * header copy travels sideways with a swipe and scrolls away under a
     * scroll -- both reported from a device. Each list reserves its space
     * instead.
     *
     * The z-flip is what lets it be pinned AND sit below the pulley. While the
     * current list is at its top (which includes being pulled), the strip
     * drops behind the pager: the list has nothing to paint up here except the
     * pulley itself, so the indicator and the opened menu both show through.
     * The moment anything is scrolled past the top the strip comes forward,
     * opaque, and rows pass behind it instead of through it. Silica's own
     * TabView makes the same move for the same reason
     * (private/TabView.qml:69-71).
     */
    ScopeTabBar {
        id: scopeTabs

        anchors { left: parent.left; right: parent.right }
        y: page.indicatorGap
        visible: page.showScopeTabs
        height: scopeTabs.visible ? scopeTabs.implicitHeight : 0
        z: page.atTop ? -1 : 1

        // Rides down with the pull so the opened menu has room above it.
        // A transform, not a `y` binding: `y` is layout, and the lists are
        // full-page siblings whose contentY feeds this -- a layout change
        // would close that loop.
        transform: Translate { y: page.pullDistance }

        hostPage: page
        titles: [qsTr("Unread"), qsTr("Favourites"), qsTr("All")]
        currentIndex: pager.currentIndex
        onTabClicked: page.selectTab(index)

        // Opaque, so rows scrolling past cannot be read through the tabs.
        // Silica fills its own with a colour that is not public API; this
        // layers the public opaque colour under the ambience tint to get both
        // opacity and the right hue.
        Rectangle {
            anchors.fill: parent
            anchors.topMargin: -page.indicatorGap
            z: -1
            visible: !page.atTop
            color: Theme.overlayBackgroundColor

            Rectangle {
                anchors.fill: parent
                color: typeof __silica_applicationwindow_instance !== "undefined"
                       ? __silica_applicationwindow_instance._backgroundColor
                       : "transparent"
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
