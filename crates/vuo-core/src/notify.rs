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
//! How many, and nothing more: no titles, no body, no badge. A digest of the
//! newest titles was tried on a device and read as clutter on the home screen;
//! the count is the news, and the articles are in Vuo. It also means no feed's
//! text reaches the home screen, which is another process: it reads markup in
//! a notification's body, and Vuo cannot set how it renders anything (§9.3).

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
    /// cover. A notification whose count merely went DOWN -- articles read on
    /// another device -- is corrected in place, without interrupting anyone to
    /// say that there is now less to read.
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
}

impl Announcer {
    /// Decide what to do about `arrivals`, the ids
    /// [`crate::db::store::arrivals`] read.
    pub fn review(&mut self, arrivals: &[EntryId]) -> Announcement {
        if arrivals.is_empty() {
            return self.withdraw();
        }
        let shown: HashSet<EntryId> = self.showing.iter().copied().collect();
        let news = arrivals.iter().any(|id| !shown.contains(id));
        let moved = news || arrivals.len() != self.showing.len();
        self.showing = arrivals.to_vec();
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
            Announcement::Withdraw
        }
    }

    /// The home screen closed the notification -- the reader swiped it away,
    /// or tapped it. Returns the ids it covered, which are now acknowledged.
    pub fn dismissed(&mut self) -> Vec<EntryId> {
        std::mem::take(&mut self.showing)
    }

    /// How many articles the notification that is up covers; 0 when none is.
    #[must_use]
    pub fn showing(&self) -> usize {
        self.showing.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrivals(ids: &[i64]) -> Vec<EntryId> {
        ids.iter().copied().map(EntryId).collect()
    }

    /// §a banner is for news, and only for news.
    #[test]
    fn the_announcer_pops_a_banner_only_when_something_new_arrives() {
        let mut a = Announcer::default();
        assert_eq!(
            a.review(&arrivals(&[])),
            Announcement::Unchanged,
            "nothing up, nothing new"
        );

        assert_eq!(
            a.review(&arrivals(&[1, 2])),
            Announcement::Show { banner: true }
        );
        assert_eq!(a.showing(), 2);
        assert_eq!(
            a.review(&arrivals(&[1, 2])),
            Announcement::Unchanged,
            "the same two again is not news, and not worth a repaint"
        );

        // 1 read on another device: fewer to read, which is not news.
        assert_eq!(
            a.review(&arrivals(&[2])),
            Announcement::Show { banner: false }
        );
        // 3 arrives while 2 is still unread: news.
        assert_eq!(
            a.review(&arrivals(&[3, 2])),
            Announcement::Show { banner: true }
        );
        // The same COUNT, different articles: 2 read, 4 arrived. Still news.
        assert_eq!(
            a.review(&arrivals(&[4, 3])),
            Announcement::Show { banner: true }
        );

        // Everything read elsewhere: take it down.
        assert_eq!(a.review(&arrivals(&[])), Announcement::Withdraw);
        assert_eq!(a.showing(), 0);
    }

    #[test]
    fn withdrawing_is_idempotent_and_a_dismissal_hands_back_what_it_covered() {
        let mut a = Announcer::default();
        assert_eq!(
            a.withdraw(),
            Announcement::Unchanged,
            "nothing to take down"
        );

        a.review(&arrivals(&[5, 6]));
        assert_eq!(a.withdraw(), Announcement::Withdraw);
        assert_eq!(a.withdraw(), Announcement::Unchanged);

        a.review(&arrivals(&[5, 6]));
        assert_eq!(a.dismissed(), vec![EntryId(5), EntryId(6)]);
        assert_eq!(a.showing(), 0, "the home screen has closed it");
        assert!(a.dismissed().is_empty(), "and one dismissal is one");

        // After a dismissal the next arrival is news again, even alongside one
        // the dismissal did not acknowledge.
        assert_eq!(
            a.review(&arrivals(&[7])),
            Announcement::Show { banner: true }
        );
    }
}
