//! Strict domain types.
//!
//! §9.2: *deserialise into permissive types and validate into strict domain
//! types at the boundary.* [`crate::api::wire`] is the permissive half; this is
//! the strict half. Once a value has one of these types, the rest of the crate
//! can rely on its invariants without re-checking.
//!
//! # On server-assigned identifiers
//!
//! §9.2 also says not to trust server-assigned identifiers to be well-behaved:
//! *not necessarily positive, not necessarily monotonic, not necessarily
//! stable across a restore.* So [`EntryId`] is a plain `i64` wrapper with no
//! positivity constraint, and nothing in Vuo treats a larger id as meaning
//! "newer".
//!
//! The sync cursor does paginate on `after_entry_id`, which is worth being
//! precise about: that relies only on ids being **totally ordered and stable
//! within a single pass**, which is a property of the server's `ORDER BY id`
//! and its primary key. It does *not* assume ids are positive, dense, or
//! correlated with time. A restore that renumbers everything changes which
//! rows a pass sees but cannot make the pagination skip a row within a pass.

use chrono::{DateTime, Utc};

use crate::content::MediaUrl;

macro_rules! id_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        pub struct $name(pub i64);

        impl $name {
            #[must_use]
            pub fn get(self) -> i64 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(EntryId, "A server-assigned entry identifier. Not assumed positive or monotonic.");
id_type!(FeedId, "A server-assigned feed identifier.");
id_type!(CategoryId, "A server-assigned category identifier.");
id_type!(IconId, "A server-assigned icon identifier.");

/// The read state of an entry.
///
/// Deliberately only two variants. Pre-2.3 servers can still emit `"removed"`,
/// which is not a state Vuo mirrors -- a removed entry should not be in the
/// local database at all. The converter maps it to a per-item rejection so the
/// sync drops the row and continues rather than storing a third state that the
/// UI would then have to render.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    Unread,
    Read,
}

impl EntryStatus {
    /// The exact string the API expects. `PUT /v1/entries` rejects anything else.
    #[must_use]
    pub fn as_api_str(self) -> &'static str {
        match self {
            EntryStatus::Unread => "unread",
            EntryStatus::Read => "read",
        }
    }

    #[must_use]
    pub fn is_read(self) -> bool {
        matches!(self, EntryStatus::Read)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub feed_id: FeedId,
    pub status: EntryStatus,
    pub starred: bool,
    /// Foreign text. Renders as `Text.PlainText` only (§9.3).
    pub title: String,
    /// `None` when the server sent nothing parseable or a non-http(s) scheme.
    /// A bad link is not a reason to discard the article body.
    pub url: Option<MediaUrl>,
    pub comments_url: Option<MediaUrl>,
    /// Foreign text. Plain text only.
    pub author: String,
    /// Raw article HTML as delivered. Transformed to render blocks lazily, at
    /// display time, rather than at sync time: the transform's output is
    /// larger than its input and depends on settings (media policy) that the
    /// user can change without re-syncing.
    pub content: String,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: Option<DateTime<Utc>>,
    /// The server's `changed_at`. Bumped on insert and on every status or
    /// starred mutation -- including Vuo's own replayed writes, which is why
    /// the pull has to tolerate seeing its own echo.
    pub changed_at: Option<DateTime<Utc>>,
    pub reading_time: i32,
    pub tags: Vec<String>,
    pub enclosures: Vec<Enclosure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enclosure {
    pub id: i64,
    pub entry_id: EntryId,
    pub url: Option<MediaUrl>,
    /// Claimed MIME type. Foreign, advisory, and not to be trusted for
    /// dispatch decisions on its own.
    pub mime_type: String,
    pub size: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub id: FeedId,
    pub category_id: Option<CategoryId>,
    /// Foreign text (a feed *name* is chosen by the feed operator). Plain text.
    pub title: String,
    pub site_url: Option<MediaUrl>,
    pub feed_url: Option<MediaUrl>,
    pub icon_id: Option<IconId>,
    pub checked_at: Option<DateTime<Utc>>,
    /// Non-empty when the server last failed to refresh this feed. Foreign
    /// text, plain text only.
    pub parsing_error_message: String,
    pub parsing_error_count: i32,
    pub disabled: bool,
    pub hide_globally: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: CategoryId,
    /// Foreign text. Plain text only.
    pub title: String,
    pub hide_globally: bool,
}

/// A decoded, content-validated feed icon.
///
/// Constructed only by [`crate::api::icon::decode_icon`], which sniffs the
/// magic bytes rather than believing `mime_type` (§9.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub id: IconId,
    /// The format as determined from the bytes, not as claimed.
    pub format: ImageFormat,
    pub bytes: Vec<u8>,
    /// Pixel dimensions where they could be read cheaply from the header.
    pub dimensions: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Ico,
    Svg,
}

impl ImageFormat {
    /// The MIME type Vuo will report for this format, derived from the bytes.
    #[must_use]
    pub fn mime_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Ico => "image/x-icon",
            ImageFormat::Svg => "image/svg+xml",
        }
    }
}

/// The Miniflux server version, parsed enough to gate request construction.
///
/// Several request-building rules differ across the version window Vuo has to
/// tolerate, and getting them wrong is not cosmetic: sending `limit=0` to a
/// 2.2.x server requests the entire corpus in one response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerVersion {
    pub raw: String,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ServerVersion {
    /// Parse a version string like `2.3.2`, tolerating suffixes and a `v`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim().trim_start_matches('v');
        let core = trimmed
            .split(|c: char| c == '-' || c == '+' || c == ' ')
            .next()
            .unwrap_or(trimmed);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
        Some(ServerVersion { raw: raw.trim().to_owned(), major, minor, patch })
    }

    fn at_least(&self, major: u32, minor: u32, patch: u32) -> bool {
        (self.major, self.minor, self.patch) >= (major, minor, patch)
    }

    /// `GET /v1/entries/ids` exists from 2.3.2.
    ///
    /// Used for cheap deletion reconciliation; older servers need a full
    /// re-pull instead.
    #[must_use]
    pub fn has_entry_ids_endpoint(&self) -> bool {
        self.at_least(2, 3, 2)
    }

    /// 2.3.x enforces `limit <= 1000` with a hard 400.
    #[must_use]
    pub fn enforces_entry_limit_cap(&self) -> bool {
        self.at_least(2, 3, 0)
    }

    /// The largest `limit` this server will accept.
    #[must_use]
    pub fn max_entry_limit(&self) -> u32 {
        if self.enforces_entry_limit_cap() {
            1000
        } else {
            // Older servers accept anything, but Vuo still refuses to ask for
            // an unbounded page: this is a phone.
            1000
        }
    }
}

/// An unbuilt "development" version string reports as this.
impl Default for ServerVersion {
    fn default() -> Self {
        // Assume the oldest behaviour Vuo supports when the server will not
        // say. Assuming the newest would mean using endpoints that 404.
        ServerVersion { raw: "unknown".to_owned(), major: 2, minor: 0, patch: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_maps_to_the_exact_api_strings() {
        assert_eq!(EntryStatus::Read.as_api_str(), "read");
        assert_eq!(EntryStatus::Unread.as_api_str(), "unread");
    }

    #[test]
    fn ids_may_be_zero_or_negative() {
        // §9.2: server-assigned ids are not assumed well-behaved. The type
        // must not silently exclude values the server can legitimately send.
        assert_eq!(EntryId(0).get(), 0);
        assert_eq!(EntryId(-5).get(), -5);
        assert!(EntryId(-5) < EntryId(0), "ordering must still be total");
    }

    #[test]
    fn version_parsing_tolerates_real_world_strings() {
        let v = ServerVersion::parse("2.3.2").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (2, 3, 2));
        assert!(ServerVersion::parse("v2.2.0").is_some());
        assert_eq!(ServerVersion::parse("2.3").map(|v| v.patch), Some(0));
        assert_eq!(ServerVersion::parse("2.3.2-dev").map(|v| v.patch), Some(2));
        assert!(ServerVersion::parse("").is_none());
        assert!(ServerVersion::parse("not-a-version").is_none());
    }

    #[test]
    fn feature_gates_follow_the_version() {
        let new = ServerVersion::parse("2.3.2").unwrap();
        let older = ServerVersion::parse("2.3.1").unwrap();
        let old = ServerVersion::parse("2.2.7").unwrap();

        assert!(new.has_entry_ids_endpoint());
        assert!(!older.has_entry_ids_endpoint(), "/v1/entries/ids lands in 2.3.2");
        assert!(!old.has_entry_ids_endpoint());

        assert!(new.enforces_entry_limit_cap());
        assert!(!old.enforces_entry_limit_cap());
    }

    #[test]
    fn unknown_version_assumes_the_oldest_supported_behaviour() {
        // Guessing high would mean calling endpoints that 404.
        let v = ServerVersion::default();
        assert!(!v.has_entry_ids_endpoint());
    }

    #[test]
    fn limit_is_never_unbounded_even_on_permissive_servers() {
        let old = ServerVersion::parse("2.2.0").unwrap();
        assert_eq!(old.max_entry_limit(), 1000, "a phone must not request the whole corpus");
    }
}
