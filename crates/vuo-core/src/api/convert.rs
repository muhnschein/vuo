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
    })
}

pub fn category(w: wire::Category) -> Result<Category> {
    Ok(Category {
        id: CategoryId(w.id),
        title: w.title,
        hide_globally: w.hide_globally,
    })
}

/// Convert a page of entries, separating the usable from the rejected.
///
/// Returns the entries that validated plus the per-item errors, so a caller can
/// log or count the rejects without either ignoring them or aborting on them.
#[must_use]
pub fn entries(page: Vec<wire::Entry>) -> (Vec<Entry>, Vec<Error>) {
    let mut ok = Vec::with_capacity(page.len());
    let mut rejected = Vec::new();
    for w in page {
        match entry(w) {
            Ok(e) => ok.push(e),
            Err(e) => rejected.push(e),
        }
    }
    (ok, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_entry(json: &str) -> wire::Entry {
        serde_json::from_str(json).expect("wire types must accept any object")
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
        let (ok, rejected) = entries(page);
        assert_eq!(ok.len(), 2, "good rows must still be applied");
        assert_eq!(rejected.len(), 1);
        assert!(rejected.iter().all(Error::is_item_local));
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
