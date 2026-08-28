//! The Miniflux REST client.
//!
//! §4: Miniflux's own REST API is the interface contract, authenticated with an
//! API key. Not Fever, not Google Reader — those compatibility layers exist for
//! other clients and would cost fidelity for nothing.
//!
//! # Request-building invariants
//!
//! Several query parameters are dangerous rather than merely fiddly, so they
//! are enforced by types here instead of being left to call sites:
//!
//! - **`limit` is never 0.** On 2.3.x a zero limit clamps to 1000, but on
//!   2.2.x it means *unlimited* — a full-corpus dump into a phone's memory.
//!   [`EntriesQuery`] clamps into `1..=1000` and cannot express zero.
//! - **`order` is an enum.** The server validates `order` against a whitelist
//!   and 400s otherwise, and only one value is safe to paginate on (see
//!   [`EntryOrder`]).
//! - **`direction` is lowercase.** `"ASC"` is a 400.
//! - **Timestamps are epoch seconds where 0 means "unset"**, so an unset
//!   cursor omits the parameter rather than sending `0` — which would
//!   otherwise mean "changed after 1970", i.e. everything.
//! - **`entry_ids` is never empty.** An empty list is a hard 400.

use url::Url;

use crate::api::transport::{BoundedResponse, Transport};
use crate::api::wire;
use crate::error::{Error, Result};
use crate::model::{EntryId, EntryStatus, ServerVersion};
use crate::redact::SafeUrl;

/// The only sort orders Miniflux accepts. Sending anything else is a 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryOrder {
    /// **The only order safe for pagination.**
    ///
    /// The server's generated `ORDER BY` contains exactly one expression with
    /// no id tiebreaker, so every other order is *unstable*: rows that compare
    /// equal can be returned in a different order on each query. With
    /// `changed_at`, a single mark-all-as-read gives thousands of rows an
    /// identical timestamp, and paging through them will silently skip and
    /// duplicate. `id` is the primary key, so it is a total order with no ties.
    Id,
    PublishedAt,
    ChangedAt,
    CreatedAt,
    CategoryTitle,
    CategoryId,
    Status,
    Title,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn as_str(self) -> &'static str {
        // Lowercase only: "ASC" is rejected with a 400.
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
    statuses: Vec<EntryStatus>,
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
            statuses: Vec::new(),
            starred: None,
            feed_id: None,
            category_id: None,
        }
    }
}

impl EntriesQuery {
    /// The query shape the sync engine uses: an id keyset scan.
    #[must_use]
    pub fn keyset(limit: u32) -> Self {
        EntriesQuery { order: EntryOrder::Id, direction: SortDirection::Asc, ..Default::default() }
            .with_limit(limit)
    }

    /// Clamps into `1..=1000`. Zero and negative values cannot be expressed.
    #[must_use]
    pub fn with_limit(mut self, limit: u32) -> Self {
        self.limit = limit.clamp(1, 1000);
        self
    }

    #[must_use]
    pub fn after_entry_id(mut self, id: Option<EntryId>) -> Self {
        self.after_entry_id = id;
        self
    }

    /// Epoch seconds. `None` omits the parameter entirely; passing `Some(0)`
    /// would mean "everything since 1970", which is never what a caller means.
    #[must_use]
    pub fn changed_after(mut self, epoch_secs: Option<i64>) -> Self {
        self.changed_after = epoch_secs.filter(|s| *s > 0);
        self
    }

    #[must_use]
    pub fn with_status(mut self, status: EntryStatus) -> Self {
        self.statuses.push(status);
        self
    }

    #[must_use]
    pub fn starred(mut self, starred: Option<bool>) -> Self {
        self.starred = starred;
        self
    }

    #[must_use]
    pub fn feed_id(mut self, id: Option<i64>) -> Self {
        self.feed_id = id;
        self
    }

    #[must_use]
    pub fn category_id(mut self, id: Option<i64>) -> Self {
        self.category_id = id;
        self
    }

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
        // Multiple statuses are expressed by repeating the key; the server
        // ORs them.
        for status in &self.statuses {
            q.append_pair("status", status.as_api_str());
        }
        if let Some(starred) = self.starred {
            // Exactly "true"/"false": /v1/entries/ids 400s on anything else.
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

/// What an entry-state write should set. Both are absolute values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryMutation {
    Status(EntryStatus),
    Starred(bool),
}

#[derive(Debug, Clone)]
pub struct MinifluxClient {
    transport: Transport,
}

impl MinifluxClient {
    #[must_use]
    pub fn new(transport: Transport) -> Self {
        MinifluxClient { transport }
    }

    #[must_use]
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    fn url(&self, path: &str) -> Result<Url> {
        // `join` on a base whose path lacks a trailing slash would replace the
        // last segment, so build against a normalised base. This matters for
        // instances hosted under a sub-path.
        let mut base = self.transport.origin().clone();
        let base_path = base.path().trim_end_matches('/').to_owned();
        base.set_path(&format!("{base_path}/{}", path.trim_start_matches('/')));
        base.set_query(None);
        Ok(base)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T> {
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::GET, url, None).await?;
        Self::decode(response, &safe)
    }

    fn decode<T: serde::de::DeserializeOwned>(
        response: BoundedResponse,
        safe: &SafeUrl,
    ) -> Result<T> {
        Self::check_status(&response, safe)?;
        serde_json::from_slice(&response.body).map_err(|e| {
            // The parse error's own message can quote response bytes, which
            // are foreign; keep only the structural position.
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
        // Miniflux always answers errors as {"error_message": "..."}. The
        // message is foreign text and must be rendered as plain text.
        let message = serde_json::from_slice::<wire::ApiError>(&response.body)
            .ok()
            .map(|e| e.error_message)
            .filter(|m| !m.is_empty());
        Err(Error::Http { status: response.status, endpoint: safe.clone(), message })
    }

    // ------------------------------------------------------------ discovery

    /// `GET /v1/version`. Called once at connect: several request-building
    /// rules branch on the answer.
    pub async fn version(&self) -> Result<ServerVersion> {
        let w: wire::VersionResponse = self.get_json(self.url("/v1/version")?).await?;
        Ok(ServerVersion::parse(&w.version).unwrap_or_default())
    }

    /// `GET /v1/me`. Used to verify that an API key works during setup.
    pub async fn me(&self) -> Result<wire::User> {
        self.get_json(self.url("/v1/me")?).await
    }

    // -------------------------------------------------------------- reading

    pub async fn categories(&self) -> Result<Vec<wire::Category>> {
        self.get_json(self.url("/v1/categories")?).await
    }

    pub async fn feeds(&self) -> Result<Vec<wire::Feed>> {
        self.get_json(self.url("/v1/feeds")?).await
    }

    pub async fn entries(&self, query: &EntriesQuery) -> Result<(wire::EntriesResponse, Option<chrono::DateTime<chrono::Utc>>)> {
        let mut url = self.url("/v1/entries")?;
        query.apply(&mut url);
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::GET, url, None).await?;
        let date = response.server_date;
        let body: wire::EntriesResponse = Self::decode(response, &safe)?;
        Ok((body, date))
    }

    pub async fn entry(&self, id: EntryId) -> Result<wire::Entry> {
        self.get_json(self.url(&format!("/v1/entries/{}", id.get()))?).await
    }

    /// `GET /v1/entries/ids`. Present only from 2.3.2; check
    /// [`ServerVersion::has_entry_ids_endpoint`] before calling.
    pub async fn entry_ids(&self, query: &EntriesQuery, offset: u32) -> Result<wire::EntryIdsResponse> {
        let mut url = self.url("/v1/entries/ids")?;
        query.apply(&mut url);
        url.query_pairs_mut().append_pair("offset", &offset.to_string());
        self.get_json(url).await
    }

    pub async fn feed_icon(&self, feed_id: i64) -> Result<wire::Icon> {
        self.get_json(self.url(&format!("/v1/feeds/{feed_id}/icon"))?).await
    }

    pub async fn counters(&self) -> Result<wire::FeedCounters> {
        self.get_json(self.url("/v1/feeds/counters")?).await
    }

    /// `GET /v1/entries/{id}/fetch-content`.
    ///
    /// §3 forbids a client-side readability port; this is the server-side
    /// equivalent, which is the endpoint that exists precisely so clients do
    /// not write one. `update_content` is deliberately never sent: it makes
    /// this GET persist the scrape server-side, which must not happen
    /// speculatively.
    pub async fn fetch_original_content(&self, id: EntryId) -> Result<wire::EntryContentResponse> {
        self.get_json(self.url(&format!("/v1/entries/{}/fetch-content", id.get()))?).await
    }

    // -------------------------------------------------------------- writing

    /// `PUT /v1/entries` — the only idempotent entry-state write.
    ///
    /// Both `status` and `starred` are absolute sets, which is what makes
    /// outbox replay safe after an ambiguous timeout. The `/star` and
    /// `/bookmark` routes are *not* usable for this: they share a handler
    /// whose SQL is `SET starred = NOT starred`, so replaying one flips the
    /// value back. Vuo never calls them.
    ///
    /// Unknown ids are silently ignored by the server (there is no
    /// rows-affected check), so a 204 does not mean every id existed.
    pub async fn update_entries(&self, ids: &[EntryId], mutation: EntryMutation) -> Result<()> {
        if ids.is_empty() {
            // A hard 400 server-side; refuse locally so it is not mistaken for
            // a server fault by the outbox's retry classifier.
            return Err(Error::Config("refusing to send an empty entry id list".to_owned()));
        }

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
            .map_err(|_| Error::Protocol("could not encode the update request".to_owned()))?;

        let url = self.url("/v1/entries")?;
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::PUT, url, Some(payload)).await?;
        Self::check_status(&response, &safe)
    }

    /// `PUT /v1/feeds/{id}/mark-all-as-read`.
    ///
    /// Only for interactive, online use. This endpoint applies a server-side
    /// `published_at < now()` cut-off captured at request time, so replaying a
    /// queued one later marks strictly more entries than the user saw — which
    /// is why the outbox expands mark-all into concrete entry ids instead of
    /// queueing this call. See [`crate::outbox`].
    pub async fn mark_feed_read(&self, feed_id: i64) -> Result<()> {
        self.put_empty(&format!("/v1/feeds/{feed_id}/mark-all-as-read")).await
    }

    pub async fn mark_category_read(&self, category_id: i64) -> Result<()> {
        self.put_empty(&format!("/v1/categories/{category_id}/mark-all-as-read")).await
    }

    /// `PUT /v1/users/{id}/mark-all-as-read`. The id must be the authenticated
    /// user's own or the server answers 403.
    pub async fn mark_user_read(&self, user_id: i64) -> Result<()> {
        self.put_empty(&format!("/v1/users/{user_id}/mark-all-as-read")).await
    }

    /// `PUT /v1/feeds/{id}/refresh` — ask the server to poll a feed now.
    ///
    /// Note what this is *not*: Vuo never fetches a feed URL itself. §3 makes
    /// local feed fetching the single most important boundary in the project.
    pub async fn refresh_feed(&self, feed_id: i64) -> Result<()> {
        self.put_empty(&format!("/v1/feeds/{feed_id}/refresh")).await
    }

    async fn put_empty(&self, path: &str) -> Result<()> {
        let url = self.url(path)?;
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::PUT, url, None).await?;
        Self::check_status(&response, &safe)
    }

    // ------------------------------------------------ subscriptions

    /// `POST /v1/feeds` — subscribe. §3 keeps add/remove in scope and leaves
    /// rewrite rules, scraper rules and blocklists to the web UI.
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

        let payload = serde_json::to_vec(&Req { feed_url, category_id })
            .map_err(|_| Error::Protocol("could not encode the subscribe request".to_owned()))?;
        let url = self.url("/v1/feeds")?;
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::POST, url, Some(payload)).await?;
        let resp: Resp = Self::decode(response, &safe)?;
        Ok(resp.feed_id)
    }

    /// `DELETE /v1/feeds/{id}` — unsubscribe.
    pub async fn delete_feed(&self, feed_id: i64) -> Result<()> {
        let url = self.url(&format!("/v1/feeds/{feed_id}"))?;
        let safe = SafeUrl::from(&url);
        let response = self.transport.send(reqwest::Method::DELETE, url, None).await?;
        Self::check_status(&response, &safe)
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

    #[test]
    fn limit_can_never_be_zero_or_unbounded() {
        // On 2.2.x limit=0 means UNLIMITED: a full-corpus dump into a phone.
        assert!(query_string(&EntriesQuery::keyset(0)).contains("limit=1"));
        assert!(query_string(&EntriesQuery::keyset(999_999)).contains("limit=1000"));
        assert!(query_string(&EntriesQuery::default()).contains("limit=100"));
    }

    #[test]
    fn direction_is_lowercase() {
        // "ASC" is a 400.
        assert!(query_string(&EntriesQuery::keyset(10)).contains("direction=asc"));
    }

    #[test]
    fn the_sync_query_paginates_on_id() {
        // Any other order is unstable and silently skips rows under paging.
        let q = EntriesQuery::keyset(500);
        let s = query_string(&q);
        assert!(s.contains("order=id"), "{s}");
        assert!(s.contains("direction=asc"), "{s}");
    }

    #[test]
    fn an_unset_cursor_omits_the_parameter_entirely() {
        // Sending changed_after=0 would mean "everything since 1970".
        assert!(!query_string(&EntriesQuery::keyset(10).changed_after(None)).contains("changed_after"));
        assert!(!query_string(&EntriesQuery::keyset(10).changed_after(Some(0))).contains("changed_after"));
        assert!(query_string(&EntriesQuery::keyset(10).changed_after(Some(42))).contains("changed_after=42"));
    }

    #[test]
    fn multiple_statuses_repeat_the_key() {
        let q = EntriesQuery::keyset(10)
            .with_status(EntryStatus::Read)
            .with_status(EntryStatus::Unread);
        let s = query_string(&q);
        assert!(s.contains("status=read") && s.contains("status=unread"), "{s}");
    }

    #[test]
    fn starred_is_exactly_true_or_false() {
        assert!(query_string(&EntriesQuery::keyset(10).starred(Some(true))).contains("starred=true"));
        assert!(query_string(&EntriesQuery::keyset(10).starred(Some(false))).contains("starred=false"));
        assert!(!query_string(&EntriesQuery::keyset(10).starred(None)).contains("starred"));
    }

    #[test]
    fn keyset_cursor_is_after_entry_id_not_offset() {
        let s = query_string(&EntriesQuery::keyset(10).after_entry_id(Some(EntryId(77))));
        assert!(s.contains("after_entry_id=77"), "{s}");
        assert!(!s.contains("offset"), "offset pagination is unsafe here: {s}");
    }
}
