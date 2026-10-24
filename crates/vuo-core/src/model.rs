//! Strict domain types.
//!
//! §9.2: *deserialise into permissive types and validate into strict domain
//! types at the boundary.* [`crate::api::wire`] = permissive half; this =
//! strict. Holding one of these types = invariants hold, no re-check.
//!
//! # Server-assigned identifiers
//!
//! §9.2: ids *not necessarily positive, not necessarily monotonic, not
//! necessarily stable across a restore.* So [`EntryId`] = plain `i64` wrapper,
//! no positivity constraint; larger id never means "newer".
//!
//! Sync cursor paginates on `after_entry_id`: relies only on ids **totally
//! ordered and stable within one pass** (server's `ORDER BY id`, primary key).
//! Not positive, dense, or time-correlated. Restore renumbering everything
//! changes which rows pass sees; can't skip row within pass.

use chrono::{DateTime, Utc};

use crate::content::MediaUrl;

macro_rules! id_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        pub struct $name(pub i64);

        impl $name {
            /// The raw identifier as the server sent it.
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

id_type!(
    EntryId,
    "A server-assigned entry identifier. Not assumed positive or monotonic."
);
id_type!(FeedId, "A server-assigned feed identifier.");
id_type!(CategoryId, "A server-assigned category identifier.");
id_type!(IconId, "A server-assigned icon identifier.");

/// Entry read state.
///
/// Two variants only. Pre-2.3 servers can emit `"removed"`: not mirrored
/// (removed entry doesn't belong locally). Converter maps it to per-item
/// rejection: sync drops row, continues; UI never renders third state.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    /// Not yet read.
    Unread,
    /// Read.
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

    /// `true` for [`EntryStatus::Read`].
    #[must_use]
    pub fn is_read(self) -> bool {
        matches!(self, EntryStatus::Read)
    }
}

/// An article as mirrored locally, validated from its [`crate::api::wire`] form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's server-assigned identifier.
    pub id: EntryId,
    /// The feed the entry belongs to.
    pub feed_id: FeedId,
    /// Whether the user has read it.
    pub status: EntryStatus,
    /// Whether the user has starred it.
    pub starred: bool,
    /// Foreign text. Renders as `Text.PlainText` only (§9.3).
    pub title: String,
    /// `None` when server sent nothing parseable or non-http(s) scheme. Bad
    /// link no reason to discard body.
    pub url: Option<MediaUrl>,
    /// The discussion page, if the feed links one. `None` on the same terms as `url`.
    pub comments_url: Option<MediaUrl>,
    /// Foreign text. Plain text only.
    pub author: String,
    /// Raw article HTML as delivered. Transformed to render blocks lazily at
    /// display, not sync: output larger than input, depends on settings (media
    /// policy) user changes without re-sync.
    pub content: String,
    /// When the feed says the entry was published.
    pub published_at: Option<DateTime<Utc>>,
    /// When the server first stored the entry.
    pub created_at: Option<DateTime<Utc>>,
    /// Server's `changed_at`. Bumped on insert and every status/starred
    /// mutation, incl. Vuo's own replayed writes: pull tolerates own echo.
    pub changed_at: Option<DateTime<Utc>>,
    /// The server's reading-time estimate in minutes. Clamped to be non-negative.
    pub reading_time: i32,
    /// Foreign text. The tags the feed attached to the entry.
    pub tags: Vec<String>,
    /// Attached media files, such as podcast episodes.
    pub enclosures: Vec<Enclosure>,
}

/// A media file attached to an entry, such as a podcast episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enclosure {
    /// The server-assigned enclosure identifier.
    pub id: i64,
    /// The entry it is attached to.
    pub entry_id: EntryId,
    /// `None` when the server sent nothing parseable or a non-http(s) scheme.
    pub url: Option<MediaUrl>,
    /// Claimed MIME type. Foreign, advisory; not trusted alone for dispatch.
    pub mime_type: String,
    /// Claimed size in bytes. Foreign and advisory; clamped to be non-negative.
    pub size: i64,
}

/// A subscribed feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    /// The feed's server-assigned identifier.
    pub id: FeedId,
    /// The category it is filed under, if the server sent one.
    pub category_id: Option<CategoryId>,
    /// Foreign text (a feed *name* is chosen by the feed operator). Plain text.
    pub title: String,
    /// The website the feed belongs to.
    pub site_url: Option<MediaUrl>,
    /// The address the server fetches the feed from.
    pub feed_url: Option<MediaUrl>,
    /// `None` when the feed has no icon (the server sends a null icon or an id of 0).
    pub icon_id: Option<IconId>,
    /// When the server last fetched the feed.
    pub checked_at: Option<DateTime<Utc>>,
    /// Non-empty when the server last failed to refresh this feed. Foreign
    /// text, plain text only.
    pub parsing_error_message: String,
    /// How many refreshes in a row have failed. Clamped to be non-negative.
    pub parsing_error_count: i32,
    /// The user has stopped the server refreshing this feed.
    pub disabled: bool,
    /// The user has hidden this feed's entries from the global unread and all-entries lists.
    pub hide_globally: bool,
    /// The server scrapes each entry's own page instead of trusting the feed's
    /// summary ("Fetch original content" in the web UI).
    pub crawler: bool,
}

/// A user-defined group of feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    /// The category's server-assigned identifier.
    pub id: CategoryId,
    /// Foreign text. Plain text only.
    pub title: String,
    /// The user has hidden its feeds' entries from the global unread and all-entries lists.
    pub hide_globally: bool,
}

/// Decoded, content-validated feed icon.
///
/// Only built by [`crate::api::icon::decode_icon`]: sniffs magic bytes, doesn't
/// believe `mime_type` (§9.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// The icon's server-assigned identifier.
    pub id: IconId,
    /// The format as determined from the bytes, not as claimed.
    pub format: ImageFormat,
    /// The decoded image bytes.
    pub bytes: Vec<u8>,
    /// Pixel dimensions where they could be read cheaply from the header.
    pub dimensions: Option<(u32, u32)>,
}

/// An image format recognised from an icon's magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// GIF.
    Gif,
    /// WebP.
    WebP,
    /// Windows bitmap.
    Bmp,
    /// Windows icon (`.ico`).
    Ico,
    /// SVG. Recognised only to refuse: [`crate::api::icon::decode_icon`] never
    /// produces [`Icon`] in this format.
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

/// Miniflux server version, parsed enough to gate request construction.
///
/// Request rules differ across tolerated window; not cosmetic: `limit=0` to
/// 2.2.x requests entire corpus in one response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerVersion {
    /// The version string as the server reported it, trimmed.
    pub raw: String,
    /// Major version.
    pub major: u32,
    /// Minor version; `0` when missing or unparseable.
    pub minor: u32,
    /// Patch version; `0` when missing or unparseable.
    pub patch: u32,
}

impl ServerVersion {
    /// Parse a version string like `2.3.2`, tolerating suffixes and a `v`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim().trim_start_matches('v');
        let core = trimmed.split(['-', '+', ' ']).next().unwrap_or(trimmed);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
        Some(ServerVersion {
            raw: raw.trim().to_owned(),
            major,
            minor,
            patch,
        })
    }

    fn at_least(&self, major: u32, minor: u32, patch: u32) -> bool {
        (self.major, self.minor, self.patch) >= (major, minor, patch)
    }

    /// `GET /v1/entries/ids` exists from 2.3.2.
    ///
    /// Cheap deletion reconcile; older servers need full re-pull.
    #[must_use]
    pub fn has_entry_ids_endpoint(&self) -> bool {
        self.at_least(2, 3, 2)
    }

    /// 2.3.x enforces `limit <= 1000`, hard 400.
    ///
    /// Vuo caps every request at 1000 anyway: `EntriesQuery::with_limit`
    /// clamps, held by
    /// `api::client::tests::limit_can_never_be_zero_or_unbounded`. Gate only
    /// says whether SERVER rejects larger; `sync` uses it to decide paging.
    #[must_use]
    pub fn enforces_entry_limit_cap(&self) -> bool {
        self.at_least(2, 3, 0)
    }

    /// `PUT /v1/entries` takes absolute `starred` set from 2.3.2.
    ///
    /// Verified by live contract tests and handler source either side: 2.2.13,
    /// 2.3.1 reject starred-only body with 400 ("invalid entry status"); 2.3.2,
    /// 2.3.3 accept. Below floor absolute-set design can't be honoured: Vuo
    /// refuses entry-state writes ENTIRELY (status too, though accepted) and
    /// keeps every intent queued. Partial support would let star write's 400
    /// match `would_discard` and drop intent as malformed. See
    /// `api::client::update_entries`.
    #[must_use]
    pub fn supports_entry_state_writes(&self) -> bool {
        self.at_least(2, 3, 2)
    }
}

/// Version string with no leading number (e.g. unbuilt "development" build)
/// read as oldest supported version.
impl Default for ServerVersion {
    fn default() -> Self {
        // Server won't say → assume oldest supported behaviour. Newest would
        // call endpoints that 404.
        ServerVersion {
            raw: "unknown".to_owned(),
            major: 2,
            minor: 0,
            patch: 0,
        }
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
        // §9.2: server ids not assumed well-behaved. Type must not exclude
        // values server can legitimately send.
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
        assert!(
            !older.has_entry_ids_endpoint(),
            "/v1/entries/ids lands in 2.3.2"
        );
        assert!(!old.has_entry_ids_endpoint());

        assert!(new.enforces_entry_limit_cap());
        assert!(!old.enforces_entry_limit_cap());

        assert!(new.supports_entry_state_writes());
        assert!(
            !older.supports_entry_state_writes(),
            "absolute starred lands in 2.3.2, same boundary as the id listing"
        );
        assert!(!old.supports_entry_state_writes());
    }

    #[test]
    fn unknown_version_assumes_the_oldest_supported_behaviour() {
        // Assert DEFAULT, not one consequence. `ServerVersion` gates three
        // behaviours: has_entry_ids_endpoint() and
        // supports_entry_state_writes() at >= 2.3.2, enforces_entry_limit_cap()
        // at >= 2.3.0. Checking only first lets default in [2.3.0, 2.3.2) pass
        // while flipping others. Guessing high calls endpoints that 404.
        let v = ServerVersion::default();
        assert_eq!((v.major, v.minor, v.patch), (2, 0, 0));
        assert!(!v.has_entry_ids_endpoint());
        assert!(!v.enforces_entry_limit_cap());
        assert!(!v.supports_entry_state_writes());
    }
}
