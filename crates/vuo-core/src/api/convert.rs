//! Validate wire types into domain types, one item at a time.
//!
//! Failures are [`Error::Item`], not general errors: §9.2 says reject the
//! malformed item, never stall sync or outbox.
//!
//! Key distinction: field whose absence loses the item vs. only itself. Missing
//! `status` = row unrepresentable, row goes. Unparseable `url` = no tappable
//! link, text still shown.

use chrono::{DateTime, FixedOffset, Utc};

use crate::api::wire;
use crate::content::MediaUrl;
use crate::error::{Error, Result};
use crate::model::{
    Category, CategoryId, Enclosure, Entry, EntryId, EntryStatus, Feed, FeedId, IconId,
};

fn to_utc(t: Option<DateTime<FixedOffset>>) -> Option<DateTime<Utc>> {
    t.map(|t| t.with_timezone(&Utc))
}

/// Parse URL; `None`, not error.
///
/// Every foreign URL field. `javascript:` or malformed = "no link": only safe
/// rendering, better than dropping entry.
fn lenient_url(raw: &str) -> Option<MediaUrl> {
    if raw.trim().is_empty() {
        return None;
    }
    MediaUrl::parse(raw)
}

/// Validate one wire entry into domain [`Entry`].
///
/// Unparseable URLs → `None`, negative reading times → 0; only status can cost
/// the entry.
///
/// # Errors
///
/// [`Error::Item`] when status is `removed`, missing, or not `read`/`unread`.
pub fn entry(w: wire::Entry) -> Result<Entry> {
    let id = EntryId(w.id);

    // Status: only field whose absence makes row unrepresentable.
    let status = match w.status.as_str() {
        "unread" => EntryStatus::Unread,
        "read" => EntryStatus::Read,
        // Pre-2.3 servers send this for deleted entries. Not a mirrored state:
        // row shouldn't exist locally. Reject = sync skips it.
        "removed" => {
            return Err(Error::item(
                "entry",
                Some(w.id),
                "server reports it as removed",
            ))
        }
        other => {
            return Err(Error::item(
                "entry",
                Some(w.id),
                format!("unrecognised status {other:?}"),
            ))
        }
    };

    Ok(Entry {
        id,
        feed_id: FeedId(w.feed_id),
        status,
        starred: w.starred,
        title: w.title,
        url: lenient_url(&w.url),
        comments_url: lenient_url(&w.comments_url),
        author: w.author,
        content: w.content,
        published_at: to_utc(w.published_at),
        created_at: to_utc(w.created_at),
        changed_at: to_utc(w.changed_at),
        // Negative reading time is nonsense; clamp, don't reject.
        reading_time: w.reading_time.max(0),
        tags: w.tags,
        enclosures: w.enclosures.into_iter().map(|e| enclosure(e, id)).collect(),
    })
}

fn enclosure(w: wire::Enclosure, entry_id: EntryId) -> Enclosure {
    Enclosure {
        id: w.id,
        entry_id,
        url: lenient_url(&w.url),
        mime_type: w.mime_type,
        size: w.size.max(0),
    }
}

/// Validate one wire feed into domain [`Feed`].
///
/// Icon id `0` = no icon; unparseable URLs → `None`.
///
/// # Errors
///
/// Never fails now; `Result` matches [`entry`] so a future check can reject
/// with [`Error::Item`].
pub fn feed(w: wire::Feed) -> Result<Feed> {
    Ok(Feed {
        id: FeedId(w.id),
        category_id: w.category.as_ref().map(|c| CategoryId(c.id)),
        title: w.title,
        site_url: lenient_url(&w.site_url),
        feed_url: lenient_url(&w.feed_url),
        // `icon` present-and-null = no icon; `icon_id` 0 same.
        icon_id: w
            .icon
            .as_ref()
            .map(|i| i.icon_id)
            .filter(|id| *id != 0)
            .map(IconId),
        checked_at: to_utc(w.checked_at),
        parsing_error_message: w.parsing_error_message,
        parsing_error_count: w.parsing_error_count.max(0),
        disabled: w.disabled,
        hide_globally: w.hide_globally,
        crawler: w.crawler,
    })
}

/// Validate one wire category into domain [`Category`].
///
/// # Errors
///
/// Never fails now; `Result` matches [`entry`] so a future check can reject
/// with [`Error::Item`].
pub fn category(w: wire::Category) -> Result<Category> {
    Ok(Category {
        id: CategoryId(w.id),
        title: w.title,
        hide_globally: w.hide_globally,
    })
}

/// What a page of entries decoded into.
#[derive(Debug, Default)]
pub struct Page {
    /// Entries that validated.
    pub valid: Vec<Entry>,
    /// Ids server reports as `removed`.
    ///
    /// Pre-2.3 server soft-deletes by setting `status` to `removed`: *only*
    /// deletion signal it gives. Must become local DELETE, else entry stays in
    /// mirror forever.
    pub removed: Vec<EntryId>,
    /// Per-item errors, for logging or counting.
    pub rejected: Vec<Error>,
}

/// Convert page of entries: usable, deleted, bad.
#[must_use]
pub fn entries(page: Vec<wire::Entry>) -> Page {
    // Removals collected FIRST, and win.
    //
    // Page can carry same id twice (content + `removed`). In both lists,
    // outcome would depend on caller's apply order. "Shouldn't exist" is
    // stronger claim and safe direction: worst case next sync brings entry
    // back, never a deleted one reappearing.
    let removed: std::collections::BTreeSet<i64> = page
        .iter()
        .filter(|w| w.status == "removed")
        .map(|w| w.id)
        .collect();

    let mut out = Page {
        valid: Vec::with_capacity(page.len()),
        removed: removed.iter().copied().map(EntryId).collect(),
        ..Page::default()
    };
    for w in page {
        if removed.contains(&w.id) {
            continue;
        }
        match entry(w) {
            Ok(e) => out.valid.push(e),
            Err(e) => out.rejected.push(e),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_entry(json: &str) -> wire::Entry {
        serde_json::from_str(json).expect("wire types must accept any object")
    }

    /// Page that both describes and deletes an entry deletes it.
    ///
    /// Exact `entry_deserialise` fuzz input: id 41 unread AND later `removed`
    /// in same page. Property: nothing both usable and deleted.
    #[test]
    fn an_id_that_is_also_removed_never_arrives_as_valid() {
        let page: wire::EntriesResponse = serde_json::from_str(
            r#"{"total": 2, "entries": [
                 {"id": 41, "feed_id": 7, "status": "unread", "title": "t",
                  "url": "https://blog.example/post"},
                 {"id": 42, "feed_id": 7, "status": "removed", "title": "t"},
                 {"id": 41, "feed_id": 7, "status": "removed", "title": "t"}
               ]}"#,
        )
        .expect("the wire types accept any object");

        let out = entries(page.entries);
        assert!(
            out.removed.contains(&EntryId(41)) && out.removed.contains(&EntryId(42)),
            "both removals must be reported"
        );
        assert!(
            !out.valid.iter().any(|e| e.id == EntryId(41)),
            "an entry the same page deletes must not also arrive as usable"
        );
    }

    /// Order irrelevant: removal wins first or last.
    #[test]
    fn a_removal_wins_from_either_end_of_the_page() {
        let parse = |json: &str| -> wire::EntriesResponse {
            serde_json::from_str(json).expect("the wire types accept any object")
        };
        let removal_last = parse(
            r#"{"entries": [
                 {"id": 9, "feed_id": 1, "status": "unread", "title": "t"},
                 {"id": 9, "feed_id": 1, "status": "removed", "title": "t"}]}"#,
        );
        let removal_first = parse(
            r#"{"entries": [
                 {"id": 9, "feed_id": 1, "status": "removed", "title": "t"},
                 {"id": 9, "feed_id": 1, "status": "unread", "title": "t"}]}"#,
        );
        for page in [removal_last, removal_first] {
            let out = entries(page.entries);
            assert_eq!(out.removed, vec![EntryId(9)]);
            assert!(out.valid.is_empty(), "the removal wins from either end");
        }
    }

    #[test]
    fn a_normal_entry_converts() {
        let e = entry(wire_entry(
            r#"{"id":7,"feed_id":3,"status":"unread","title":"T","url":"https://x.example/a",
                "published_at":"2026-01-02T03:04:05Z","starred":true,"reading_time":4}"#,
        ))
        .unwrap();
        assert_eq!(e.id, EntryId(7));
        assert_eq!(e.status, EntryStatus::Unread);
        assert!(e.starred);
        assert_eq!(
            e.url.map(|u| u.as_str().to_owned()),
            Some("https://x.example/a".to_owned())
        );
        // Covers `to_utc`: returning `None` would wipe every entry date (list
        // sort order).
        assert_eq!(
            e.published_at,
            Some("2026-01-02T03:04:05Z".parse::<DateTime<Utc>>().unwrap())
        );
        assert_eq!(e.reading_time, 4);
        assert_eq!(e.title, "T");
    }

    #[test]
    fn a_non_utc_offset_is_converted_not_dropped() {
        // Wire keeps server offset; domain is UTC. Ignoring offset puts non-UTC
        // server's entries hours out of place.
        let e = entry(wire_entry(
            r#"{"id":8,"feed_id":3,"status":"unread","title":"T",
                "published_at":"2026-01-02T05:04:05+02:00"}"#,
        ))
        .unwrap();
        assert_eq!(
            e.published_at,
            Some("2026-01-02T03:04:05Z".parse::<DateTime<Utc>>().unwrap()),
            "05:04:05+02:00 is 03:04:05Z"
        );
    }

    #[test]
    fn a_bad_url_does_not_lose_the_article() {
        let e = entry(wire_entry(
            r#"{"id":7,"status":"read","title":"T","url":"javascript:alert(1)","content":"body"}"#,
        ))
        .unwrap();
        assert!(e.url.is_none(), "a dangerous link must not survive");
        assert_eq!(e.content, "body", "but the article text must");
    }

    #[test]
    fn an_unrecognised_status_rejects_only_that_entry() {
        let err = entry(wire_entry(r#"{"id":9,"status":"quantum"}"#)).unwrap_err();
        assert!(err.is_item_local(), "one bad entry must not stall the sync");
    }

    #[test]
    fn removed_entries_are_rejected_rather_than_mirrored() {
        let err = entry(wire_entry(r#"{"id":9,"status":"removed"}"#)).unwrap_err();
        assert!(err.is_item_local());
    }

    #[test]
    fn a_missing_status_is_rejected_not_defaulted() {
        // `wire::Entry` defaults status to "", must not become "unread": would
        // resurrect read entries.
        let err = entry(wire_entry(r#"{"id":9}"#)).unwrap_err();
        assert!(err.is_item_local());
    }

    #[test]
    fn a_page_survives_a_poisoned_row() {
        let page = vec![
            wire_entry(r#"{"id":1,"status":"read"}"#),
            wire_entry(r#"{"id":2,"status":"nonsense"}"#),
            wire_entry(r#"{"id":3,"status":"unread"}"#),
        ];
        let page = entries(page);
        assert_eq!(page.valid.len(), 2, "good rows must still be applied");
        assert_eq!(page.rejected.len(), 1);
        assert!(page.rejected.iter().all(Error::is_item_local));
    }

    #[test]
    fn absurd_numbers_are_clamped_not_rejected() {
        let e = entry(wire_entry(
            r#"{"id":1,"status":"read","reading_time":-9000}"#,
        ))
        .unwrap();
        assert_eq!(e.reading_time, 0);
    }

    #[test]
    fn a_feed_without_an_icon_has_no_icon_id() {
        let f: wire::Feed = serde_json::from_str(r#"{"id":1,"title":"F","icon":null}"#).unwrap();
        assert!(feed(f).unwrap().icon_id.is_none());
        // `icon_id` 0 = same statement.
        let f: wire::Feed =
            serde_json::from_str(r#"{"id":1,"icon":{"feed_id":1,"icon_id":0}}"#).unwrap();
        assert!(feed(f).unwrap().icon_id.is_none());
    }

    #[test]
    fn negative_ids_round_trip() {
        // §9.2: ids chosen by someone else, not assumed positive.
        let e = entry(wire_entry(r#"{"id":-3,"feed_id":-1,"status":"read"}"#)).unwrap();
        assert_eq!(e.id, EntryId(-3));
        assert_eq!(e.feed_id, FeedId(-1));
    }
}

#[cfg(test)]
mod removed_status_tests {
    use super::*;

    #[test]
    fn a_removed_entry_is_reported_as_a_deletion_not_a_reject() {
        // Pre-2.3 server: ONLY deletion signal. Retention job flips status to
        // 'removed', `changed_after` reports it. Must delete from mirror.
        let page = entries(vec![
            serde_json::from_str(r#"{"id":1,"status":"read"}"#).unwrap(),
            serde_json::from_str(r#"{"id":2,"status":"removed"}"#).unwrap(),
            serde_json::from_str(r#"{"id":3,"status":"unread"}"#).unwrap(),
        ]);
        assert_eq!(page.valid.len(), 2);
        assert_eq!(page.removed, vec![EntryId(2)]);
        assert!(
            page.rejected.is_empty(),
            "a removed entry is not a malformed one"
        );
    }
}
