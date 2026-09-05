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

    // `unreadCount: entries.count` used to live here. `entries` is now
    // re-scoped in place by the tab strip, so an app-level property of that
    // name would report the starred or all-entries row count. It was already
    // dead -- the cover binds entries.unreadTotal -- and leaving it would be a
    // trap for whoever bound to it next.

    // The models are Rust QObjects. They read the local SQLite mirror, which
    // is the single source of truth for the UI; nothing here waits on the
    // network.
    // One model per scope tab, each fixed to its own scope for the life of
    // the app.
    //
    // There used to be a single EntryModel that every list re-scoped. That
    // made a neighbouring tab impossible to keep populated -- it could only
    // show rows once the swipe had settled and the scope had been applied, so
    // every swipe arrived on a blank page that filled a moment later. Three
    // models cost three queries against an already-open SQLite connection and
    // let all three tabs stay laid out at all times.
    EntryModel { id: entries }
    EntryModel { id: starredEntries }
    EntryModel { id: allEntries }
    FeedModel { id: feeds }
    // Asked one thing here: whether an account is stored at all. That
    // decides the first page, and it is read from the file on every access,
    // so it is right before anything has been loaded.
    Settings { id: account }

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
            // Every model is polled, not just the first: a local mutation on
            // one tab bumps the generation so the others pick the change up,
            // and `pollSync` is the only thing that looks.
            var changed = entries.pollSync()
            changed = starredEntries.pollSync() || changed
            changed = allEntries.pollSync() || changed
            if (changed) {
                feeds.pollSync()
            }
        }
    }

    Component.onCompleted: {
        // The models are empty until a scope is set. 0 unread, 1 starred,
        // 2 all -- see models::Scope.
        entries.setScope(0, 0)
        starredEntries.setScope(1, 0)
        allEntries.setScope(2, 0)
        feeds.refresh()
    }

    Component {
        id: entryList

        EntryListPage {
            // In `scopeTabKinds` order: unread, starred, all.
            scopeModels: [entries, starredEntries, allEntries]
            model: entries
            feedModel: feeds
            scopeKind: 0
        }
    }

    // A fresh install opens on the onboarding page instead of an empty list,
    // and moves to the list once an account has been saved.
    Component {
        id: onboarding

        OnboardingPage {
            account: account
            onFinished: app.showEntries()
        }
    }

    initialPage: account.configured ? entryList : onboarding

    /// Swap the onboarding page for the entry list, and fetch: a mirror that
    /// has just been given a server has nothing in it yet, and the pulley's
    /// Refresh should not be the first thing a new user has to find.
    function showEntries() {
        pageStack.replace(entryList)
        entries.requestSync()
    }

    // The cover is a separate Component so its bindings can reach the models
    // -- the unread count and the feeds it draws -- which a bare URL cover
    // cannot.
    cover: Component {
        CoverPage {
            unreadCount: entries.unreadTotal
            syncing: entries.syncing
            // So a refresh that fails while the app is on the cover says so
            // there, rather than spinning until the user reopens the app.
            syncError: entries.syncError
            syncErrorIsAuth: entries.syncErrorIsAuth
            onRefresh: entries.requestSync()
        }
    }
    allowedOrientations: defaultAllowedOrientations
}
