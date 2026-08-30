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
    function selectScope(kind) {
        if (kind === page.scopeKind) {
            // Tapping the tab you are already on goes back to the top.
            listView.scrollToTop()
            return
        }
        // NOT optional. `markAllRead` reads the model's scope when the remorse
        // countdown FIRES, not when the item was tapped, and RemorsePopup
        // flushes itself only on PageStatus.Deactivating. A page change was
        // therefore the only way the shared model could be re-scoped, so a
        // pending action always resolved first. Switching tabs is not a page
        // change, so that safety net is gone: without this, arming "Mark all
        // as read" on Unread and tapping "All" within the countdown would run
        // it against every article in the mirror. `trigger()` runs it NOW,
        // against the scope the user meant; `cancel()` would silently discard
        // what they asked for.
        if (remorse.active) {
            remorse.trigger()
        }
        page.scopeKind = kind
        // Clear the feed id AND the feed name together, or "Favourites" would
        // keep a feed title sitting under it.
        page.scopeId = 0
        page.title = ""
        page.applyScope()
        listView.positionViewAtBeginning()
    }

    allowedOrientations: Orientation.All

    // The strip stands where the PageHeader would, pinned, as the clock and
    // Settings apps wear it -- Silica's own TabView pins its TabBar outside
    // the paged content and sizes the content to what is left.
    ScopeTabBar {
        id: scopeTabs

        anchors { top: parent.top; left: parent.left; right: parent.right }
        visible: page.showScopeTabs
        height: scopeTabs.visible ? scopeTabs.implicitHeight : 0

        hostPage: page
        titles: [qsTr("Unread"), qsTr("Favourites"), qsTr("All")]
        currentIndex: page.scopeTabKinds.indexOf(page.scopeKind)
        onTabClicked: page.selectScope(page.scopeTabKinds[index])
    }

    SilicaListView {
        id: listView
        anchors {
            top: scopeTabs.bottom
            left: parent.left
            right: parent.right
            bottom: parent.bottom
        }
        // A SilicaListView does not clip, and the pulley menu parents itself
        // into the flickable's content ABOVE the viewport. Without this it and
        // any rows scrolled past the top would paint over the pinned strip.
        // Silica's TabView solves the same problem with an opaque background
        // filled from a colour that is not public API.
        clip: true
        model: page.model

        // PageHeader's own title label offers no supported way to force its
        // textFormat, and `page.title` can be a FEED NAME -- foreign text
        // chosen by the feed operator. §9.3: a crafted title in a rich-text
        // context is markup injection, and can pull a remote image that leaks
        // the device's IP on a list scroll. So the header carries a fixed
        // string and the name is rendered by a Label this file controls.
        header: Column {
            width: listView.width

            // Only for the scopes the strip does not cover. A Column skips
            // invisible children, so this costs no space on a tab scope.
            //
            // Retiring it there also drops a binding that was about to start
            // lying: `count` is rows HELD, and reload caps the query at 500,
            // so an "All" scope on any real mirror would have read
            // "500 articles" for ever.
            PageHeader {
                visible: !page.showScopeTabs
                title: page.scopeLabel
                description: page.model && page.model.count > 0
                             ? qsTr("%n article(s)", "", page.model.count)
                             : ""
            }

            // Why the last refresh failed. Before this, a refresh that could
            // not reach the server said nothing at all and span forever.
            Item {
                width: parent.width
                height: failure.visible ? failure.height + Theme.paddingLarge : 0

                Column {
                    id: failure
                    // A ternary, not `page.model && ...`: with no model that
                    // conjunction is `null`, and Qt refuses to assign it to a
                    // bool ("Unable to assign [undefined] to bool").
                    visible: page.model
                             ? (page.model.syncError.length > 0 || page.model.syncErrorIsAuth)
                             : false
                    x: Theme.horizontalPageMargin
                    width: parent.width - Theme.horizontalPageMargin * 2
                    spacing: Theme.paddingSmall

                    Label {
                        width: parent.width
                        wrapMode: Text.Wrap
                        // The server's own words on a general failure. Plain
                        // text, always: Silica's own Notices banner would
                        // render this as AutoText, which turns a crafted error
                        // into markup injection and a remote-image IP leak.
                        textFormat: Text.PlainText
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.errorColor
                        text: (page.model ? page.model.syncErrorIsAuth : false)
                              ? qsTr("The server rejected the API key.")
                              : qsTr("Refresh failed: %1").arg(
                                    page.model ? page.model.syncError : "")
                    }

                    Button {
                        visible: page.model ? page.model.syncErrorIsAuth : false
                        text: qsTr("Open settings")
                        onClicked: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
                    }
                }
            }

            Label {
                visible: page.title.length > 0
                x: Theme.horizontalPageMargin
                width: parent.width - Theme.horizontalPageMargin * 2
                textFormat: Text.PlainText
                text: page.title
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
                onClicked: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
            }
            MenuItem {
                text: qsTr("Feeds")
                onClicked: pageStack.push(Qt.resolvedUrl("FeedListPage.qml"),
                                          { model: page.feedModel,
                                            entryModel: page.model })
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
                visible: page.scopeKind !== 2
                onClicked: remorse.execute(qsTr("Marking all as read"), function() {
                    page.model.markAllRead()
                })
            }
            MenuItem {
                text: qsTr("Refresh")
                // Asks the worker for a network sync. `refresh()` alone only
                // re-reads the local mirror, so on its own the pulley menu
                // never actually talked to the server.
                onClicked: page.model.requestSync()
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
            running: page.model ? page.model.syncing : false
        }

        ViewPlaceholder {
            // Not while syncing: "Nothing to read / Pull down to refresh"
            // under a running spinner reads as a refusal.
            enabled: listView.count === 0
                     && !(page.model && page.model.syncing)
            // "Nothing to read / Pull down to refresh" is an Unread string.
            // Under a Favourites tab it reads as a refusal, and refreshing
            // does not create favourites.
            text: page.scopeKind === 1 ? qsTr("No favourites")
                                       : qsTr("Nothing to read")
            hintText: page.scopeKind === 1
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

                Label {
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

                Row {
                    spacing: Theme.paddingMedium

                    Label {
                        // Author names are foreign too.
                        textFormat: Text.PlainText
                        text: author
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.secondaryColor
                        visible: author.length > 0
                    }
                    Label {
                        // Generated locally from an integer, so no foreign
                        // data reaches this one -- but it is still explicit,
                        // because a rule with exceptions is not checkable.
                        textFormat: Text.PlainText
                        text: readingTime > 0 ? qsTr("%n min", "", readingTime) : ""
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: Theme.secondaryColor
                    }
                    Label {
                        textFormat: Text.PlainText
                        text: "★"
                        color: Theme.highlightColor
                        font.pixelSize: Theme.fontSizeExtraSmall
                        visible: starred
                    }
                }
            }

            onClicked: pageStack.push(Qt.resolvedUrl("ArticlePage.qml"), {
                entryId: entryId,
                entryTitle: title
            })

            menu: ContextMenu {
                MenuItem {
                    text: unread ? qsTr("Mark as read") : qsTr("Mark as unread")
                    onClicked: page.model.setRead(index, unread)
                }
                MenuItem {
                    text: starred ? qsTr("Remove favourite") : qsTr("Add favourite")
                    onClicked: page.model.setStarred(index, !starred)
                }
            }
        }

        VerticalScrollDecorator {}
    }
}
