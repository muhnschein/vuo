//! What the new-articles notification needs from the mirror.
//!
//! The notification itself is QML's to raise -- `Nemo.Notifications` is a QML
//! module, and the home screen is reached through it -- so this object does
//! what the models do: it reads the mirror when asked, and hands back plain
//! values. The decision of what to tell the home screen is
//! [`vuo_core::notify::Announcer`]'s, where it is tested without Qt; this is
//! the thin wrapper that feeds it and remembers its answer.
//!
//! # When it is asked
//!
//! By the root window, each time its poll finds that the mirror changed, and
//! each time the app comes to the front or goes to the cover. There is no push
//! from the worker, for the reason there is none into the models: QML owns this
//! object, so Rust has no handle to call into. The consequence is worth
//! stating, because it is visible: while Vuo is on its cover, the poll runs at
//! a quarter of the sync interval (`settings::cover_poll_ms_for`), so a
//! notification can arrive up to that long after the sync that found the
//! articles -- exactly as late as the cover's own count, and for the same
//! reason.

// See the note at the top of settings.rs: the `QObject` derive for a plain
// `QObject` base expands to glue clippy reads as a useless transmute.
#![allow(clippy::useless_transmute)]

use qmetaobject::{qt_base_class, qt_method, QObject};
use vuo_core::db::store;
use vuo_core::notify::{Announcement, Announcer};

use crate::context::AppContext;

/// What [`Arrivals::review`] tells QML to do. Integers, because this
/// `qmetaobject` has no `qml_register_enum` on Qt 5.6 (see the crate docs).
///
/// Leave the notification as it is.
pub const ANNOUNCE_NOTHING: i32 = 0;
/// Publish it, with a banner: something new arrived.
pub const ANNOUNCE_BANNER: i32 = 1;
/// Publish it without a banner: the count changed, but nothing new arrived.
pub const ANNOUNCE_QUIET: i32 = 2;
/// Close it.
pub const ANNOUNCE_WITHDRAW: i32 = 3;

#[derive(QObject, Default)]
pub struct Arrivals {
    base: qt_base_class!(trait QObject),

    /// Look at the mirror and say what the notification should do: one of
    /// the `ANNOUNCE_*` constants.
    ///
    /// `active` is whether the reader has Vuo in front of them. While they
    /// do, nothing is news -- the list is right there -- so every arrival is
    /// acknowledged as it lands and whatever notification is up comes down.
    /// `enabled` is the Settings switch.
    review: qt_method!(fn(&mut self, active: bool, enabled: bool) -> i32),
    /// The home screen closed the notification: the reader swiped it away,
    /// cleared the lot, or tapped it. Acknowledges exactly the articles it
    /// covered, so the next one counts only what came after.
    dismissed: qt_method!(fn(&mut self)),
    /// How many articles the notification covers, after a `review` that
    /// said to publish.
    /// It says this and nothing else -- see [`vuo_core::notify`].
    articleCount: qt_method!(fn(&self) -> i32),

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
        // No account yet, so no mirror and nothing that could have arrived.
        let Some(ctx) = self.context() else {
            return self.announcer.withdraw();
        };
        if active {
            acknowledge_everything(&ctx);
            return self.announcer.withdraw();
        }
        if !enabled {
            // Left flagged rather than acknowledged. Nobody has seen them; if
            // the switch is turned on, the next arrival's notification counts
            // them too -- and opening Vuo clears them either way.
            return self.announcer.withdraw();
        }
        // A read that fails leaves the notification as it is. It is a status
        // line on another screen, and the next sync asks again.
        let Some(Ok(arrivals)) = ctx.read(|db| store::arrivals(db.conn())) else {
            return Announcement::Unchanged;
        };
        self.announcer.review(&arrivals)
    }

    fn dismissed(&mut self) {
        let covered = self.announcer.dismissed();
        if covered.is_empty() {
            return;
        }
        let Some(ctx) = self.context() else { return };
        let written = ctx.write(|db| db.with_tx(|tx| store::acknowledge_arrivals(tx, &covered)));
        if !matches!(written, Some(Ok(_))) {
            // Costs one notification that counts a few articles twice. Not
            // worth more than a log line, and not worth a retry loop.
            tracing::debug!("could not acknowledge a dismissed notification's articles");
        }
    }

    fn articleCount(&self) -> i32 {
        i32::try_from(self.announcer.showing()).unwrap_or(i32::MAX)
    }
}

/// The reader is looking at Vuo: nothing that has arrived is news any more.
///
/// Checked with a read first. This runs every time the mirror changes while
/// the app is open -- every tap that marks something read -- and the write
/// almost always has nothing to do. See [`store::any_arrivals`].
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

    /// Undated, as the other shim tests' entries are.
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

    /// As the pull would: a new, unread entry inside the window.
    fn arrive(ctx: &AppContext, id: i64) {
        ctx.write(|db| {
            db.with_tx(|tx| store::upsert_entry(tx, &entry(id), 2, store::Arrival::Announce))
        })
        .expect("a borrow")
        .expect("an arrival");
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

        // The reader opens Vuo.
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

        // A sync lands between the notification going up and the swipe.
        arrive(&ctx, 3);
        arrivals.dismissed();
        assert_eq!(flagged(&ctx), vec![3], "1 and 2 were covered; 3 was not");

        assert_eq!(arrivals.review(false, true), ANNOUNCE_BANNER);
        assert_eq!(arrivals.articleCount(), 1);
    }

    /// §the switch is off: nothing goes up, and nothing is thrown away.
    #[test]
    fn with_notifications_off_nothing_is_published_or_acknowledged() {
        let (_dir, ctx, mut arrivals) = with_context();
        arrive(&ctx, 1);
        assert_eq!(arrivals.review(false, false), ANNOUNCE_NOTHING);
        assert_eq!(flagged(&ctx), vec![1], "unseen, so not acknowledged");

        // Turned on: the next look announces it.
        assert_eq!(arrivals.review(false, true), ANNOUNCE_BANNER);
        // Turned off while it is up: it comes down.
        assert_eq!(arrivals.review(false, false), ANNOUNCE_WITHDRAW);
    }

    #[test]
    fn with_no_account_there_is_nothing_to_announce() {
        // QML constructs this before any account exists; `current()` is
        // `None` on a test thread.
        let mut arrivals = Arrivals::default();
        assert_eq!(arrivals.review(false, true), ANNOUNCE_NOTHING);
        assert_eq!(arrivals.review(true, true), ANNOUNCE_NOTHING);
        arrivals.dismissed();
        assert_eq!(arrivals.articleCount(), 0);
    }

    /// §the QML's names for the answers are the Rust answers.
    ///
    /// QML cannot see a Rust const -- there is no `qml_register_enum` on Qt
    /// 5.6 -- so the root window restates these, and a restatement is a copy
    /// that can drift. Swap two and a banner pops for every article read on
    /// another device, or the notification never comes down; nothing else in
    /// the build would notice, because both sides are just integers.
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
