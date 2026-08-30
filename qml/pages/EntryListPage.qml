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

    /// How far the list has been pulled past its top, as a positive number.
    ///
    /// `contentY - originY` is the expression Silica's own TabItem uses
    /// (private/TabItem.qml:47-49) -- `originY` rather than 0 because a list
    /// with a header starts at a non-zero content origin.
    property real _pullDistance: page.model && listView.contentY < listView.originY
                                 ? listView.originY - listView.contentY
                                 : 0

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
        // A `transform`, not a `y` binding: the list is anchored to this
        // item's bottom, and moving its geometry would move the list, which
        // changes `contentY`, which moves this item again -- a binding loop.
        // A transform paints somewhere else without touching layout.
        transform: Translate { y: page._pullDistance }
        // Behind the list while the menu is out, in front of it otherwise, so
        // rows scrolled past the top still cannot paint over the tabs.
        z: page._pullDistance > 0 ? -1 : 1

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
        clip: page._pullDistance === 0
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

                // The feed's own line: icon, name, age -- the three things
                // that say "where is this from, and is it fresh" before the
                // headline is even read. Miniflux's own list leads with the
                // same three, which is the shape a user coming from the web
                // UI is already reading for.
                Row {
                    width: parent.width
                    spacing: Theme.paddingSmall

                    Image {
                        id: favicon
                        // Square, and tied to the line's own text size so it
                        // tracks the user's font scaling instead of pinning a
                        // pixel count that is wrong on half the devices.
                        width: Theme.fontSizeExtraSmall
                        height: width
                        sourceSize.width: width
                        sourceSize.height: width
                        fillMode: Image.PreserveAspectFit
                        anchors.verticalCenter: parent.verticalCenter
                        // A `data:` URI built in Rust from bytes already in
                        // the mirror -- no network fetch happens here, so a
                        // list scroll cannot leak the device's IP (§9.3).
                        source: feedIcon
                        asynchronous: true
                        // The mirror stores whatever format the icon arrived
                        // in, and the device ships handlers for only some of
                        // them. Collapsing on failure keeps a missing handler
                        // to a missing icon rather than a broken-image glyph.
                        visible: feedIcon.length > 0 && status === Image.Ready
                    }

                    Label {
                        // The feed's name, which the user may have renamed.
                        // Foreign text either way: PlainText, explicitly.
                        textFormat: Text.PlainText
                        text: feedName
                        visible: feedName.length > 0
                        // Never let a long feed name push the age off the row.
                        width: Math.min(implicitWidth,
                                        parent.width - favicon.width - age.width
                                        - Theme.paddingSmall * 3)
                        truncationMode: TruncationMode.Fade
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: item.highlighted ? Theme.secondaryHighlightColor
                                                : Theme.secondaryColor
                        anchors.verticalCenter: parent.verticalCenter
                    }

                    Label {
                        id: age
                        textFormat: Text.PlainText
                        // Silica's own elapsed formatter, so "2 h ago" is
                        // worded and localised exactly as the rest of the
                        // system words it. `published` is epoch SECONDS.
                        text: published > 0
                              ? Format.formatDate(new Date(published * 1000),
                                                  Formatter.DurationElapsed)
                              : ""
                        visible: published > 0
                        font.pixelSize: Theme.fontSizeExtraSmall
                        color: item.highlighted ? Theme.secondaryHighlightColor
                                                : Theme.secondaryColor
                        anchors.verticalCenter: parent.verticalCenter
                    }
                }

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
                        text: "\u2605"
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
                MenuItem {
                    text: qsTr("Open in browser")
                    // Hidden rather than disabled: an entry with no link is
                    // rare enough that a permanently dead row would read as a
                    // bug in the app rather than a gap in the feed.
                    visible: url.length > 0
                    onClicked: Qt.openUrlExternally(url)
                }
            }
        }

        VerticalScrollDecorator {}
    }

    // Docked, not in the list's header: a notice must not move the rows the
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
