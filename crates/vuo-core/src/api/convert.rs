//! Validating wire types into domain types, one item at a time.
//!
//! Everything here returns [`Error::Item`] on failure rather than a general
//! error, because §9.2 requires that a single malformed row be *rejected
//! without stalling the sync*: "reject the entry rather than the sync when one
//! item is malformed. One bad entry must not stall the outbox."
//!
//! The distinction that matters is between a field whose absence loses the
//! item and one whose absence loses only itself. A missing `status` means the
//! row cannot be represented, so the row goes. An unparseable `url` means the
//! article has no tappable link, which is a shame but not a reason to hide the
//! text the user wanted to read.

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

/// Parse a URL, yielding `None` rather than an error.
///
/// Applied to every foreign URL field. A `javascript:` or malformed value
/// silently becomes "no link", which is the only safe rendering and is far
/// better than dropping the whole entry.
fn lenient_url(raw: &str) -> Option<MediaUrl> {
    if raw.trim().is_empty() {
        return None;
    }
    MediaUrl::parse(raw)
}

pub fn entry(w: wire::Entry) -> Result<Entry> {
    let id = EntryId(w.id);

    // Status is the one field whose absence makes the row unrepresentable.
    let status = match w.status.as_str() {
        "unread" => EntryStatus::Unread,
        "read" => EntryStatus::Read,
        // Pre-2.3 servers emit this for entries the user deleted. It is not a
        // state Vuo mirrors: the row should not exist locally at all. Rejecting
        // it here means the sync skips it, which is exactly right.
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
        // A negative reading time is nonsense; clamp rather than reject.
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

pub fn feed(w: wire::Feed) -> Result<Feed> {
    Ok(Feed {
        id: FeedId(w.id),
        category_id: w.category.as_ref().map(|c| CategoryId(c.id)),
        title: w.title,
        site_url: lenient_url(&w.site_url),
        feed_url: lenient_url(&w.feed_url),
        // `icon` is present-and-null when the feed has no icon, and an
        // icon_id of 0 means the same thing.
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
    /// Ids the server reports as `removed`.
    ///
    /// A pre-2.3 server soft-deletes by flipping `status` to `removed` rather
    /// than deleting the row, so this is the *only* deletion signal such a
    /// server ever gives. Dropping these as merely "unusable" left the entry
    /// in the local mirror forever — visible in the UI, absent from the server
    /// — which is exactly what the schema comment about translating `removed`
    /// into a local DELETE promises does not happen.
    pub removed: Vec<EntryId>,
    /// Per-item errors, for logging or counting.
    pub rejected: Vec<Error>,
}

/// Convert a page of entries, separating the usable, the deleted and the bad.
#[must_use]
pub fn entries(page: Vec<wire::Entry>) -> Page {
    // Removals are collected FIRST, and then win.
    //
    // A page can carry the same id twice -- once with content and once as
    // `removed`. A single pass put such an id in BOTH `valid` and `removed`,
    // which leaves what happens to it decided by the order the caller applies
    // the two lists: apply removals first and the entry is deleted and then
    // re-inserted, so a deleted article silently comes back; apply them last
    // and it does not. Neither order is written down anywhere, which is the
    // real defect.
    //
    // "This entry should not exist" is the stronger claim of the two, and it
    // is the safe direction to resolve towards: the worst case is an entry
    // that the next sync brings back, rather than one the user deleted
    // reappearing. Found by `entry_deserialise` fuzzing, not by a device.
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

    /// §a page that both describes and deletes an entry deletes it.
    ///
    /// The exact input `entry_deserialise` fuzzing crashed on: id 41 appears
    /// as a normal unread entry AND, later in the same page, as `removed`.
    /// One pass put it in both lists, and the fuzz target's "nothing may
    /// appear as both usable and deleted" assertion is the property that
    /// matters -- what the mirror does with such a page was otherwise decided
    /// by the order the caller happened to apply them in.
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

    /// Order must not matter: the removal wins whether it comes first or last.
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
        // The fixture supplies a timestamp and nothing used to look at it, so
        // `to_utc` had no behavioural coverage at all: returning `None` from
        // it wiped every entry date -- the sort order of the whole list -- with
        // the crate still green.
        assert_eq!(
            e.published_at,
            Some(
                "2026-01-02T03:04:05Z"
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap()
            )
        );
        assert_eq!(e.reading_time, 4);
        assert_eq!(e.title, "T");
    }

    #[test]
    fn a_non_utc_offset_is_converted_not_dropped() {
        // The wire type keeps the server's offset; the domain type is UTC. A
        // conversion that ignored the offset would put every entry from a
        // non-UTC server hours out of place in the list.
        let e = entry(wire_entry(
            r#"{"id":8,"feed_id":3,"status":"unread","title":"T",
                "published_at":"2026-01-02T05:04:05+02:00"}"#,
        ))
        .unwrap();
        assert_eq!(
            e.published_at,
            Some(
                "2026-01-02T03:04:05Z"
                    .parse::<chrono::DateTime<chrono::Utc>>()
                    .unwrap()
            ),
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
        // wire::Entry defaults status to "", which must not silently become
        // "unread" -- that would resurrect entries the user has read.
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
        // icon_id 0 is the same statement said differently.
        let f: wire::Feed =
            serde_json::from_str(r#"{"id":1,"icon":{"feed_id":1,"icon_id":0}}"#).unwrap();
        assert!(feed(f).unwrap().icon_id.is_none());
    }

    #[test]
    fn negative_ids_round_trip() {
        // §9.2: ids are chosen by someone else and are not assumed positive.
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
        // On a pre-2.3 server this is the ONLY deletion signal there is:
        // the retention job flips status to 'removed' rather than deleting the
        // row, and `changed_after` then reports it. Treating it as merely
        // unusable left the entry in the mirror forever.
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
