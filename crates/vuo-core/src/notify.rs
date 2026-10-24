//! Telling reader new articles arrived.
//!
//! Scope milestone 5: *"a cover showing unread count; notification on new entries."* This
//! = notification half.
//!
//! # What counts as new
//!
//! New = sync brought it into mirror for first time, unread, since reader last had Vuo
//! open. First half decided where row lands ([`crate::sync::pull::arrival_for`],
//! [`crate::db::store::upsert_entry`]) and recorded with it: survives pass failing
//! part-way. This module decides rest: what home screen is told, given mirror now and
//! what it was last told.
//!
//! # What it says
//!
//! Numbers only: no titles, body, badge. Count is the news; articles are in Vuo. Also no
//! feed text reaches home screen: other process, reads markup in body, Vuo can't set
//! rendering (§9.3).
//!
//! Two numbers when they differ: arrived, and unread total (cover's number). Without
//! total, three left unread plus eleven new read "11 new articles" on home screen vs 14
//! on cover; reader guesses which is wrong. Total kept in step with cover:
//! [`Announcer::review`].

use std::collections::HashSet;

use crate::model::EntryId;

/// What the home screen should be told after a look at the mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Announcement {
    /// Leave whatever is showing as it is.
    Unchanged,
    /// Put notification up, or replace one up.
    ///
    /// `banner`: also pops up over what reader is doing. Only for news: arrival
    /// showing notification didn't cover. Numbers merely moving (read on
    /// another device) → corrected in place, no interruption for "less to
    /// read".
    Show {
        /// Whether it also pops up over whatever the reader is doing.
        banner: bool,
    },
    /// Take the notification down.
    Withdraw,
}

/// Remembers what notification up says: next look tells news from repeat.
///
/// Holds covered ids, not count: dismissal acknowledges exactly those (see
/// [`crate::db::store::acknowledge_arrivals`]); same count ≠ same articles (one
/// read elsewhere while another arrived = count unchanged, still news).
#[derive(Debug, Clone, Default)]
pub struct Announcer {
    showing: Vec<EntryId>,
    /// The unread total the notification that is up states; 0 when none is.
    unread: i64,
}

impl Announcer {
    /// Decide about `arrivals` (ids from [`crate::db::store::arrivals`]) when
    /// `unread` articles unread in all ([`crate::db::store::unread_count`],
    /// cover's number).
    ///
    /// Total alone moving = quiet update, never banner: old article read
    /// elsewhere changes cover, so notification, but isn't news.
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

    /// Reader has Vuo open, or notifications off: take down whatever is up.
    pub fn withdraw(&mut self) -> Announcement {
        if self.showing.is_empty() {
            Announcement::Unchanged
        } else {
            self.showing.clear();
            self.unread = 0;
            Announcement::Withdraw
        }
    }

    /// Home screen closed notification (swiped or tapped). Returns covered ids,
    /// now acknowledged.
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
    /// Three older articles unread throughout: total always arrivals + 3.
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

        // 1 read on another device: fewer to read, not news.
        assert_eq!(
            a.review(&arrivals(&[2]), 4),
            Announcement::Show { banner: false }
        );
        // 3 arrives while 2 still unread: news.
        assert_eq!(
            a.review(&arrivals(&[3, 2]), 5),
            Announcement::Show { banner: true }
        );
        // Same COUNT, different articles: 2 read, 4 arrived. Still news.
        assert_eq!(
            a.review(&arrivals(&[4, 3]), 5),
            Announcement::Show { banner: true }
        );

        // Everything read elsewhere: take down.
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

        // Older article read elsewhere: cover says 4, notification must too;
        // nothing new.
        assert_eq!(
            a.review(&arrivals(&[1, 2]), 4),
            Announcement::Show { banner: false }
        );
        assert_eq!(a.unread(), 4);
        assert_eq!(a.review(&arrivals(&[1, 2]), 4), Announcement::Unchanged);

        // Old article marked unread again, not arrival: more to read, not news.
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

        // After dismissal next arrival is news again, even beside one dismissal
        // didn't acknowledge.
        assert_eq!(
            a.review(&arrivals(&[7]), 3),
            Announcement::Show { banner: true }
        );
    }
}
