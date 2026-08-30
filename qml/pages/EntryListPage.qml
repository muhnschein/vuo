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

    /// The model the page-level furniture (the notice banner) speaks for.
    property var currentModel: page.showScopeTabs
                               ? (page.scopeModels[pager.currentIndex] || null)
                               : page.model

    allowedOrientations: Orientation.All

    /*
     * The tab strip, above the tabs rather than on top of them.
     *
     * The strip owns the top of the page and the pager starts underneath it,
     * so nothing the lists draw ever reaches this band: no scrolled row, and
     * no swiped-in neighbour. That is the whole reason for the inset, and it
     * replaces an earlier arrangement where the pager was full-page and the
     * strip floated over it.
     *
     * That arrangement had to hide the rows passing behind the strip, and the
     * only way to do that is to paint the strip opaque -- which Silica does
     * with `BackgroundRectangle` (private/TabView.qml:74-81), a
     * Sailfish.Silica.private item that redraws the window background. There
     * is no public equivalent: `_backgroundColor` is
     * `Qt.tint(_pageDimmerColor, _pageColor)` and BOTH are
     * `Theme.rgba(overlayBackgroundColor, 0)` for a page in a stack
     * (ApplicationWindow.qml:84-100), so a rectangle filled from it is
     * transparent and only the fallback colour under it showed -- a black
     * band across the top of the screen, reported from the device.
     *
     * Insetting the pager needs no colour at all: the ambience shows through
     * the strip at every scroll position, which is exactly what it did at the
     * top before. The cost is that the pulley menu now opens BELOW the strip
     * instead of over it, which is also what the device asked for -- the strip
     * can no longer obscure the menu because it is no longer in front of it.
     */
    ScopeTabBar {
        id: scopeTabs

        anchors { top: parent.top; left: parent.left; right: parent.right }
        visible: page.showScopeTabs
        height: scopeTabs.visible ? scopeTabs.implicitHeight : 0

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
    PagedView {
        id: pager

        // Below the strip, not underneath it. Every list is clipped to these
        // bounds, so the band the strip occupies is the ambience and nothing
        // else -- see the note on the strip below.
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
            topPadding: page.showScopeTabs ? Theme.paddingLarge : 0
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
