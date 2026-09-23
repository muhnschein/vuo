import QtQuick 2.6
import Sailfish.Silica 1.0
import Nemo.Notifications 1.0
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
    // And a fourth for BROWSING: the list a feed or a category opens into.
    //
    // Not one of the three. Opening a feed calls `setScope(3, feedId)` on
    // whatever model the page was handed, and `setScope` is a plain overwrite:
    // handing it a tab's model re-scoped the very list that tab shows, and
    // nothing ever set it back. Reported from a device -- open a feed, go
    // back, and the Unread tab lists that one feed's articles until the app is
    // restarted. This model is the only one a pushed view ever re-scopes, and
    // no tab is bound to it.
    EntryModel { id: browseEntries }
    FeedModel { id: feeds }
    // Asked one thing here: whether an account is stored at all. That
    // decides the first page, and it is read from the file on every access,
    // so it is right before anything has been loaded.
    //
    // NOT `id: account`. OnboardingPage declares `property var account`, and a
    // binding is resolved against the object's own properties before the ids
    // around it -- so `account: account` in the Component below bound that
    // property to itself. Qt called it a binding loop and left it null, which
    // it did on a device while every check in the build passed. The id has to
    // be a name no page declares.
    Settings { id: accountSettings }

    // The notification for new articles.
    //
    // `arrivals` reads the mirror and decides; `arrivalNotice` is how the
    // decision reaches the home screen. One notification, replaced in place,
    // never one per article: a sync that brings forty articles is one piece of
    // news, not forty.
    Arrivals { id: arrivals }

    /// What `arrivals.review` answers. Mirrors `arrivals::ANNOUNCE_*`, and
    /// `arrivals::tests` holds the two to the same numbers.
    readonly property int announceNothing: 0
    readonly property int announceBanner: 1
    readonly property int announceQuiet: 2
    readonly property int announceWithdraw: 3

    Notification {
        id: arrivalNotice
        appName: "Vuo"
        appIcon: "/usr/share/icons/hicolor/172x172/apps/harbour-vuo.png"
        // Without an action named "default" the home screen offers nothing to
        // tap, and `clicked` is never emitted: the plugin raises it only when
        // that action is invoked. No D-Bus target, because Harbour gives Vuo
        // no service to be started through -- so a tap does something only
        // while Vuo is running, which is the only time it can have published.
        remoteActions: [ { "name": "default" } ]
        onClicked: app.activate()
        // Whatever the reason. The plugin's own CloseReason enum starts at 0
        // (Expired) where lipstick's starts at 1, so `DismissedByUser` there
        // is lipstick's "expired" -- a comparison against it would be wrong
        // on a device and right against any stub. It does not matter here:
        // this notification never expires on its own, and a close Vuo asks
        // for itself is not reported back, so a close that arrives at all is
        // the reader's doing.
        onClosed: arrivals.dismissed()
    }

    /// Bring the notification for new articles up to date with the mirror.
    ///
    /// Called whenever the poll finds that the mirror changed, and on every
    /// move to the front or to the cover. While Vuo is in front of the reader
    /// this acknowledges everything and takes the notification down.
    function announceArrivals() {
        var action = arrivals.review(Qt.application.active,
                                     accountSettings.notifyNewArticles)
        if (action === app.announceWithdraw) {
            arrivalNotice.close()
            return
        }
        if (action !== app.announceBanner && action !== app.announceQuiet) {
            return
        }
        // The count and nothing else: no titles, no body, and no itemCount,
        // whose default of 1 is what keeps the home screen from drawing a
        // badge beside a number the summary already says.
        arrivalNotice.summary = qsTr("%n new article(s)", "",
                                     arrivals.articleCount())
        // A banner only for news. A quiet update has to EMPTY the preview
        // rather than leave it unset: the plugin fills an unset preview in
        // from the summary, and would pop a banner to say that there is now
        // less to read. Emptying works because a quiet update only ever
        // follows a banner on this object, which is what set it. The preview
        // body is never set, so the plugin fills it from the body: empty.
        var banner = action === app.announceBanner
        arrivalNotice.previewSummary = banner ? arrivalNotice.summary : ""
        arrivalNotice.publish()
    }

    // Models observe SQLite, and the worker writes to SQLite from another
    // thread. This is how they find out. A poll rather than a signal because
    // QML owns these objects: Rust has no handle on a live model to call into,
    // and a registry of cross-thread pointers is exactly the sort of thing
    // that cannot be exercised without a device.
    //
    // AT THE CADENCE OF WHAT IT COULD POSSIBLY SEE. This used to be
    // `running: true` at 1.5 seconds, which on a phone means forty wakeups a
    // minute for the life of the process -- minimised, screen off, no account
    // configured, it made no difference. A tick is individually cheap
    // (`pollSync` is an atomic load and an early return while nothing has
    // changed), but the wakeup itself is the cost: it enters the JS engine,
    // walks five models, and denies the CPU the deep idle states it would
    // otherwise reach between them.
    //
    // While the app is active, 1.5 seconds: the reader is looking at a list
    // that their own taps and a running sync both change.
    //
    // While it is not, the only thing that can change what the cover shows is
    // an automatic sync finishing, and those are an interval apart -- so the
    // poll drops to a quarter of that interval, and to nothing at all on
    // "Manual only", where nothing syncs by itself. See
    // `settings::cover_poll_ms_for` for the table.
    function pollModels() {
        // Every model is polled, not just the first: a local mutation on
        // one tab bumps the generation so the others pick the change up,
        // and `pollSync` is the only thing that looks.
        var changed = entries.pollSync()
        changed = starredEntries.pollSync() || changed
        changed = allEntries.pollSync() || changed
        // Cheap while nothing is being browsed: a model with no scope
        // reloads nothing.
        changed = browseEntries.pollSync() || changed
        if (changed) {
            feeds.pollSync()
        }
        return changed
    }

    /// True while a refresh started FROM THE COVER is still in flight.
    ///
    /// The cover has a Refresh action and the app is by definition not active
    /// while it is showing, so without this the one thing the reader can start
    /// from the cover would raise a spinner that never moved and never
    /// cleared: `syncing` is a live read, but the CHANGE NOTIFICATION that
    /// redraws the cover is only emitted by `pollSync`.
    property bool _watchingCoverRefresh: false

    /// True while the app is active, or while a refresh started from the
    /// cover is still running -- both want the fast cadence.
    readonly property bool _watching: Qt.application.active
                                      || app._watchingCoverRefresh

    Timer {
        id: mirrorPoll
        interval: app._watching ? 1500 : accountSettings.coverPollMs
        repeat: true
        running: app._watching || accountSettings.coverPollMs > 0
        onTriggered: {
            if (app.pollModels()) {
                app.announceArrivals()
            }
            // `syncing` reads the worker's flag directly, so this is current
            // whether or not the tick above emitted anything.
            if (app._watchingCoverRefresh && !entries.syncing) {
                app._watchingCoverRefresh = false
            }
        }
    }

    /// Mirrors the application's own state so a handler can fire on it.
    property bool _active: Qt.application.active

    on_ActiveChanged: {
        // The interval above is read off the stored account, and Settings is a
        // separate instance of this object that writes the file -- so this one
        // has to be told to look again. On the way OUT is when it matters: the
        // reader may have just changed the sync interval, and the cadence they
        // leave on should be the one they chose.
        accountSettings.reload()
        if (app._active) {
            // Once, immediately, on the way back in. The timer starts on the
            // same transition, but its first tick is an interval away, and the
            // list should be current in the frame the reader sees rather than
            // a second and a half later.
            app.pollModels()
        }
        // Both ways. In: the reader is looking, so the notification comes
        // down. Out: anything that landed in the moment before they left is
        // theirs to be told about, and the switch was just re-read above.
        app.announceArrivals()
    }

    Component.onCompleted: {
        // `coverPollMs` comes off the stored account, and this object holds
        // Rust defaults until it is told to look. Without this the idle
        // cadence would be "Manual only" -- no poll at all -- for a reader who
        // started the app and minimised it without ever visiting Settings,
        // whatever interval their account actually carries.
        accountSettings.reload()
        // The models are empty until a scope is set. 0 unread, 1 starred,
        // 2 all -- see models::Scope.
        entries.setScope(0, 0)
        starredEntries.setScope(1, 0)
        allEntries.setScope(2, 0)
        feeds.refresh()
        // A notification the previous run left up. `arrivalNotice` only knows
        // the one it published itself, and the home screen keeps them after
        // the process that raised them has gone -- where a tap on one could
        // do nothing at all. The reader is opening Vuo, which is what it was
        // asking them to do.
        var stale = arrivalNotice.notifications()
        for (var i = 0; i < stale.length; i++) {
            stale[i].close()
        }
    }

    Component {
        id: entryList

        EntryListPage {
            // In `scopeTabKinds` order: unread, starred, all.
            scopeModels: [entries, starredEntries, allEntries]
            model: entries
            browseModel: browseEntries
            feedModel: feeds
            // `entries` is polled first above, so it is the model that takes
            // the sync-failure notice. See EntryListPage.noticeModel.
            noticeModel: entries
            scopeKind: 0
        }
    }

    // A fresh install opens on the onboarding page instead of an empty list,
    // and moves through setup to the list. Every step REPLACES the one before
    // it, so the stack is one page deep the whole way and the article list
    // ends up as the app's root: a welcome screen left underneath would be
    // what a swipe back from the list landed on ever after.
    Component {
        id: onboarding

        OnboardingPage {
            onContinued: pageStack.replace(setup)
        }
    }

    Component {
        id: setup

        SetupDialog {
            // Accepting navigates here itself, as part of the accept: nothing
            // in this flow navigates from a signal handler any more.
            acceptDestination: entryList
            // A mirror that has just been given a server has nothing in it
            // yet, and the pulley's Refresh should not be the first thing a
            // new user has to find.
            onConfigured: entries.requestSync()
        }
    }

    initialPage: accountSettings.configured ? entryList : onboarding

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
            onRefresh: {
                entries.requestSync()
                // See `_watchingCoverRefresh`: the poll is stopped while the
                // cover is up, and this is the one thing that starts work
                // from there.
                app._watchingCoverRefresh = true
            }
        }
    }
    allowedOrientations: defaultAllowedOrientations
}
