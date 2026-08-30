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

    function applyScope() {
        if (page.model) {
            page.model.setScope(page.scopeKind, page.scopeId)
        }
    }

    /// Switch scope in place, with no page transition.
    ///
    /// Called by the tab strip on a tap and by the PagedView when a swipe
    /// settles, so both routes go through exactly one place.
    function selectScope(kind) {
        if (kind === page.scopeKind) {
            // Tapping the tab you are already on goes back to the top.
            if (pager.currentItem) {
                pager.currentItem.scrollToTop()
            }
            return
        }
        page.scopeKind = kind
        // Clear the feed id AND the feed name together, or "Favourites" would
        // keep a feed title sitting under it.
        page.scopeId = 0
        page.title = ""
        page.applyScope()
        // Keep the pager in step when the change came from the strip.
        var index = page.scopeTabKinds.indexOf(kind)
        if (page.showScopeTabs && index >= 0 && pager.currentIndex !== index) {
            pager.moveTo(index)
        }
    }

    allowedOrientations: Orientation.All

    /// How far the CURRENT tab has been pulled past its top.
    ///
    /// Read off `currentItem`, exactly as Silica's TabView reads its own
    /// `yOffset` (private/TabView.qml:51): the strip has to follow whichever
    /// list is in front, and after a swipe that is a different object.
    property real _pullDistance: pager.currentItem ? pager.currentItem.pullDistance : 0

    // The strip stands where the PageHeader would, pinned, as the clock and
    // Settings apps wear it -- Silica's own TabView pins its TabBar outside
    // the paged content and sizes the content to what is left.
    ScopeTabBar {
        id: scopeTabs

        anchors { top: parent.top; left: parent.left; right: parent.right }
        visible: page.showScopeTabs
        height: scopeTabs.visible ? scopeTabs.implicitHeight : 0

        // Ride down with the pulley and get out of its way, exactly as
        // Silica's TabView moves its own TabBar (private/TabView.qml:69-71:
        // `y: Math.max(0, -root.yOffset)` and `z: yOffset < 0 ? -1 : 1`).
        // Without this the strip stayed pinned over the top of the menu, so
        // the pulley opened UNDERNEATH the tabs.
        //
        // A `transform`, not a `y` binding: the pager is anchored to this
        // item's bottom, and moving its geometry would move the lists, which
        // changes `contentY`, which moves this item again -- a binding loop.
        // A transform paints somewhere else without touching layout.
        transform: Translate { y: page._pullDistance }
        // Behind the lists while the menu is out, in front of them otherwise,
        // so rows scrolled past the top still cannot paint over the tabs.
        z: page._pullDistance > 0 ? -1 : 1

        hostPage: page
        titles: [qsTr("Unread"), qsTr("Favourites"), qsTr("All")]
        currentIndex: page.scopeTabKinds.indexOf(page.scopeKind)
        onTabClicked: page.selectScope(page.scopeTabKinds[index])
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

        anchors {
            top: scopeTabs.bottom
            left: parent.left
            right: parent.right
            bottom: parent.bottom
        }

        // One page per tab -- or exactly one, with no swiping, for a feed or
        // category view, which is reached by pushing a page and left by going
        // back rather than by swiping sideways.
        model: page.showScopeTabs ? page.scopeTabKinds.length : 1
        interactive: page.showScopeTabs
        // `currentIndex` is driven imperatively, NOT bound.
        //
        // A swipe writes this property from C++, and a QML binding that is
        // written to is gone for good -- so binding it to the scope would work
        // exactly until the first swipe, after which tapping a tab would stop
        // moving the pager. `moveTo` in `selectScope` and the handler below
        // keep the two in step in both directions instead.
        cacheSize: 1

        Component.onCompleted: {
            var index = page.scopeTabKinds.indexOf(page.scopeKind)
            if (page.showScopeTabs && index > 0) {
                pager.currentIndex = index
            }
        }

        onCurrentIndexChanged: {
            if (!page.showScopeTabs) {
                return
            }
            // A settled swipe is a scope change like any other. The guard is
            // what stops this and `selectScope`'s `moveTo` calling each other:
            // by the time either fires again, the two already agree.
            var kind = page.scopeTabKinds[pager.currentIndex]
            if (kind !== undefined && kind !== page.scopeKind) {
                page.selectScope(kind)
            }
        }

        delegate: EntryListView {
            width: pager.width
            height: pager.height

            hostPage: page
            entryModel: page.model
            current: pager.currentIndex === index
            // Each tab knows its OWN scope, not the page's: that is what lets
            // "Mark all as read" bind to the tab it was armed on.
            scopeKind: page.showScopeTabs ? page.scopeTabKinds[index] : page.scopeKind
            scopeId: page.showScopeTabs ? 0 : page.scopeId
            showScopeTabs: page.showScopeTabs
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
    property string _failureToken: page.model
                                  ? (page.model.syncErrorIsAuth ? "auth" : page.model.syncError)
                                  : ""

    on_FailureTokenChanged: {
        if (page._failureToken.length === 0) {
            notice.dismiss()
        } else if (page.model.syncErrorIsAuth) {
            notice.post(qsTr("The server rejected the API key."), true,
                        qsTr("Open settings"))
        } else {
            notice.post(qsTr("Refresh failed: %1").arg(page.model.syncError), true, "")
        }
    }
}
