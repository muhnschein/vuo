import QtQuick 2.6
import Sailfish.Silica 1.0
import Vuo 1.0
import "pages"
import "cover"

/*
 * Vuo's root window.
 *
 * The QML layer is deliberately dumb (scope §5): it draws what the Rust models
 * hand it and makes no decisions about parsing, sanitising or sync. Two rules
 * from §9.3 are enforced by convention throughout and are worth stating once,
 * here, because they are invisible in a diff:
 *
 *   1. Every Text that renders foreign data sets `textFormat` EXPLICITLY.
 *      Never leave it at the default. A feed title is chosen by the feed
 *      operator, and in rich-text mode a crafted title becomes markup
 *      injection into the UI -- and can pull a remote image that leaks the
 *      device's IP on a list scroll.
 *
 *   2. No QML is ever built from server data. No Qt.createQmlObject, no
 *      Component source assembled from a string containing anything foreign.
 *      That is arbitrary code execution in the app's own process.
 */
ApplicationWindow {
    id: app

    property alias unreadCount: entries.count

    // The models are Rust QObjects. They read the local SQLite mirror, which
    // is the single source of truth for the UI; nothing here waits on the
    // network.
    EntryModel { id: entries }
    FeedModel { id: feeds }

    Component.onCompleted: {
        // 0 = unread. The models are empty until a scope is set.
        entries.setScope(0, 0)
        feeds.refresh()
    }

    initialPage: Component { EntryListPage { model: entries; feedModel: feeds } }

    // The cover is a separate Component so its bindings can reach `entries`
    // for the unread count, which a bare URL cover cannot.
    cover: Component {
        CoverPage {
            unreadCount: entries.count
            onRefresh: entries.refresh()
        }
    }
    allowedOrientations: defaultAllowedOrientations
}
