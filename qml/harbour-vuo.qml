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

    // Models observe SQLite, and the worker writes to SQLite from another
    // thread. This is how they find out. A poll rather than a signal because
    // QML owns these objects: Rust has no handle on a live model to call into,
    // and a registry of cross-thread pointers is exactly the sort of thing
    // that cannot be exercised without a device.
    Timer {
        interval: 1500
        repeat: true
        running: true
        onTriggered: {
            if (entries.pollSync()) {
                feeds.pollSync()
            }
        }
    }

    // Periodic sync while the app is running.
    //
    // The Harbour package cannot ship the systemd timer -- validatepaths
    // permits only the binary, the desktop file, the icons and
    // %{_datadir}/harbour-vuo, so a user unit is "Installation not allowed in
    // this location". Without this Timer the Sync interval setting would reach
    // nothing at all in a store build, which is the defect it was just fixed
    // out of.
    //
    // In an OpenRepos build the systemd timer also exists and covers the app
    // being closed; the two overlapping is harmless, because a sync is a
    // no-op when the cursor has not moved.
    Settings {
        id: appSettings
        Component.onCompleted: appSettings.load()
    }

    Timer {
        id: periodicSync
        // SYNC_INTERVALS_MINUTES in settings.rs: 0 = manual only.
        readonly property var minutes: [0, 15, 30, 60, 360]
        interval: Math.max(1, minutes[appSettings.syncIntervalIndex] || 0) * 60 * 1000
        repeat: true
        running: (minutes[appSettings.syncIntervalIndex] || 0) > 0
        onTriggered: entries.requestSync()
    }

    Component.onCompleted: {
        // 0 = unread. The models are empty until a scope is set.
        entries.setScope(0, 0)
        feeds.refresh()
    }

    initialPage: Component {
        EntryListPage { model: entries; feedModel: feeds; scopeKind: 0 }
    }

    // The cover is a separate Component so its bindings can reach `entries`
    // for the unread count, which a bare URL cover cannot.
    cover: Component {
        CoverPage {
            unreadCount: entries.unreadTotal
            syncing: entries.syncing
            onRefresh: entries.requestSync()
        }
    }
    allowedOrientations: defaultAllowedOrientations
}
