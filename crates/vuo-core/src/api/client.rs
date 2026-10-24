//! Miniflux REST client.
//!
//! §4: Miniflux's own REST API with API key. Not Fever, not Google Reader:
//! those layers serve other clients, cost fidelity.
//!
//! # Request-building invariants
//!
//! Some query params are dangerous, so types enforce them, not call sites:
//!
//! - **`limit` never 0.** 2.3.x clamps 0 to 1000; 2.2.x reads it as *unlimited*
//!   (full corpus into phone memory). [`EntriesQuery`] clamps into `1..=1000`.
//! - **`order` is an enum.** Server whitelists it, 400 otherwise; only one
//!   value safe to paginate on (see [`EntryOrder`]).
//! - **`direction` lowercase.** `"ASC"` = 400.
//! - **Timestamps: epoch seconds, 0 = "unset"**, so unset cursor omits param;
//!   `0` would mean "changed after 1970", everything.
//! - **`entry_ids` never empty.** Empty list = hard 400.

use url::Url;

use crate::api::transport::{BoundedResponse, Transport};
use crate::api::wire;
use crate::error::{Error, Result};
use crate::model::{EntryId, EntryStatus, ServerVersion};
use crate::redact::SafeUrl;

/// Only sort orders Miniflux accepts; anything else = 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryOrder {
    /// **Only order safe for pagination.**
    ///
    /// Server's `ORDER BY` has one expression, no id tiebreaker: every other
    /// order *unstable*, equal rows reorder between queries. Mark-all-read
    /// gives thousands of rows same `changed_at`; paging skips and duplicates.
    /// `id` = primary key, total order.
    Id,
    /// By publication date (`published_at`). Unstable for pagination.
    PublishedAt,
    /// By the server's last-change time (`changed_at`). Unstable for pagination.
    ChangedAt,
    /// By when the server first stored the entry (`created_at`). Unstable for pagination.
    CreatedAt,
    /// By category title. Unstable for pagination.
    CategoryTitle,
    /// By category id. Unstable for pagination.
    CategoryId,
    /// By read state. Unstable for pagination.
    Status,
    /// By entry title. Unstable for pagination.
    Title,
    /// By author. Unstable for pagination.
    Author,
}

impl EntryOrder {
    fn as_str(self) -> &'static str {
        match self {
            EntryOrder::Id => "id",
            EntryOrder::PublishedAt => "published_at",
            EntryOrder::ChangedAt => "changed_at",
            EntryOrder::CreatedAt => "created_at",
            EntryOrder::CategoryTitle => "category_title",
            EntryOrder::CategoryId => "category_id",
            EntryOrder::Status => "status",
            EntryOrder::Title => "title",
            EntryOrder::Author => "author",
        }
    }
}

/// The direction of an entry-list sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

impl SortDirection {
    fn as_str(self) -> &'static str {
        // Lowercase only: "ASC" = 400.
        match self {
            SortDirection::Asc => "asc",
            SortDirection::Desc => "desc",
        }
    }
}

/// A validated query against the entry-list endpoints.
#[derive(Debug, Clone)]
pub struct EntriesQuery {
    limit: u32,
    order: EntryOrder,
    direction: SortDirection,
    after_entry_id: Option<EntryId>,
    changed_after: Option<i64>,
    starred: Option<bool>,
    feed_id: Option<i64>,
    category_id: Option<i64>,
}

impl Default for EntriesQuery {
    fn default() -> Self {
        EntriesQuery {
            limit: 100,
            order: EntryOrder::Id,
            direction: SortDirection::Asc,
            after_entry_id: None,
            changed_after: None,
            starred: None,
            feed_id: None,
            category_id: None,
        }
    }
}

impl EntriesQuery {
    /// Query shape sync engine uses: id keyset scan.
    #[must_use]
    pub fn keyset(limit: u32) -> Self {
        EntriesQuery {
            order: EntryOrder::Id,
            direction: SortDirection::Asc,
            ..Default::default()
        }
        .with_limit(limit)
    }

    /// Clamps into `1..=1000`. Zero can't be expressed.
    #[must_use]
    pub fn with_limit(mut self, limit: u32) -> Self {
        self.limit = limit.clamp(1, 1000);
        self
    }

    /// Only entries with id > `id`: keyset cursor. `None` omits param.
    #[must_use]
    pub fn after_entry_id(mut self, id: Option<EntryId>) -> Self {
        self.after_entry_id = id;
        self
    }

    /// Epoch seconds. `None` omits param; `Some(0)` would mean "since 1970",
    /// never intended.
    #[must_use]
    pub fn changed_after(mut self, epoch_secs: Option<i64>) -> Self {
        self.changed_after = epoch_secs.filter(|s| *s > 0);
        self
    }

    /// Only starred (`Some(true)`) or only unstarred (`Some(false)`) entries.
    /// `None` omits the filter.
    #[must_use]
    pub fn starred(mut self, starred: Option<bool>) -> Self {
        self.starred = starred;
        self
    }

    /// Adds a `feed_id` filter. `None` omits it.
    #[must_use]
    pub fn feed_id(mut self, id: Option<i64>) -> Self {
        self.feed_id = id;
        self
    }

    /// Adds a `category_id` filter. `None` omits it.
    #[must_use]
    pub fn category_id(mut self, id: Option<i64>) -> Self {
        self.category_id = id;
        self
    }

    /// The page size this query requests, always in `1..=1000`.
    #[must_use]
    pub fn limit(&self) -> u32 {
        self.limit
    }

    fn apply(&self, url: &mut Url) {
        let mut q = url.query_pairs_mut();
        q.append_pair("limit", &self.limit.to_string());
        q.append_pair("order", self.order.as_str());
        q.append_pair("direction", self.direction.as_str());
        if let Some(id) = self.after_entry_id {
            q.append_pair("after_entry_id", &id.get().to_string());
        }
        if let Some(secs) = self.changed_after {
            q.append_pair("changed_after", &secs.to_string());
        }
        if let Some(starred) = self.starred {
            // Exactly "true"/"false": /v1/entries/ids 400s otherwise.
            q.append_pair("starred", if starred { "true" } else { "false" });
        }
        if let Some(id) = self.feed_id {
            q.append_pair("feed_id", &id.to_string());
        }
        if let Some(id) = self.category_id {
            q.append_pair("category_id", &id.to_string());
        }
    }
}

/// What entry-state write sets. Both absolute values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryMutation {
    /// Set the read state.
    Status(EntryStatus),
    /// Set or clear the star.
    Starred(bool),
}

/// Client for one Miniflux instance's REST API.
///
/// Clones share cached server version.
#[derive(Debug, Clone)]
pub struct MinifluxClient {
    transport: Transport,
    /// Server version once known. Written by [`MinifluxClient::version`], read
    /// by entry-write floor (see `write_capable_version`).
    ///
    /// Lock, not `OnceLock`: `version` must REPLACE answer when instance
    /// upgrades past write floor while Vuo runs.
    server_version: std::sync::Arc<std::sync::Mutex<Option<ServerVersion>>>,
}

impl MinifluxClient {
    /// Client sending through `transport`. Server version starts unknown;
    /// [`Self::version`] or first entry-state write fills it.
    #[must_use]
    pub fn new(transport: Transport) -> Self {
        MinifluxClient {
            transport,
            server_version: std::sync::Arc::default(),
        }
    }

    /// The transport this client sends through.
    #[must_use]
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    fn url(&self, path: &str) -> Url {
        // `join` on base without trailing slash replaces last segment:
        // normalise base. Matters for instances under sub-path.
        let mut base = self.transport.origin().clone();
        let base_path = base.path().trim_end_matches('/').to_owned();
        base.set_path(&format!("{base_path}/{}", path.trim_start_matches('/')));
        base.set_query(None);
        base
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T> {
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::GET, url, None).await?;
        Self::decode(&response, &safe)
    }

    fn decode<T: serde::de::DeserializeOwned>(
        response: &BoundedResponse,
        safe: &SafeUrl,
    ) -> Result<T> {
        Self::check_status(response, safe)?;
        serde_json::from_slice(&response.body).map_err(|e| {
            // Parse error message can quote foreign response bytes; keep only
            // position.
            Error::Protocol(format!(
                "could not decode the response at line {} column {}",
                e.line(),
                e.column()
            ))
        })
    }

    fn check_status(response: &BoundedResponse, safe: &SafeUrl) -> Result<()> {
        if (200..300).contains(&response.status) {
            return Ok(());
        }
        // Miniflux errors are `{"error_message": "..."}`. Message is foreign:
        // plain text only.
        let message = serde_json::from_slice::<wire::ApiError>(&response.body)
            .ok()
            .map(|e| e.error_message)
            .filter(|m| !m.is_empty());
        Err(Error::Http {
            status: response.status,
            endpoint: safe.clone(),
            message,
        })
    }

    // ------------------------------------------------------------ discovery

    /// `GET /v1/version`. Request-building rules branch on answer;
    /// [`Self::update_entries`] checks it before writing.
    ///
    /// Each call refetches and refreshes cache: answer changes when owner
    /// upgrades Miniflux; `sync` calls this on version-check TTL to notice.
    ///
    /// Unparseable version string not an error: treated as oldest supported
    /// ([`ServerVersion::default`]).
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn version(&self) -> Result<ServerVersion> {
        let w: wire::VersionResponse = self.get_json(self.url("/v1/version")).await?;
        let version = ServerVersion::parse(&w.version).unwrap_or_default();
        *self.cached_version() = Some(version.clone());
        Ok(version)
    }

    /// Cache behind `version`. Poisoned lock still holds a whole version: read
    /// it.
    fn cached_version(&self) -> std::sync::MutexGuard<'_, Option<ServerVersion>> {
        self.server_version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Version entry-state writes are checked against.
    ///
    /// Known (sync pass asked)? Use it. Else ask now: floor is data-loss guard,
    /// never guesses past unknown server. Failed fetch propagates (transient):
    /// intent stays queued, next flush retries.
    async fn write_capable_version(&self) -> Result<ServerVersion> {
        let known = self.cached_version().clone();
        match known {
            Some(v) => Ok(v),
            None => self.version().await,
        }
    }

    /// Refuse entry-state write server can't apply as absolute set.
    ///
    /// `Error::Config` on purpose: not transient (retry replays refusal), not
    /// permanent payload rejection (payload fine, SERVER too old), so outbox
    /// keeps intent queued. See `ServerVersion::supports_entry_state_writes`.
    async fn refuse_entry_writes_before_floor(&self) -> Result<()> {
        let version = self.write_capable_version().await?;
        if version.supports_entry_state_writes() {
            return Ok(());
        }
        Err(Error::Config(format!(
            "Miniflux {} predates absolute entry-state writes (Vuo needs 2.3.2+); \
             actions stay queued until the server is upgraded",
            version.raw
        )))
    }

    /// `GET /v1/me`. Verifies API key during setup.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status, incl. 401/403 for rejected key.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn me(&self) -> Result<wire::User> {
        self.get_json(self.url("/v1/me")).await
    }

    // -------------------------------------------------------------- reading

    /// `GET /v1/categories`: every category user has.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn categories(&self) -> Result<Vec<wire::Category>> {
        self.get_json(self.url("/v1/categories")).await
    }

    /// `GET /v1/feeds`: every subscribed feed.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn feeds(&self) -> Result<Vec<wire::Feed>> {
        self.get_json(self.url("/v1/feeds")).await
    }

    /// `GET /v1/entries`: one page matching `query`.
    ///
    /// Also returns response `Date` header when it parsed: caller anchors sync
    /// cursor to server clock, not phone's.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn entries(
        &self,
        query: &EntriesQuery,
    ) -> Result<(wire::EntriesResponse, Option<chrono::DateTime<chrono::Utc>>)> {
        let mut url = self.url("/v1/entries");
        query.apply(&mut url);
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::GET, url, None).await?;
        let date = response.server_date;
        let body: wire::EntriesResponse = Self::decode(&response, &safe)?;
        Ok((body, date))
    }

    /// `GET /v1/entries/{id}`: one entry.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status, e.g. 404 for unknown id.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn entry(&self, id: EntryId) -> Result<wire::Entry> {
        self.get_json(self.url(&format!("/v1/entries/{}", id.get())))
            .await
    }

    /// `GET /v1/entries/ids`. Only from 2.3.2; check
    /// [`ServerVersion::has_entry_ids_endpoint`] first.
    ///
    /// `offset` skips that many ids.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status, incl. server too old for endpoint.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn entry_ids(
        &self,
        query: &EntriesQuery,
        offset: u32,
    ) -> Result<wire::EntryIdsResponse> {
        let mut url = self.url("/v1/entries/ids");
        query.apply(&mut url);
        url.query_pairs_mut()
            .append_pair("offset", &offset.to_string());
        self.get_json(url).await
    }

    /// `GET /v1/feeds/{id}/icon`: feed icon, still encoded, unchecked. Pass
    /// through [`crate::api::icon::decode_icon`] before use.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status, e.g. feed has no icon.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn feed_icon(&self, feed_id: i64) -> Result<wire::Icon> {
        self.get_json(self.url(&format!("/v1/feeds/{feed_id}/icon")))
            .await
    }

    /// `GET /v1/feeds/counters`: read and unread counts per feed.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn counters(&self) -> Result<wire::FeedCounters> {
        self.get_json(self.url("/v1/feeds/counters")).await
    }

    /// `GET /v1/entries/{id}/fetch-content`.
    ///
    /// §3 forbids client-side readability port; this is server-side equivalent.
    /// `update_content` never sent: it makes this GET persist scrape
    /// server-side, must not happen speculatively.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status, e.g. original page unfetchable.
    /// - [`Error::Protocol`] when body isn't expected JSON.
    pub async fn fetch_original_content(&self, id: EntryId) -> Result<wire::EntryContentResponse> {
        self.get_json(self.url(&format!("/v1/entries/{}/fetch-content", id.get())))
            .await
    }

    // -------------------------------------------------------------- writing

    /// `PUT /v1/entries`: only idempotent entry-state write.
    ///
    /// `status` and `starred` are absolute sets: outbox replay safe after
    /// ambiguous timeout. Never `/star` or `/bookmark`: shared handler does
    /// `SET starred = NOT starred`, replay flips back.
    ///
    /// Server ignores unknown ids (no rows-affected check): 204 ≠ every id
    /// existed.
    ///
    /// # 2.3.2 floor
    ///
    /// Before 2.3.2 endpoint knew only `status`; starred-only body = 400
    /// ("invalid entry status"), which outbox `would_discard` would treat as
    /// malformed and DROP the star. So older server: refuse before sending.
    /// Error neither transient nor permanent rejection: intents stay queued
    /// (visible as stuck) until upgrade. Status writes refused too:
    /// half-working writes lose stars.
    ///
    /// # Errors
    ///
    /// - [`Error::Config`] when `ids` empty or server older than 2.3.2. Nothing
    ///   sent.
    /// - Whatever [`Self::version`] returns when version unknown and fetch
    ///   fails.
    /// - [`Error::Protocol`] if request body can't be encoded.
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    pub async fn update_entries(&self, ids: &[EntryId], mutation: EntryMutation) -> Result<()> {
        if ids.is_empty() {
            // Hard 400 server-side; refuse locally so outbox retry classifier
            // doesn't call it server fault.
            return Err(Error::Config(
                "refusing to send an empty entry id list".to_owned(),
            ));
        }
        self.refuse_entry_writes_before_floor().await?;

        let (status, starred) = match mutation {
            EntryMutation::Status(s) => (Some(s.as_api_str()), None),
            EntryMutation::Starred(b) => (None, Some(b)),
        };
        let body = wire::EntriesUpdateRequest {
            entry_ids: ids.iter().map(|i| i.get()).collect(),
            status,
            starred,
        };
        let payload = serde_json::to_vec(&body)
            .map_err(|e| Error::Protocol(format!("could not encode the entry-state write: {e}")))?;

        let url = self.url("/v1/entries");
        let safe = SafeUrl::from(&url);
        let response = self
            .transport
            .send(reqwest::Method::PUT, url, Some(payload))
            .await?;
        Self::check_status(&response, &safe)
    }

    /// `PUT /v1/feeds/{id}/mark-all-as-read`.
    ///
    /// Interactive, online only. Server applies `published_at < now()` cut-off
    /// at request time; replaying later marks more than user saw. So outbox
    /// expands mark-all into concrete entry ids. See [`crate::db::outbox`].
    ///
    /// Refused below 2.3.2 write floor like every entry-state write; see
    /// [`Self::update_entries`].
    ///
    /// # Errors
    ///
    /// - [`Error::Config`] when server older than 2.3.2. Nothing sent.
    /// - Whatever [`Self::version`] returns when version unknown and fetch
    ///   fails.
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    pub async fn mark_feed_read(&self, feed_id: i64) -> Result<()> {
        self.refuse_entry_writes_before_floor().await?;
        self.put_empty(&format!("/v1/feeds/{feed_id}/mark-all-as-read"))
            .await
    }

    /// As [`Self::mark_feed_read`], for one category.
    ///
    /// # Errors
    ///
    /// As for [`Self::mark_feed_read`].
    pub async fn mark_category_read(&self, category_id: i64) -> Result<()> {
        self.refuse_entry_writes_before_floor().await?;
        self.put_empty(&format!("/v1/categories/{category_id}/mark-all-as-read"))
            .await
    }

    async fn put_empty(&self, path: &str) -> Result<()> {
        let url = self.url(path);
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::PUT, url, None).await?;
        Self::check_status(&response, &safe)
    }

    // ------------------------------------------------ subscriptions

    /// `POST /v1/feeds`: subscribe. §3 keeps add/remove in scope; rewrite
    /// rules, scraper rules, blocklists stay in web UI.
    ///
    /// Returns new feed id, or `0` when success response omits it. Not error:
    /// next sync finds feed anyway.
    ///
    /// # Errors
    ///
    /// - [`Error::Protocol`] if request body can't be encoded or response isn't
    ///   expected JSON.
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status, e.g. feed unfetchable/unparseable
    ///   or already subscribed. `message` carries server's reason.
    pub async fn create_feed(&self, feed_url: &str, category_id: Option<i64>) -> Result<i64> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            feed_url: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            category_id: Option<i64>,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            feed_id: i64,
        }

        let payload = serde_json::to_vec(&Req {
            feed_url,
            category_id,
        })
        .map_err(|e| Error::Protocol(format!("could not encode the subscribe request: {e}")))?;
        let url = self.url("/v1/feeds");
        let safe = SafeUrl::from(&url);
        let response = self
            .transport
            .send(reqwest::Method::POST, url, Some(payload))
            .await?;
        let resp: Resp = Self::decode(&response, &safe)?;
        Ok(resp.feed_id)
    }

    /// `PUT /v1/feeds/{id}`: change feed settings.
    ///
    /// Fields optional, omitted ones untouched: Miniflux
    /// `FeedModificationRequest` is POINTERS, `Patch` applies only present
    /// ones. Full feed object would resend possibly stale values and revert
    /// web-UI changes.
    ///
    /// # Errors
    ///
    /// - [`Error::Protocol`] if request body can't be encoded.
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    pub async fn update_feed(&self, feed_id: i64, update: &FeedPatch) -> Result<()> {
        let payload = serde_json::to_vec(update)
            .map_err(|e| Error::Protocol(format!("could not encode the feed update: {e}")))?;
        let url = self.url(&format!("/v1/feeds/{feed_id}"));
        let safe = SafeUrl::from(&url);
        let response = self
            .transport
            .send(reqwest::Method::PUT, url, Some(payload))
            .await?;
        Self::check_status(&response, &safe)
    }

    /// `DELETE /v1/feeds/{id}`: unsubscribe.
    ///
    /// # Errors
    ///
    /// - [`Error::Transport`] when no response arrives.
    /// - [`Error::Http`] on non-2xx status.
    pub async fn delete_feed(&self, feed_id: i64) -> Result<()> {
        let url = self.url(&format!("/v1/feeds/{feed_id}"));
        let safe = SafeUrl::from(&url);
        let response = self
            .transport
            .send(reqwest::Method::DELETE, url, None)
            .await?;
        Self::check_status(&response, &safe)
    }
}

/// Subset of Miniflux feed settings Vuo lets user change.
///
/// On purpose: §3 leaves rewrite/scraper rules, blocklists to web UI.
/// Credential fields (`username`, `password`, `cookie`, `user_agent`,
/// `proxy_url`) absent: phone form storing site passwords server-side isn't
/// this app's job.
///
/// `None` = don't touch; skipped when serialising, so rename is rename, not
/// full overwrite.
#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct FeedPatch {
    /// Rename the feed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Move the feed to this category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_id: Option<i64>,
    /// "Fetch original content": server downloads each entry's own page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crawler: Option<bool>,
    /// Stop refreshing this feed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// Keep the feed out of the global unread list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_globally: Option<bool>,
}

impl FeedPatch {
    /// True when nothing to send.
    ///
    /// Check before request: empty PUT = wasted radio wake-up.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &FeedPatch::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_string(q: &EntriesQuery) -> String {
        let mut url = Url::parse("https://h.example/v1/entries").unwrap();
        q.apply(&mut url);
        url.query().unwrap_or_default().to_owned()
    }

    /// One query param, PARSED.
    ///
    /// Substring check passes under the widening it guards against:
    /// `limit=1000` matches `limit=10000`, `order=id` matches
    /// `order=id_something`.
    fn param(q: &EntriesQuery, key: &str) -> Option<String> {
        let mut url = Url::parse("https://h.example/v1/entries").unwrap();
        q.apply(&mut url);
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    #[test]
    fn limit_can_never_be_zero_or_unbounded() {
        // 2.2.x: limit=0 = UNLIMITED, full corpus into phone.
        assert_eq!(
            param(&EntriesQuery::keyset(0), "limit").as_deref(),
            Some("1")
        );
        assert_eq!(
            param(&EntriesQuery::keyset(999_999), "limit").as_deref(),
            Some("1000"),
            "the server's own ceiling, exactly -- not merely a value starting with 1000"
        );
        assert_eq!(
            param(&EntriesQuery::default(), "limit").as_deref(),
            Some("100")
        );
    }

    #[test]
    fn direction_is_lowercase() {
        // "ASC" = 400.
        assert_eq!(
            param(&EntriesQuery::keyset(10), "direction").as_deref(),
            Some("asc")
        );
    }

    #[test]
    fn the_sync_query_paginates_on_id() {
        // Other orders unstable: paging silently skips rows.
        let q = EntriesQuery::keyset(500);
        assert_eq!(param(&q, "order").as_deref(), Some("id"));
        assert_eq!(param(&q, "direction").as_deref(), Some("asc"));
    }

    #[test]
    fn an_unset_cursor_omits_the_parameter_entirely() {
        // changed_after=0 would mean "since 1970".
        assert!(
            !query_string(&EntriesQuery::keyset(10).changed_after(None)).contains("changed_after")
        );
        assert!(
            !query_string(&EntriesQuery::keyset(10).changed_after(Some(0)))
                .contains("changed_after")
        );
        assert!(
            query_string(&EntriesQuery::keyset(10).changed_after(Some(42)))
                .contains("changed_after=42")
        );
    }

    #[test]
    fn starred_is_exactly_true_or_false() {
        assert!(
            query_string(&EntriesQuery::keyset(10).starred(Some(true))).contains("starred=true")
        );
        assert!(
            query_string(&EntriesQuery::keyset(10).starred(Some(false))).contains("starred=false")
        );
        assert!(!query_string(&EntriesQuery::keyset(10).starred(None)).contains("starred"));
    }

    #[test]
    fn keyset_cursor_is_after_entry_id_not_offset() {
        let s = query_string(&EntriesQuery::keyset(10).after_entry_id(Some(EntryId(77))));
        assert!(s.contains("after_entry_id=77"), "{s}");
        assert!(
            !s.contains("offset"),
            "offset pagination is unsafe here: {s}"
        );
    }
}
