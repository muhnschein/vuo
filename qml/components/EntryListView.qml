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
    property string scopeLabel: ""
    property string title: ""

    /// How far the list has been pulled past its top, as a positive number.
    ///
    /// `contentY - originY` is the expression Silica's own TabItem uses
    /// (private/TabItem.qml:47-49) -- `originY` rather than 0 because a list
    /// with a header starts at a non-zero content origin. The tab strip reads
    /// this off the CURRENT tab, which is why it lives here and not on the
    /// page: Silica reads its own `yOffset` off `currentItem` the same way
    /// (private/TabView.qml:51).
    readonly property real pullDistance: listView.contentY < listView.originY
                                         ? listView.originY - listView.contentY
                                         : 0

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
    clip: listView.pullDistance === 0
    // Bound only while this is the tab in front.
    //
    // All three tabs share ONE EntryModel -- it is re-scoped on a switch --
    // so binding every tab to it would slide an identical copy of the list
    // the user is already looking at in from the side. An unbound neighbour
    // fills the moment the swipe settles and the scope is applied, which is
    // the same trade Silica's own TabView makes: `cacheSize: 0` and an
    // asynchronously-loaded delegate, so its neighbour is not live either
    // (private/TabView.qml:57, :95).
    model: listView.current ? listView.entryModel : null

    // PageHeader's own title label offers no supported way to force its
    // textFormat, and `listView.title` can be a FEED NAME -- foreign text
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
            visible: !listView.showScopeTabs
            title: listView.scopeLabel
            description: listView.entryModel && listView.entryModel.count > 0
                         ? qsTr("%n article(s)", "", listView.entryModel.count)
                         : ""
        }

        Label {
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
        MenuItem {
            text: qsTr("Settings")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/SettingsPage.qml"))
        }
        MenuItem {
            text: qsTr("Feeds")
            onClicked: pageStack.push(Qt.resolvedUrl("../pages/FeedListPage.qml"),
                                      { model: listView.hostPage ? listView.hostPage.feedModel : null,
                                        entryModel: listView.entryModel })
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
            visible: listView.scopeKind !== 2
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
        enabled: listView.count === 0
                 && !(listView.entryModel && listView.entryModel.syncing)
        // "Nothing to read / Pull down to refresh" is an Unread string.
        // Under a Favourites tab it reads as a refusal, and refreshing
        // does not create favourites.
        text: listView.scopeKind === 1 ? qsTr("No favourites")
                                   : qsTr("Nothing to read")
        hintText: listView.scopeKind === 1
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

        onClicked: pageStack.push(Qt.resolvedUrl("../pages/ArticlePage.qml"), {
            entryId: entryId,
            entryTitle: title
        })

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