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
//! [`store::upsert_entry`] -- and recorded with it, so it survives a pass that
//! fails part-way. This module decides the rest: what the home screen should
//! be told, given what the mirror says now and what it was last told.
//!
//! # Where the text goes, and why it is shaped here
//!
//! A notification is drawn by the HOME SCREEN, which is another process. Every
//! other piece of foreign text Vuo shows goes through a `Text` whose
//! `textFormat` Vuo sets itself (§9.3); this one goes somewhere Vuo cannot set
//! anything. The freedesktop notification spec lets a server interpret markup
//! in a notification's body -- `<b>`, `<a href>`, and `<img src>` among it --
//! and the home screen is that server. A feed title that reached it verbatim
//! would be a feed operator choosing markup for a system surface, and an
//! `<img>` there is the IP leak §9.3 exists to prevent, fetched by a process
//! the media proxy knows nothing about.
//!
//! So nothing that could open or close a tag leaves Vuo:
//! [`notification_line`] replaces both angle brackets with look-alikes that no
//! markup parser treats as syntax. That is not a sanitiser in the sense §3
//! rules out -- there is no allowlist of tags here, because no tag survives.
//! Entities are left alone: a decoded entity is a character, never a tag, so
//! `&lt;img&gt;` renders at worst as the text "<img>".

use std::collections::HashSet;

use crate::db::store;
use crate::model::EntryId;

/// How many titles a notification lists.
///
/// The home screen shows a notification's body in a few lines at most, and a
/// fourth title would be cut off rather than read.
pub const HEADLINES: i64 = 3;

/// The longest a single line may be, in characters, before it is cut.
///
/// A feed title has no length limit anyone enforces. The home screen elides a
/// long line anyway; this bounds what is sent to it, which crosses a process
/// boundary on every sync that finds something.
pub const MAX_LINE_CHARS: usize = 120;

/// One line of foreign text, made fit for a surface Vuo does not control.
///
/// - `<` and `>` become `‹` and `›`, so no markup parser can find a tag.
/// - Every run of whitespace, line breaks included, becomes one space: a
///   title is one line, and a feed that puts a newline in one does not get to
///   push the next title off the notification.
/// - Control characters and bidirectional overrides are dropped. The former
///   draw as boxes or not at all; the latter can make a title render
///   backwards, which in a system notification is a spoof rather than a
///   typographical choice.
/// - The result is cut to [`MAX_LINE_CHARS`] on a character boundary, with an
///   ellipsis when anything was cut.
///
/// `None` when nothing is left, so a feed with empty titles adds no blank line.
#[must_use]
pub fn notification_line(text: &str) -> Option<String> {
    let mut line = String::with_capacity(text.len().min(MAX_LINE_CHARS * 4));
    let mut chars = 0usize;
    let mut pending_space = false;
    let mut cut = false;

    for c in text.chars() {
        let c = match c {
            '<' => '‹',
            '>' => '›',
            // LRE, RLE, PDF, LRO, RLO and LRI, RLI, FSI, PDI.
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => continue,
            c if c.is_whitespace() => {
                pending_space = chars > 0;
                continue;
            }
            c if c.is_control() => continue,
            c => c,
        };
        let needed = if pending_space { 2 } else { 1 };
        if chars + needed > MAX_LINE_CHARS {
            cut = true;
            break;
        }
        if pending_space {
            line.push(' ');
            chars += 1;
            pending_space = false;
        }
        line.push(c);
        chars += 1;
    }

    if line.is_empty() {
        return None;
    }
    if cut {
        // The ellipsis replaces the last character rather than going past the
        // cap, so the cap is what it says. A line cut at a space is one short
        // of the cap already, and has room for it as it is.
        if chars >= MAX_LINE_CHARS {
            line.pop();
        }
        line.push('…');
    }
    Some(line)
}

/// The two pieces of foreign text a notification carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationText {
    /// The newest title, for the banner that pops up.
    pub headline: String,
    /// The newest few, one per line, for the notification that stays.
    pub digest: String,
}

impl NotificationText {
    /// Build both from the titles [`store::arrivals`] read, newest first.
    #[must_use]
    pub fn from_titles(titles: &[String]) -> Self {
        let lines: Vec<String> = titles
            .iter()
            .filter_map(|t| notification_line(t))
            .take(usize::try_from(HEADLINES).unwrap_or(0))
            .collect();
        NotificationText {
            headline: lines.first().cloned().unwrap_or_default(),
            digest: lines.join("\n"),
        }
    }
}

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
/// acknowledges exactly those (see [`store::acknowledge_arrivals`]), and "the
/// same count" is not "the same articles" -- one read elsewhere while another
/// arrived leaves the count where it was and is still news.
#[derive(Debug, Clone, Default)]
pub struct Announcer {
    showing: Vec<EntryId>,
}

impl Announcer {
    /// Decide what to do about `arrivals`, the ids [`store::arrivals`] read.
    pub fn review(&mut self, arrivals: &store::Arrivals) -> Announcement {
        if arrivals.ids.is_empty() {
            return self.withdraw();
        }
        let shown: HashSet<EntryId> = self.showing.iter().copied().collect();
        let news = arrivals.ids.iter().any(|id| !shown.contains(id));
        let moved = news || arrivals.ids.len() != self.showing.len();
        self.showing.clone_from(&arrivals.ids);
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

    fn arrivals(ids: &[i64]) -> store::Arrivals {
        store::Arrivals {
            ids: ids.iter().copied().map(EntryId).collect(),
            titles: Vec::new(),
        }
    }

    /// §nothing that can open a tag reaches the home screen.
    ///
    /// The whole reason this function exists. `<img src>` in a notification
    /// body is a remote fetch by the home screen, which knows nothing of the
    /// media proxy, on every sync that brings the article.
    #[test]
    fn no_markup_survives_into_a_notification() {
        for hostile in [
            "<img src=\"https://tracker.example/p.gif\">",
            "Breaking: <a href=\"https://evil.example/\">click</a>",
            "<b>bold</b> claims",
            "<<script>>alert(1)<</script>>",
            "a <\u{0}img> split by a NUL",
        ] {
            let line = notification_line(hostile).expect("text survives");
            assert!(
                !line.contains('<') && !line.contains('>'),
                "{hostile:?} became {line:?}"
            );
        }
        assert_eq!(
            notification_line("<img src=x>").as_deref(),
            Some("‹img src=x›"),
            "and what was there is still readable"
        );
        // An entity decodes to a character, never to a tag, so it is left for
        // whoever renders it to decide.
        assert_eq!(
            notification_line("AT&amp;T &lt;3").as_deref(),
            Some("AT&amp;T &lt;3")
        );
    }

    #[test]
    fn a_title_is_one_line_whatever_the_feed_put_in_it() {
        assert_eq!(
            notification_line("  Two\n\nlines\tand\r\n  some   space  ").as_deref(),
            Some("Two lines and some space")
        );
        // Unicode line and paragraph separators are whitespace too.
        assert_eq!(
            notification_line("one\u{2028}two\u{2029}three").as_deref(),
            Some("one two three")
        );
        assert_eq!(
            notification_line("bell\u{7}and\u{1b}[31mescape").as_deref(),
            Some("belland[31mescape"),
            "control characters are dropped"
        );
        assert_eq!(
            notification_line("\u{202E}txet desrever\u{202C} and \u{2067}isolate\u{2069}")
                .as_deref(),
            Some("txet desrever and isolate"),
            "bidirectional overrides and isolates are dropped"
        );
    }

    #[test]
    fn nothing_left_is_no_line_at_all() {
        for empty in ["", "   ", "\n\t", "\u{202E}\u{202C}", "\u{0}\u{7}"] {
            assert_eq!(notification_line(empty), None, "{empty:?}");
        }
    }

    #[test]
    fn a_long_title_is_cut_to_the_cap_on_a_character_boundary() {
        let long = "ä".repeat(MAX_LINE_CHARS * 3);
        let line = notification_line(&long).expect("a line");
        assert_eq!(line.chars().count(), MAX_LINE_CHARS);
        assert!(line.ends_with('…'));

        // Exactly at the cap: nothing cut, no ellipsis.
        let exact = "x".repeat(MAX_LINE_CHARS);
        assert_eq!(notification_line(&exact).as_deref(), Some(exact.as_str()));

        // A space that would land on the cap is not kept as a trailing one.
        let spaced = format!("{} tail", "y".repeat(MAX_LINE_CHARS - 1));
        let line = notification_line(&spaced).expect("a line");
        assert_eq!(line.chars().count(), MAX_LINE_CHARS);
        assert!(!line.contains(' '), "{line:?}");
    }

    #[test]
    fn the_digest_lists_the_newest_few_and_skips_empty_titles() {
        let titles: Vec<String> = ["", "Newest", "  ", "Second\nline", "Third", "Fourth"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let text = NotificationText::from_titles(&titles);
        assert_eq!(text.headline, "Newest");
        assert_eq!(text.digest, "Newest\nSecond line\nThird");

        assert_eq!(
            NotificationText::from_titles(&[]),
            NotificationText::default()
        );
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
