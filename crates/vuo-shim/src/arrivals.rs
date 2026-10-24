//! What new-articles notification needs from mirror.
//!
//! QML raises notification (`Nemo.Notifications` = QML module, reaches home
//! screen), so this does what models do: reads mirror when asked, returns plain
//! values. Decision = [`vuo_core::notify::Announcer`]'s, tested without Qt;
//! this = thin wrapper feeding it, remembering answer.
//!
//! # When asked
//!
//! By root window, each time poll finds mirror changed, and each time app comes
//! to front or goes to cover. No push from worker (QML owns object; Rust has no
//! handle). Visible consequence: on cover, poll runs at quarter of sync
//! interval (`settings::cover_poll_ms_for`); notification can lag finding sync
//! that long, same as cover's count.

// See note atop settings.rs: `QObject` derive for plain `QObject` base expands
// to glue clippy reads as useless transmute.
#![allow(clippy::useless_transmute)]

use qmetaobject::{qt_base_class, qt_method, QObject};
use vuo_core::db::store;
use vuo_core::notify::{Announcement, Announcer};

use crate::context::AppContext;

/// What `Arrivals::review` tells QML. Integers: this `qmetaobject` has no
/// `qml_register_enum` on Qt 5.6 (crate docs).
///
/// Leave notification as is.
pub const ANNOUNCE_NOTHING: i32 = 0;
/// Publish it, with a banner: something new arrived.
pub const ANNOUNCE_BANNER: i32 = 1;
/// Publish without banner: count changed, nothing new arrived.
pub const ANNOUNCE_QUIET: i32 = 2;
/// Close it.
pub const ANNOUNCE_WITHDRAW: i32 = 3;

/// The QML object behind the new-articles notification; see the module docs.
#[derive(QObject, Default)]
pub struct Arrivals {
    base: qt_base_class!(trait QObject),

    /// Look at mirror, say what notification should do: one of `ANNOUNCE_*`.
    ///
    /// `active`: reader has Vuo in front. Then nothing is news (list right
    /// there): every arrival acknowledged on landing, notification comes down.
    /// `enabled` = Settings switch.
    review: qt_method!(fn(&mut self, active: bool, enabled: bool) -> i32),
    /// Home screen closed notification (swiped, cleared all, tapped).
    /// Acknowledges exactly covered articles; next counts only what came after.
    dismissed: qt_method!(fn(&mut self)),
    /// Articles notification covers, after `review` said publish.
    articleCount: qt_method!(fn(&self) -> i32),
    /// Unread total as of same `review`: cover's number, stated beside own when
    /// they differ. See [`vuo_core::notify`].
    unreadTotal: qt_method!(fn(&self) -> i32),

    announcer: Announcer,
    /// `None` until [`Arrivals::attach`] is called; QML never passes one.
    ctx: Option<std::rc::Rc<AppContext>>,
}

impl Arrivals {
    /// Give this object a context explicitly, instead of the installed one.
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
    }

    /// See `EntryModel::context`.
    fn context(&self) -> Option<std::rc::Rc<AppContext>> {
        self.ctx.clone().or_else(crate::context::current)
    }

    fn review(&mut self, active: bool, enabled: bool) -> i32 {
        let announcement = self.decide(active, enabled);
        encode(announcement)
    }

    /// [`Arrivals::review`], answering in the core's own type.
    fn decide(&mut self, active: bool, enabled: bool) -> Announcement {
        // No account: no mirror, nothing arrived.
        let Some(ctx) = self.context() else {
            return self.announcer.withdraw();
        };
        if active {
            acknowledge_everything(&ctx);
            return self.announcer.withdraw();
        }
        if !enabled {
            // Left flagged, not acknowledged: nobody saw them. Switch on → next
            // notification counts them; opening Vuo clears them anyway.
            return self.announcer.withdraw();
        }
        // Failed read leaves notification as is: status line on other screen;
        // next sync asks again.
        let looked = ctx.read(|db| {
            let conn = db.conn();
            store::arrivals(conn).and_then(|ids| Ok((ids, store::unread_count(conn)?)))
        });
        let Some(Ok((arrivals, unread))) = looked else {
            return Announcement::Unchanged;
        };
        self.announcer.review(&arrivals, unread)
    }

    fn dismissed(&mut self) {
        let covered = self.announcer.dismissed();
        if covered.is_empty() {
            return;
        }
        let Some(ctx) = self.context() else { return };
        let written = ctx.write(|db| db.with_tx(|tx| store::acknowledge_arrivals(tx, &covered)));
        if !matches!(written, Some(Ok(_))) {
            // Costs one notification counting few articles twice. Log line, no
            // retry loop.
            tracing::debug!("could not acknowledge a dismissed notification's articles");
        }
    }

    fn articleCount(&self) -> i32 {
        i32::try_from(self.announcer.showing()).unwrap_or(i32::MAX)
    }

    fn unreadTotal(&self) -> i32 {
        i32::try_from(self.announcer.unread()).unwrap_or(i32::MAX)
    }
}

/// Reader looking at Vuo: nothing arrived is news.
///
/// Read first. Runs on every mirror change while open (every mark-read tap);
/// write almost always has nothing to do. See [`store::any_arrivals`].
fn acknowledge_everything(ctx: &AppContext) {
    let pending = ctx
        .read(|db| store::any_arrivals(db.conn()))
        .and_then(Result::ok)
        .unwrap_or(false);
    if pending {
        let written = ctx.write(|db| db.with_tx(store::acknowledge_all_arrivals));
        if !matches!(written, Some(Ok(_))) {
            tracing::debug!(
                "could not acknowledge the articles that arrived; will retry next poll"
            );
        }
    }
}

/// The QML-facing integer for an [`Announcement`].
fn encode(announcement: Announcement) -> i32 {
    match announcement {
        Announcement::Unchanged => ANNOUNCE_NOTHING,
        Announcement::Show { banner: true } => ANNOUNCE_BANNER,
        Announcement::Show { banner: false } => ANNOUNCE_QUIET,
        Announcement::Withdraw => ANNOUNCE_WITHDRAW,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vuo_core::db::Database;
    use vuo_core::model::{Entry, EntryId, EntryStatus, Feed, FeedId};

    /// Undated, like other shim tests' entries.
    fn entry(id: i64) -> Entry {
        Entry {
            id: EntryId(id),
            feed_id: FeedId(1),
            status: EntryStatus::Unread,
            starred: false,
            title: format!("entry {id}"),
            url: None,
            comments_url: None,
            author: String::new(),
            content: String::new(),
            published_at: None,
            created_at: None,
            changed_at: None,
            reading_time: 1,
            tags: Vec::new(),
            enclosures: Vec::new(),
        }
    }

    /// As pull would: new unread entry inside window.
    fn arrive(ctx: &AppContext, id: i64) {
        ctx.write(|db| {
            db.with_tx(|tx| store::upsert_entry(tx, &entry(id), 2, store::Arrival::Announce))
        })
        .expect("a borrow")
        .expect("an arrival");
    }

    /// As pull would an already-held article, or one from before window: in
    /// mirror with `status`, not arrival.
    fn hold(ctx: &AppContext, id: i64, status: EntryStatus) {
        let e = Entry {
            status,
            ..entry(id)
        };
        ctx.write(|db| db.with_tx(|tx| store::upsert_entry(tx, &e, 2, store::Arrival::Quiet)))
            .expect("a borrow")
            .expect("an entry");
    }

    fn flagged(ctx: &AppContext) -> Vec<i64> {
        ctx.read(|db| store::arrivals(db.conn()))
            .expect("a borrow")
            .expect("arrivals")
            .iter()
            .map(|id| id.get())
            .collect()
    }

    fn with_context() -> (tempfile::TempDir, std::rc::Rc<AppContext>, Arrivals) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut db = Database::open(&dir.path().join("mirror.sqlite")).expect("mirror");
        db.with_tx(|tx| {
            store::upsert_feed(
                tx,
                &Feed {
                    id: FeedId(1),
                    category_id: None,
                    title: "A feed".to_owned(),
                    site_url: None,
                    feed_url: None,
                    icon_id: None,
                    checked_at: None,
                    parsing_error_message: String::new(),
                    parsing_error_count: 0,
                    disabled: false,
                    hide_globally: false,
                    crawler: false,
                },
                1,
            )
        })
        .expect("a feed");
        let ctx = crate::context::context_for_test(
            db,
            url::Url::parse("https://unreachable.invalid/").expect("url"),
        );
        let mut arrivals = Arrivals::default();
        arrivals.attach(std::rc::Rc::clone(&ctx));
        (dir, ctx, arrivals)
    }

    /// §the notification's whole life, from the cover to the reader's return.
    #[test]
    fn a_notification_goes_up_on_the_cover_and_comes_down_when_vuo_is_opened() {
        let (_dir, ctx, mut arrivals) = with_context();
        assert_eq!(
            arrivals.review(false, true),
            ANNOUNCE_NOTHING,
            "nothing yet"
        );

        arrive(&ctx, 1);
        arrive(&ctx, 2);
        assert_eq!(arrivals.review(false, true), ANNOUNCE_BANNER);
        assert_eq!(arrivals.articleCount(), 2);

        assert_eq!(
            arrivals.review(false, true),
            ANNOUNCE_NOTHING,
            "a poll that finds the same two says nothing"
        );

        // Reader opens Vuo.
        assert_eq!(arrivals.review(true, true), ANNOUNCE_WITHDRAW);
        assert!(
            flagged(&ctx).is_empty(),
            "and what arrived is not news any more"
        );
        assert_eq!(arrivals.review(false, true), ANNOUNCE_NOTHING);
    }

    /// §a dismissal acknowledges what the reader was told, not what came after.
    #[test]
    fn a_dismissed_notification_counts_only_what_arrives_after_it() {
        let (_dir, ctx, mut arrivals) = with_context();
        arrive(&ctx, 1);
        arrive(&ctx, 2);
        assert_eq!(arrivals.review(false, true), ANNOUNCE_BANNER);

        // Sync lands between notification going up and swipe.
        arrive(&ctx, 3);
        arrivals.dismissed();
        assert_eq!(flagged(&ctx), vec![3], "1 and 2 were covered; 3 was not");

        assert_eq!(arrivals.review(false, true), ANNOUNCE_BANNER);
        assert_eq!(arrivals.articleCount(), 1);
    }

    /// §the notification's total is the cover's number, kept in step.
    #[test]
    fn the_notification_states_the_covers_unread_total_and_follows_it() {
        let (_dir, ctx, mut arrivals) = with_context();
        // Three unread from before; two new arrive.
        for id in 1..=3 {
            hold(&ctx, id, EntryStatus::Unread);
        }
        arrive(&ctx, 4);
        arrive(&ctx, 5);
        assert_eq!(arrivals.review(false, true), ANNOUNCE_BANNER);
        assert_eq!((arrivals.articleCount(), arrivals.unreadTotal()), (2, 5));

        // Older one read on another device. Cover drops to 4; notification
        // corrected, no banner.
        hold(&ctx, 1, EntryStatus::Read);
        assert_eq!(arrivals.review(false, true), ANNOUNCE_QUIET);
        assert_eq!((arrivals.articleCount(), arrivals.unreadTotal()), (2, 4));
    }

    /// §the switch is off: nothing goes up, and nothing is thrown away.
    #[test]
    fn with_notifications_off_nothing_is_published_or_acknowledged() {
        let (_dir, ctx, mut arrivals) = with_context();
        arrive(&ctx, 1);
        assert_eq!(arrivals.review(false, false), ANNOUNCE_NOTHING);
        assert_eq!(flagged(&ctx), vec![1], "unseen, so not acknowledged");

        // Turned on: next look announces it.
        assert_eq!(arrivals.review(false, true), ANNOUNCE_BANNER);
        // Turned off while up: comes down.
        assert_eq!(arrivals.review(false, false), ANNOUNCE_WITHDRAW);
    }

    #[test]
    fn with_no_account_there_is_nothing_to_announce() {
        // QML constructs this before any account; `current()` is `None` on test
        // thread.
        let mut arrivals = Arrivals::default();
        assert_eq!(arrivals.review(false, true), ANNOUNCE_NOTHING);
        assert_eq!(arrivals.review(true, true), ANNOUNCE_NOTHING);
        arrivals.dismissed();
        assert_eq!(arrivals.articleCount(), 0);
        assert_eq!(arrivals.unreadTotal(), 0);
    }

    /// §the QML's names for the answers are the Rust answers.
    ///
    /// QML can't see Rust const (no `qml_register_enum` on Qt 5.6): root window
    /// restates these; restatement = copy that drifts. Swapping two → banner
    /// for every article read elsewhere, or notification never comes down; both
    /// sides plain integers, nothing else notices.
    #[test]
    fn the_root_window_names_each_answer_with_the_same_number() {
        const WINDOW: &str = include_str!("../../../qml/harbour-vuo.qml");
        for (name, value) in [
            ("announceNothing", ANNOUNCE_NOTHING),
            ("announceBanner", ANNOUNCE_BANNER),
            ("announceQuiet", ANNOUNCE_QUIET),
            ("announceWithdraw", ANNOUNCE_WITHDRAW),
        ] {
            let declaration = format!("readonly property int {name}: {value}");
            assert!(
                WINDOW.contains(&declaration),
                "harbour-vuo.qml must declare `{declaration}`"
            );
        }
    }

    #[test]
    fn every_announcement_has_its_own_integer() {
        let all = [
            encode(Announcement::Unchanged),
            encode(Announcement::Show { banner: true }),
            encode(Announcement::Show { banner: false }),
            encode(Announcement::Withdraw),
        ];
        assert_eq!(
            all,
            [
                ANNOUNCE_NOTHING,
                ANNOUNCE_BANNER,
                ANNOUNCE_QUIET,
                ANNOUNCE_WITHDRAW
            ]
        );
    }
}
