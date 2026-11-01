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

    // No app-level `unreadCount: entries.count`: tabs re-scope `entries`, so it
    // would report starred or all-entries row count. Cover binds
    // entries.unreadTotal.

    // Models = Rust QObjects reading local SQLite mirror, UI's single source of
    // truth; nothing here waits on network. One model per scope tab, fixed
    // scope for app life.
    //
    // Shared re-scoped model can't keep neighbour tab populated (rows only
    // after swipe settles → blank page on every swipe). Three models = three
    // queries on open SQLite connection, all tabs always laid out.
    EntryModel { id: entries }
    EntryModel { id: starredEntries }
    EntryModel { id: allEntries }
    // Fourth for BROWSING: list feed or category opens into.
    //
    // Not one of the three. Opening feed calls `setScope(3, feedId)` on handed
    // model; `setScope` plain overwrite, so tab model would show that feed
    // until restart. Only model pushed views re-scope; no tab bound to it.
    EntryModel { id: browseEntries }
    // Fifth for SEARCH, same reason: search page re-scopes handed model, gets
    // one no tab shows.
    EntryModel { id: searchEntries }
    FeedModel { id: feeds }
    // Asked one thing: is account stored. Decides first page; read from file
    // every access, so right before anything loaded.
    //
    // NOT `id: account`. OnboardingPage declares `property var account`;
    // binding resolves against own properties before surrounding ids, so
    // `account: account` binds property to itself (Qt: binding loop, null,
    // while build checks pass). Id must be name no page declares.
    Settings { id: accountSettings }

    // New-articles notification.
    //
    // `arrivals` reads mirror, decides; `arrivalNotice` carries decision to
    // home screen. One notification replaced in place, never per article:
    // forty-article sync = one piece of news.
    Arrivals { id: arrivals }

    /// What `arrivals.review` answers. Mirrors `arrivals::ANNOUNCE_*`;
    /// `arrivals::tests` holds both to same numbers.
    readonly property int announceNothing: 0
    readonly property int announceBanner: 1
    readonly property int announceQuiet: 2
    readonly property int announceWithdraw: 3

    Notification {
        id: arrivalNotice
        appName: "Vuo"
        appIcon: "/usr/share/icons/hicolor/172x172/apps/harbour-vuo.png"
        // No action named "default" = home screen offers nothing to tap,
        // `clicked` never emitted (plugin raises it only on that action). No
        // D-Bus target: Harbour gives no service to start through, so tap works
        // only while Vuo runs (only time it can have published).
        remoteActions: [ { "name": "default" } ]
        onClicked: app.activate()
        // Any reason. Plugin CloseReason starts at 0 (Expired), lipstick's at
        // 1, so `DismissedByUser` there = lipstick "expired": comparing would
        // be wrong on device, right on stub. Irrelevant: never expires itself,
        // Vuo-requested close not reported, so any close = reader's.
        onClosed: arrivals.dismissed()
    }

    /// Sync new-articles notification with mirror.
    ///
    /// Called when poll sees mirror change, and on every move to front or
    /// cover. While Vuo in front: acknowledges all, takes notification down.
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
        // Numbers only: no titles, no body, no itemCount (default 1 stops home
        // screen badge duplicating summary's number).
        var count = arrivals.articleCount()
        var total = arrivals.unreadTotal()
        var summary = qsTr("%n new article(s)", "", count)
        // Then cover's number, screens agree: "11 new articles · 14 unread".
        // Only when informative: arrivals all unread, total <= count says
        // nothing new ("3 new articles · 3 unread" says it twice). Joined like
        // list details, outside translations.
        if (total > count) {
            summary += " \u00b7 " + qsTr("%n unread", "", total)
        }
        arrivalNotice.summary = summary
        // Banner only for news. Quiet update must EMPTY preview, not leave
        // unset: plugin fills unset preview from summary, would pop banner
        // saying less to read. Works because quiet update only follows banner
        // on this object (which set it). Preview body never set; plugin fills
        // from body: empty.
        var banner = action === app.announceBanner
        arrivalNotice.previewSummary = banner ? arrivalNotice.summary : ""
        arrivalNotice.publish()
    }

    // Models observe SQLite; worker writes from another thread. This is how
    // they learn. Poll, not signal: QML owns objects, Rust has no handle on
    // live model; cross-thread pointer registry untestable off device.
    //
    // CADENCE OF WHAT IT COULD SEE. Wakeup itself is cost (tick cheap:
    // `pollSync` = atomic load + early return): enters JS engine, walks six
    // models, blocks CPU deep idle.
    //
    // Active: 1.5 s; reader looks at list own taps and running sync change.
    //
    // Inactive: only automatic sync finishing changes cover; those are interval
    // apart. Poll = quarter of interval; none on "Manual only" (nothing syncs
    // itself). Table: `settings::cover_poll_ms_for`.
    function pollModels() {
        // Poll every model, not first only: local mutation on one tab bumps
        // generation so others pick it up; `pollSync` only thing that looks.
        var changed = entries.pollSync()
        changed = starredEntries.pollSync() || changed
        changed = allEntries.pollSync() || changed
        // Cheap while nothing browsed: unscoped model reloads nothing.
        changed = browseEntries.pollSync() || changed
        // Same: no search open, nothing scoped, nothing runs.
        changed = searchEntries.pollSync() || changed
        if (changed) {
            feeds.pollSync()
        }
        return changed
    }

    /// True while refresh started FROM COVER in flight.
    ///
    /// Cover has Refresh; app inactive while cover shows. Without this,
    /// cover-started spinner never moves or clears: `syncing` is live read, but
    /// CHANGE NOTIFICATION that redraws cover only from `pollSync`.
    property bool _watchingCoverRefresh: false

    /// True while app active or cover refresh running; both want fast cadence.
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
            // `syncing` reads worker flag directly; current whether or not tick
            // emitted.
            if (app._watchingCoverRefresh && !entries.syncing) {
                app._watchingCoverRefresh = false
            }
        }
    }

    /// Mirrors application state so handler can fire on it.
    property bool _active: Qt.application.active

    on_ActiveChanged: {
        // Interval read off stored account; Settings is separate instance
        // writing file, so this one must re-read. Matters on way OUT: reader
        // may have just changed sync interval; leave with chosen cadence.
        accountSettings.reload()
        if (app._active) {
            // Once, immediately, on way in. Timer starts on same transition but
            // first tick interval away; list current in first frame.
            app.pollModels()
        }
        // Both ways. In: reader looking, notification down. Out: anything
        // landing just before leaving gets told; switch re-read above.
        app.announceArrivals()
    }

    Component.onCompleted: {
        // `coverPollMs` from stored account; object holds Rust defaults until
        // told. Without this idle cadence = "Manual only" (no poll) for reader
        // who minimised without visiting Settings, whatever account interval.
        accountSettings.reload()
        // Models empty until scoped. 0 unread, 1 starred, 2 all; see
        // models::Scope.
        entries.setScope(0, 0)
        starredEntries.setScope(1, 0)
        allEntries.setScope(2, 0)
        feeds.refresh()
        // Notification left by previous run. `arrivalNotice` knows only own;
        // home screen keeps them after process gone (tap does nothing). Reader
        // opening Vuo = what it asked.
        var stale = arrivalNotice.notifications()
        for (var i = 0; i < stale.length; i++) {
            stale[i].close()
        }
    }

    Component {
        id: entryList

        EntryListPage {
            // `scopeTabKinds` order: unread, starred, all.
            scopeModels: [entries, starredEntries, allEntries]
            model: entries
            browseModel: browseEntries
            searchModel: searchEntries
            feedModel: feeds
            // `entries` polled first above, takes sync-failure notice. See
            // EntryListPage.noticeModel.
            noticeModel: entries
            scopeKind: 0
        }
    }

    // Fresh install opens on onboarding, not empty list, through setup to list.
    // Each step REPLACES previous: stack one deep, article list ends as root
    // (welcome screen underneath = swipe-back target forever).
    Component {
        id: onboarding

        OnboardingPage {
            onContinued: pageStack.replace(setup)
        }
    }

    Component {
        id: setup

        SetupDialog {
            // Accept navigates here itself; nothing in flow navigates from
            // signal handler.
            acceptDestination: entryList
            // Mirror just given server is empty; Refresh pulley shouldn't be
            // new user's first find.
            onConfigured: entries.requestSync()
        }
    }

    initialPage: accountSettings.configured ? entryList : onboarding

    // Cover separate Component so bindings reach models (unread count, feeds);
    // bare URL cover can't.
    cover: Component {
        CoverPage {
            unreadCount: entries.unreadTotal
            syncing: entries.syncing
            // Refresh failing while on cover says so there, not endless spin.
            syncError: entries.syncError
            syncErrorIsAuth: entries.syncErrorIsAuth
            onRefresh: {
                entries.requestSync()
                // See `_watchingCoverRefresh`: poll stopped while cover up;
                // only work started from there.
                app._watchingCoverRefresh = true
            }
        }
    }
    allowedOrientations: defaultAllowedOrientations
}
