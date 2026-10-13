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

/// Whether `t` is Go's zero time, the sentinel Miniflux sends for an unset timestamp.
#[must_use]
pub fn is_zero_time(t: &DateTime<FixedOffset>) -> bool {
    t.timestamp() == GO_ZERO_TIME_SECS
}

/// Deserialise a timestamp that may be absent, `null`, or the zero sentinel.
///
/// All three collapse to `None`, so callers never have to know which spelling
/// of "unset" a given server version chose.
///
/// # Errors
///
/// Returns the deserialiser's error when the value is neither `null` nor an
/// RFC 3339 timestamp.
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
///
/// # Errors
///
/// Returns the deserialiser's error when a non-null value does not decode as `T`.
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
    /// Count of every entry matching the query; see the type docs for why it is no loop bound.
    pub total: i64,
    /// This page of entries.
    #[serde(deserialize_with = "de_null_default")]
    pub entries: Vec<Entry>,
}

/// `{ "total": N, "entry_ids": [...] }` from `GET /v1/entries/ids`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EntryIdsResponse {
    /// Count of every matching entry id.
    pub total: i64,
    /// The matching entry ids.
    #[serde(deserialize_with = "de_null_default")]
    pub entry_ids: Vec<i64>,
}

/// One entry (article), as `GET /v1/entries` and friends send it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// The server's id for the entry.
    pub id: i64,
    /// The user who owns the entry.
    pub user_id: i64,
    /// The feed the entry came from.
    pub feed_id: i64,
    /// Deliberately a `String`, not an enum.
    ///
    /// A server older than 2.3 can still send the retired `"removed"` status,
    /// and a newer one could add a value this client predates. Decoding into
    /// an enum would turn that into a deserialisation failure for the whole
    /// page rather than one unusable row.
    pub status: String,
    /// Miniflux's hash identifying the entry within its feed.
    pub hash: String,
    /// The article title. Foreign text.
    pub title: String,
    /// Link to the article on its own site. Foreign text, validated on conversion.
    pub url: String,
    /// Link to the article's discussion page, if the feed gives one. Foreign text.
    pub comments_url: String,
    /// Added in 2.3.x; simply absent on older servers.
    pub language: String,
    /// When the feed says the article was published.
    #[serde(deserialize_with = "de_opt_time")]
    pub published_at: Option<DateTime<FixedOffset>>,
    /// When the server first stored the entry.
    #[serde(deserialize_with = "de_opt_time")]
    pub created_at: Option<DateTime<FixedOffset>>,
    /// When the server last changed the entry.
    #[serde(deserialize_with = "de_opt_time")]
    pub changed_at: Option<DateTime<FixedOffset>>,
    /// The article body as HTML. Foreign text.
    pub content: String,
    /// The article's author. Foreign text.
    pub author: String,
    /// The public share code, empty when the entry has not been shared.
    pub share_code: String,
    /// Whether the user has starred (bookmarked) the entry.
    pub starred: bool,
    /// Estimated reading time in minutes.
    pub reading_time: i32,
    /// Attached media; `null` on the wire becomes empty.
    #[serde(deserialize_with = "de_null_default")]
    pub enclosures: Vec<Enclosure>,
    /// Omitted entirely when nil, and only partially populated when present.
    pub feed: Option<Feed>,
    /// Tags (categories) the feed attached to the entry. Foreign text; `null` becomes empty.
    #[serde(deserialize_with = "de_null_default")]
    pub tags: Vec<String>,
}

// Miniflux's own shape, field for field: the bools are the server's.
/// A subscribed feed, from `GET /v1/feeds` or embedded in an [`Entry`].
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Feed {
    /// The server's id for the feed.
    pub id: i64,
    /// The user who owns the subscription.
    pub user_id: i64,
    /// The URL the server fetches the feed from.
    pub feed_url: String,
    /// The website the feed belongs to. Foreign text.
    pub site_url: String,
    /// The feed title. Foreign text.
    pub title: String,
    /// The feed's description. Foreign text.
    pub description: String,
    /// The feed's declared language. Foreign text.
    pub language: String,
    /// When the server last fetched the feed.
    #[serde(deserialize_with = "de_opt_time")]
    pub checked_at: Option<DateTime<FixedOffset>>,
    /// When the server next plans to fetch the feed.
    #[serde(deserialize_with = "de_opt_time")]
    pub next_check_at: Option<DateTime<FixedOffset>>,
    /// The `ETag` from the feed's last response.
    pub etag_header: String,
    /// The `Last-Modified` from the feed's last response.
    pub last_modified_header: String,
    /// Why the last fetch failed, empty when it did not. Foreign text.
    pub parsing_error_message: String,
    /// How many fetches in a row have failed.
    pub parsing_error_count: i32,
    /// Scraper rules for fetching full article content.
    pub scraper_rules: String,
    /// Content rewrite rules.
    pub rewrite_rules: String,
    /// Whether the server fetches each article's full content.
    pub crawler: bool,
    /// Rules for entries the server should drop.
    pub blocklist_rules: String,
    /// Rules for entries the server should keep.
    pub keeplist_rules: String,
    /// Rules for rewriting entry URLs.
    pub urlrewrite_rules: String,
    /// A custom user agent for fetching the feed.
    pub user_agent: String,
    /// A custom cookie for fetching the feed.
    pub cookie: String,
    /// Username for a feed behind HTTP authentication.
    pub username: String,
    /// Password for a feed behind HTTP authentication.
    pub password: String,
    /// Whether fetching the feed is paused.
    pub disabled: bool,
    /// Whether the server hides its media player for this feed.
    pub no_media_player: bool,
    /// Whether the server ignores HTTP caching headers when fetching.
    pub ignore_http_cache: bool,
    /// Whether the server accepts a self-signed certificate from the feed's host.
    pub allow_self_signed_certificates: bool,
    /// Whether the server fetches the feed through its proxy.
    pub fetch_via_proxy: bool,
    /// Whether the feed's entries are hidden from the global unread list.
    pub hide_globally: bool,
    /// The category the feed is filed under; omitted when nil.
    pub category: Option<Category>,
    /// Present and `null` when the feed has no icon.
    pub icon: Option<FeedIconRef>,
}

/// The `icon` member of a feed: a reference, not the bytes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FeedIconRef {
    /// The feed the icon belongs to.
    pub feed_id: i64,
    /// The id to fetch the icon by; `0` means no icon.
    pub icon_id: i64,
    /// Miniflux's external icon id.
    pub external_icon_id: String,
}

/// The body of `GET /v1/feeds/{id}/icon` and `GET /v1/icons/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Icon {
    /// The server's id for the icon.
    pub id: i64,
    /// The mime type the server claims. Not trusted; see [`crate::api::icon`].
    pub mime_type: String,
    /// Base64, **prefixed with the mime type**: `"image/png;base64,iVBOR..."`.
    /// Not a `data:` URI -- the `data:` scheme prefix is absent.
    pub data: String,
}

/// A category, from `GET /v1/categories` or embedded in a [`Feed`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Category {
    /// The server's id for the category.
    pub id: i64,
    /// The user who owns the category.
    pub user_id: i64,
    /// The category title.
    pub title: String,
    /// Whether entries in this category are hidden from the global unread list.
    pub hide_globally: bool,
    /// Only present on `GET /v1/categories?counts=true`.
    pub feed_count: Option<i64>,
    /// Unread entries in the category; likewise only present with `counts=true`.
    pub total_unread: Option<i64>,
}

/// A media attachment on an entry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Enclosure {
    /// The server's id for the enclosure.
    pub id: i64,
    /// The user who owns the enclosure.
    pub user_id: i64,
    /// The entry the enclosure belongs to.
    pub entry_id: i64,
    /// Where the media lives. Foreign text, validated on conversion.
    pub url: String,
    /// The mime type the feed claims. Foreign text.
    pub mime_type: String,
    /// Size in bytes, as the feed claims it.
    pub size: i64,
    /// Playback position the user reached, in seconds.
    pub media_progression: i64,
}

/// `GET /v1/feeds/counters`.
///
/// Feeds with a zero count are omitted from the maps rather than sent as
/// explicit zeros, so a missing key means zero.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FeedCounters {
    /// Read entries per feed, keyed by feed id as a string.
    #[serde(deserialize_with = "de_null_default")]
    pub reads: std::collections::HashMap<String, i64>,
    /// Unread entries per feed, keyed by feed id as a string.
    #[serde(deserialize_with = "de_null_default")]
    pub unreads: std::collections::HashMap<String, i64>,
}

/// `GET /v1/version`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VersionResponse {
    /// The Miniflux version, for example `2.3.2`.
    pub version: String,
    /// The commit the server was built from.
    pub commit: String,
    /// When the server was built.
    pub build_date: String,
    /// The Go version the server was built with.
    pub go_version: String,
    /// The Go compiler used.
    pub compiler: String,
    /// The server's CPU architecture.
    pub arch: String,
    /// The server's operating system.
    pub os: String,
}

/// `GET /v1/me`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct User {
    /// The user's id.
    pub id: i64,
    /// The user's login name.
    pub username: String,
    /// Whether the user is an administrator.
    pub is_admin: bool,
    /// The user's web UI theme.
    pub theme: String,
    /// The user's interface language.
    pub language: String,
    /// The user's time zone name.
    pub timezone: String,
    /// The field the web UI sorts entries by.
    pub entry_sorting_field: String,
    /// The direction the web UI sorts entries in.
    pub entry_sorting_direction: String,
}

/// `GET /v1/entries/{id}/fetch-content`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EntryContentResponse {
    /// The entry's content as the server scraped it from the original page. Foreign text.
    pub content: String,
    /// Reading time in minutes for the scraped content.
    pub reading_time: i32,
}

/// Every error body Miniflux produces: `{"error_message": "..."}`.
///
/// The message is foreign text. Render it as plain text, never as markup.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ApiError {
    /// The server's description of what went wrong. Foreign text.
    pub error_message: String,
}

// ---------------------------------------------------------------- requests

/// Body of `PUT /v1/entries` -- the only idempotent entry-state write.
///
/// Both fields are absolute sets, not deltas, which is what makes outbox
/// replay safe. `status` and `starred` are independent; sending neither is a
/// 400 ("either the status or the starred field must be specified", verified
/// on 2.3.2+), and so is an empty `entry_ids`. `starred` itself only exists
/// from 2.3.2; on older servers the field is unknown and a starred-only body
/// 400s, which is why the client refuses writes below that floor rather than
/// sending this.
///
/// The `/star` and `/bookmark` routes are *not* usable here: they map to a
/// handler whose SQL is `SET starred = NOT starred`, a true toggle that flips
/// state back when replayed.
#[derive(Debug, Clone, Serialize)]
pub struct EntriesUpdateRequest {
    /// The entries to change; must not be empty.
    pub entry_ids: Vec<i64>,
    /// The status to set, `"read"` or `"unread"`; omitted when unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<&'static str>,
    /// The starred flag to set; omitted when unchanged.
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
        assert_eq!(e.enclosures.len(), 0);
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
        assert_eq!(e.enclosures.len(), 0);
        assert_eq!(e.tags, Vec::<String>::new());
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
        // `timestamp()` truncates, so the whole-seconds assertion above says
        // nothing about the fractional half of this test's name: zeroing the
        // nanoseconds left it, and the rest of the crate, green.
        assert_eq!(ts.timestamp_subsec_micros(), 123_456);
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
