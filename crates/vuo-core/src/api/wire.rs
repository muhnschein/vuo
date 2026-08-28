//! Permissive wire types: exactly what Miniflux puts on the socket.
//!
//! §9.2: *treat every JSON field as absent, wrong-typed, or absurd. Server
//! versions skew; fields get added and deprecated. Deserialise into permissive
//! types and validate into strict domain types at the boundary, rejecting the
//! entry rather than the sync when one item is malformed.*
//!
//! Nothing in this module validates anything. These structs exist to make
//! `serde` succeed on any plausible server response so that validation can
//! happen afterwards, per item, where a failure can be attributed and
//! isolated. The strict types live in [`crate::model`] and the translation in
//! [`crate::api::convert`].
//!
//! # Three rules, learned from the server's own history
//!
//! 1. **No `deny_unknown_fields`, anywhere.** `Feed` gained a dozen fields and
//!    `User` eight within the version window Vuo has to tolerate. A newer
//!    server will always send keys this client has never heard of, and that
//!    must be uninteresting rather than fatal.
//! 2. **`#[serde(default)]` on every container.** `Option<T>` alone does *not*
//!    make a field optional in serde -- a missing key is still an error unless
//!    a default exists. Container-level `default` fixes every field at once,
//!    including the ones added after this file was written.
//! 3. **Absence has three spellings** and Miniflux uses all of them: the key
//!    is omitted (`feed`, `category`), the key is present and `null`
//!    (`icon`, `enclosures`, `tags`), or the key is present and holds a zero
//!    value (`""`, `0`, `false`, and the Go zero time
//!    `"0001-01-01T00:00:00Z"`). Only the first two are serde's problem; the
//!    third is handled by [`de_opt_time`].

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer, Serialize};

/// Go's zero `time.Time`, `0001-01-01T00:00:00Z`, as a Unix timestamp.
///
/// Miniflux serialises unset timestamps as this rather than omitting them or
/// sending null. Treating it as a real date is not a cosmetic bug: sorting a
/// feed list by a literal year-1 timestamp silently corrupts the ordering.
const GO_ZERO_TIME_SECS: i64 = -62_135_596_800;

#[must_use]
pub fn is_zero_time(t: &DateTime<FixedOffset>) -> bool {
    t.timestamp() == GO_ZERO_TIME_SECS
}

/// Deserialise a timestamp that may be absent, `null`, or the zero sentinel.
///
/// All three collapse to `None`, so callers never have to know which spelling
/// of "unset" a given server version chose.
pub fn de_opt_time<'de, D>(d: D) -> Result<Option<DateTime<FixedOffset>>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<DateTime<FixedOffset>>::deserialize(d)?;
    Ok(opt.filter(|t| !is_zero_time(t)))
}

/// Deserialise a collection that may arrive as `null`.
///
/// Collapses `null` to the empty collection so no call site has to branch.
pub fn de_null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// `{ "total": N, "entries": [...] }`
///
/// `total` is `count(*) OVER()` -- the full match count ignoring limit and
/// offset. It is *not* a safe loop bound during keyset pagination: with
/// `after_entry_id` set it counts only the rows still ahead of the cursor and
/// therefore shrinks on every page.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EntriesResponse {
    pub total: i64,
    #[serde(deserialize_with = "de_null_default")]
    pub entries: Vec<Entry>,
}

/// `{ "total": N, "entry_ids": [...] }` from `GET /v1/entries/ids`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EntryIdsResponse {
    pub total: i64,
    #[serde(deserialize_with = "de_null_default")]
    pub entry_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: i64,
    pub user_id: i64,
    pub feed_id: i64,
    /// Deliberately a `String`, not an enum.
    ///
    /// A server older than 2.3 can still send the retired `"removed"` status,
    /// and a newer one could add a value this client predates. Decoding into
    /// an enum would turn that into a deserialisation failure for the whole
    /// page rather than one unusable row.
    pub status: String,
    pub hash: String,
    pub title: String,
    pub url: String,
    pub comments_url: String,
    /// Added in 2.3.x; simply absent on older servers.
    pub language: String,
    #[serde(deserialize_with = "de_opt_time")]
    pub published_at: Option<DateTime<FixedOffset>>,
    #[serde(deserialize_with = "de_opt_time")]
    pub created_at: Option<DateTime<FixedOffset>>,
    #[serde(deserialize_with = "de_opt_time")]
    pub changed_at: Option<DateTime<FixedOffset>>,
    pub content: String,
    pub author: String,
    pub share_code: String,
    pub starred: bool,
    pub reading_time: i32,
    #[serde(deserialize_with = "de_null_default")]
    pub enclosures: Vec<Enclosure>,
    /// Omitted entirely when nil, and only partially populated when present.
    pub feed: Option<Feed>,
    #[serde(deserialize_with = "de_null_default")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Feed {
    pub id: i64,
    pub user_id: i64,
    pub feed_url: String,
    pub site_url: String,
    pub title: String,
    pub description: String,
    pub language: String,
    #[serde(deserialize_with = "de_opt_time")]
    pub checked_at: Option<DateTime<FixedOffset>>,
    #[serde(deserialize_with = "de_opt_time")]
    pub next_check_at: Option<DateTime<FixedOffset>>,
    pub etag_header: String,
    pub last_modified_header: String,
    pub parsing_error_message: String,
    pub parsing_error_count: i32,
    pub scraper_rules: String,
    pub rewrite_rules: String,
    pub crawler: bool,
    pub blocklist_rules: String,
    pub keeplist_rules: String,
    pub urlrewrite_rules: String,
    pub user_agent: String,
    pub cookie: String,
    pub username: String,
    pub password: String,
    pub disabled: bool,
    pub no_media_player: bool,
    pub ignore_http_cache: bool,
    pub allow_self_signed_certificates: bool,
    pub fetch_via_proxy: bool,
    pub hide_globally: bool,
    pub category: Option<Category>,
    /// Present and `null` when the feed has no icon.
    pub icon: Option<FeedIconRef>,
}

/// The `icon` member of a feed: a reference, not the bytes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FeedIconRef {
    pub feed_id: i64,
    pub icon_id: i64,
    pub external_icon_id: String,
}

/// The body of `GET /v1/feeds/{id}/icon` and `GET /v1/icons/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Icon {
    pub id: i64,
    pub mime_type: String,
    /// Base64, **prefixed with the mime type**: `"image/png;base64,iVBOR..."`.
    /// Not a `data:` URI -- the `data:` scheme prefix is absent.
    pub data: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Category {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub hide_globally: bool,
    /// Only present on `GET /v1/categories?counts=true`.
    pub feed_count: Option<i64>,
    pub total_unread: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Enclosure {
    pub id: i64,
    pub user_id: i64,
    pub entry_id: i64,
    pub url: String,
    pub mime_type: String,
    pub size: i64,
    pub media_progression: i64,
}

/// `GET /v1/feeds/counters`.
///
/// Feeds with a zero count are omitted from the maps rather than sent as
/// explicit zeros, so a missing key means zero.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FeedCounters {
    #[serde(deserialize_with = "de_null_default")]
    pub reads: std::collections::HashMap<String, i64>,
    #[serde(deserialize_with = "de_null_default")]
    pub unreads: std::collections::HashMap<String, i64>,
}

/// `GET /v1/version`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VersionResponse {
    pub version: String,
    pub commit: String,
    pub build_date: String,
    pub go_version: String,
    pub compiler: String,
    pub arch: String,
    pub os: String,
}

/// `GET /v1/me`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub theme: String,
    pub language: String,
    pub timezone: String,
    pub entry_sorting_field: String,
    pub entry_sorting_direction: String,
}

/// `GET /v1/entries/{id}/fetch-content`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EntryContentResponse {
    pub content: String,
    pub reading_time: i32,
}

/// Every error body Miniflux produces: `{"error_message": "..."}`.
///
/// The message is foreign text. Render it as plain text, never as markup.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiError {
    pub error_message: String,
}

// ---------------------------------------------------------------- requests

/// Body of `PUT /v1/entries` -- the only idempotent entry-state write.
///
/// Both fields are absolute sets, not deltas, which is what makes outbox
/// replay safe. `status` and `starred` are independent; sending neither is a
/// 400, and so is an empty `entry_ids`.
///
/// The `/star` and `/bookmark` routes are *not* usable here: they map to a
/// handler whose SQL is `SET starred = NOT starred`, a true toggle that flips
/// state back when replayed.
#[derive(Debug, Clone, Serialize)]
pub struct EntriesUpdateRequest {
    pub entry_ids: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_survives_a_completely_empty_object() {
        // The point of container-level `default`: a server that omits
        // everything must still deserialise.
        let e: Entry = serde_json::from_str("{}").unwrap();
        assert_eq!(e.id, 0);
        assert!(e.enclosures.is_empty());
        assert!(e.feed.is_none());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // A newer server will always send keys this client predates.
        let json = r#"{"id":1,"title":"x","some_field_from_2027":{"nested":true},"tags":["a"]}"#;
        let e: Entry = serde_json::from_str(json).unwrap();
        assert_eq!(e.id, 1);
        assert_eq!(e.tags, vec!["a".to_owned()]);
    }

    #[test]
    fn null_collections_become_empty_not_errors() {
        let json = r#"{"id":1,"enclosures":null,"tags":null}"#;
        let e: Entry = serde_json::from_str(json).unwrap();
        assert!(e.enclosures.is_empty());
        assert!(e.tags.is_empty());
    }

    #[test]
    fn go_zero_time_becomes_none() {
        let json = r#"{"id":1,"published_at":"0001-01-01T00:00:00Z"}"#;
        let e: Entry = serde_json::from_str(json).unwrap();
        assert!(
            e.published_at.is_none(),
            "the year-1 sentinel must not sort as a real date"
        );
    }

    #[test]
    fn timestamps_keep_their_offset_and_fractional_seconds() {
        let json = r#"{"id":1,"published_at":"2026-03-04T05:06:07.123456+02:00"}"#;
        let e: Entry = serde_json::from_str(json).unwrap();
        let ts = e.published_at.unwrap();
        // 05:06:07+02:00 is 03:06:07Z.
        assert_eq!(ts.timestamp(), 1_772_593_567);
        assert_eq!(ts.offset().local_minus_utc(), 2 * 3600);
    }

    #[test]
    fn null_timestamps_become_none() {
        let json = r#"{"id":1,"checked_at":null}"#;
        let f: Feed = serde_json::from_str(json).unwrap();
        assert!(f.checked_at.is_none());
    }

    #[test]
    fn a_retired_status_value_does_not_fail_the_page() {
        // Pre-2.3 servers still emit "removed". An enum here would reject the
        // entire response rather than one row.
        let e: Entry = serde_json::from_str(r#"{"id":1,"status":"removed"}"#).unwrap();
        assert_eq!(e.status, "removed");
    }

    #[test]
    fn update_request_omits_absent_fields() {
        let req = EntriesUpdateRequest {
            entry_ids: vec![1, 2],
            status: Some("read"),
            starred: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"entry_ids":[1,2],"status":"read"}"#);

        let req = EntriesUpdateRequest {
            entry_ids: vec![3],
            status: None,
            starred: Some(false),
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"entry_ids":[3],"starred":false}"#
        );
    }

    #[test]
    fn counters_tolerate_nulls() {
        let c: FeedCounters = serde_json::from_str(r#"{"reads":null,"unreads":{"7":3}}"#).unwrap();
        assert!(c.reads.is_empty());
        assert_eq!(c.unreads.get("7"), Some(&3));
    }
}
