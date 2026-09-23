//! Telling the reader that new articles have arrived.
//!
//! Milestone 5 of the scope: *"a cover showing unread count; notification on
//! new entries."* The cover was done; this is the other half.
//!
//! # What counts as new
//!
//! An article is new when a sync brought it into the mirror for the first
//! time, unread, since the reader last had Vuo open. The first half of that is
//! decided where the row lands -- see [`crate::sync::pull::arrival_for`] and
//! [`crate::db::store::upsert_entry`] -- and recorded with it, so it survives a pass that
//! fails part-way. This module decides the rest: what the home screen should
//! be told, given what the mirror says now and what it was last told.
//!
//! # What it says
//!
//! Numbers, and nothing more: no titles, no body, no badge. A digest of the
//! newest titles was tried on a device and read as clutter on the home screen;
//! the count is the news, and the articles are in Vuo. It also means no feed's
//! text reaches the home screen, which is another process: it reads markup in
//! a notification's body, and Vuo cannot set how it renders anything (§9.3).
//!
//! Two numbers, though, when they differ: how many arrived, and how many are
//! unread in all. The second is the cover's own number. Without it, three
//! articles left unread and eleven new ones read "11 new articles" on the home
//! screen and 14 on the cover, and the reader is left to work out which is
//! wrong. So the notification carries the total too, and keeps it in step
//! with the cover -- see [`Announcer::review`].

use std::collections::HashSet;

use crate::model::EntryId;

/// What the home screen should be told after a look at the mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Announcement {
    /// Leave whatever is showing as it is.
    Unchanged,
    /// Put the notification up, or replace the one that is up.
    ///
    /// `banner` is whether it also pops up over whatever the reader is doing.
    /// Only for news: something arrived that the showing notification did not
    /// cover. A notification whose numbers merely moved -- articles read on
    /// another device, new or old -- is corrected in place, without
    /// interrupting anyone to say that there is now less to read.
    Show { banner: bool },
    /// Take the notification down.
    Withdraw,
}

/// Remembers what the notification that is up says, so the next look at the
/// mirror can tell news from a repeat.
///
/// Holds the ids the notification covers, not just how many: a dismissal
/// acknowledges exactly those (see
/// [`crate::db::store::acknowledge_arrivals`]), and "the
/// same count" is not "the same articles" -- one read elsewhere while another
/// arrived leaves the count where it was and is still news.
#[derive(Debug, Clone, Default)]
pub struct Announcer {
    showing: Vec<EntryId>,
    /// The unread total the notification that is up states; 0 when none is.
    unread: i64,
}

impl Announcer {
    /// Decide what to do about `arrivals`, the ids
    /// [`crate::db::store::arrivals`] read, when `unread` articles are unread
    /// in all ([`crate::db::store::unread_count`], the cover's number).
    ///
    /// The total alone moving is a quiet update, never a banner: an old
    /// article read on another device changes what the cover says, so it
    /// changes what the notification says, but it is not news.
    pub fn review(&mut self, arrivals: &[EntryId], unread: i64) -> Announcement {
        if arrivals.is_empty() {
            return self.withdraw();
        }
        let shown: HashSet<EntryId> = self.showing.iter().copied().collect();
        let news = arrivals.iter().any(|id| !shown.contains(id));
        let moved = news || arrivals.len() != self.showing.len() || unread != self.unread;
        self.showing = arrivals.to_vec();
        self.unread = unread;
        if news {
            Announcement::Show { banner: true }
        } else if moved {
            Announcement::Show { banner: false }
        } else {
            Announcement::Unchanged
        }
    }

    /// The reader has Vuo open, or has turned notifications off: take down
    /// whatever is up.
    pub fn withdraw(&mut self) -> Announcement {
        if self.showing.is_empty() {
            Announcement::Unchanged
        } else {
            self.showing.clear();
            self.unread = 0;
            Announcement::Withdraw
        }
    }

    /// The home screen closed the notification -- the reader swiped it away,
    /// or tapped it. Returns the ids it covered, which are now acknowledged.
    pub fn dismissed(&mut self) -> Vec<EntryId> {
        self.unread = 0;
        std::mem::take(&mut self.showing)
    }

    /// How many articles the notification that is up covers; 0 when none is.
    #[must_use]
    pub fn showing(&self) -> usize {
        self.showing.len()
    }

    /// The unread total the notification that is up states; 0 when none is.
    #[must_use]
    pub fn unread(&self) -> i64 {
        self.unread
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrivals(ids: &[i64]) -> Vec<EntryId> {
        ids.iter().copied().map(EntryId).collect()
    }

    /// §a banner is for news, and only for news.
    ///
    /// Three older articles are unread throughout, so the total is always
    /// three more than the arrivals.
    #[test]
    fn the_announcer_pops_a_banner_only_when_something_new_arrives() {
        let mut a = Announcer::default();
        assert_eq!(
            a.review(&arrivals(&[]), 3),
            Announcement::Unchanged,
            "nothing up, nothing new"
        );

        assert_eq!(
            a.review(&arrivals(&[1, 2]), 5),
            Announcement::Show { banner: true }
        );
        assert_eq!((a.showing(), a.unread()), (2, 5));
        assert_eq!(
            a.review(&arrivals(&[1, 2]), 5),
            Announcement::Unchanged,
            "the same two again is not news, and not worth a repaint"
        );

        // 1 read on another device: fewer to read, which is not news.
        assert_eq!(
            a.review(&arrivals(&[2]), 4),
            Announcement::Show { banner: false }
        );
        // 3 arrives while 2 is still unread: news.
        assert_eq!(
            a.review(&arrivals(&[3, 2]), 5),
            Announcement::Show { banner: true }
        );
        // The same COUNT, different articles: 2 read, 4 arrived. Still news.
        assert_eq!(
            a.review(&arrivals(&[4, 3]), 5),
            Announcement::Show { banner: true }
        );

        // Everything read elsewhere: take it down.
        assert_eq!(a.review(&arrivals(&[]), 3), Announcement::Withdraw);
        assert_eq!((a.showing(), a.unread()), (0, 0));
    }

    /// §the notification's total is the cover's number, kept in step.
    #[test]
    fn the_total_alone_moving_is_a_quiet_update() {
        let mut a = Announcer::default();
        assert_eq!(
            a.review(&arrivals(&[1, 2]), 5),
            Announcement::Show { banner: true }
        );

        // One of the three older articles read on another device: the cover
        // now says 4, so the notification has to as well -- but nothing new
        // has arrived.
        assert_eq!(
            a.review(&arrivals(&[1, 2]), 4),
            Announcement::Show { banner: false }
        );
        assert_eq!(a.unread(), 4);
        assert_eq!(a.review(&arrivals(&[1, 2]), 4), Announcement::Unchanged);

        // An old article marked unread again comes back without being an
        // arrival: more to read, but still not news.
        assert_eq!(
            a.review(&arrivals(&[1, 2]), 6),
            Announcement::Show { banner: false }
        );
    }

    #[test]
    fn withdrawing_is_idempotent_and_a_dismissal_hands_back_what_it_covered() {
        let mut a = Announcer::default();
        assert_eq!(
            a.withdraw(),
            Announcement::Unchanged,
            "nothing to take down"
        );

        a.review(&arrivals(&[5, 6]), 2);
        assert_eq!(a.withdraw(), Announcement::Withdraw);
        assert_eq!(a.withdraw(), Announcement::Unchanged);

        a.review(&arrivals(&[5, 6]), 2);
        assert_eq!(a.dismissed(), vec![EntryId(5), EntryId(6)]);
        assert_eq!(
            (a.showing(), a.unread()),
            (0, 0),
            "the home screen has closed it"
        );
        assert!(a.dismissed().is_empty(), "and one dismissal is one");

        // After a dismissal the next arrival is news again, even alongside one
        // the dismissal did not acknowledge.
        assert_eq!(
            a.review(&arrivals(&[7]), 3),
            Announcement::Show { banner: true }
        );
    }
}
