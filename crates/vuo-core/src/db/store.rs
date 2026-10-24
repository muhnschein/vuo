//! Reads and writes against mirror.
//!
//! Conflict rule up front: only place "server said X" meets "user did Y"; wrong
//! = silently lost user actions.
//!
//! **Per field, never per entry.** Pulled entry field with pending outbox
//! intent: local wins *that field only*, server's value for rest. Per entry,
//! remote read-status change clobbers local pending star (or vice versa).
//!
//! Vuo sees its *own* writes return: each mutation bumps server `changed_at`,
//! so replayed outbox row echoes into next pull. Rule makes echo a no-op.

use rusqlite::{OptionalExtension as _, Transaction};

use crate::db::outbox::{self, DesiredValue, OutboxField};
use crate::error::Result;
use crate::model::{
    Category, CategoryId, Entry, EntryId, EntryStatus, Feed, FeedId, Icon, IconId, ImageFormat,
};
use crate::search::MatchKind;

fn ts(t: Option<chrono::DateTime<chrono::Utc>>) -> Option<i64> {
    t.map(|t| t.timestamp())
}

/// INTEGER column held as `i32`. SQLite stores 64 bits; out-of-range clamps, no
/// wrap.
fn saturating_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn from_ts(secs: Option<i64>) -> Option<chrono::DateTime<chrono::Utc>> {
    secs.and_then(|s| chrono::DateTime::from_timestamp(s, 0))
}

/// Sort key for user-visible name.
///
/// SQLite `ORDER BY title` = BYTE comparison: upper case before lower ("Zeit"
/// before "heise"). `COLLATE NOCASE` folds ASCII only: "Ärzteblatt" after
/// "Zeit".
///
/// Folds case, then German umlauts per DIN 5007-1: what German reader expects,
/// no worse than byte order otherwise. Not full Unicode collation: big table
/// and dependency for a few dozen names.
#[must_use]
pub fn name_sort_key(title: &str) -> String {
    let mut key = String::with_capacity(title.len());
    for c in title.to_lowercase().chars() {
        match c {
            // `to_lowercase` keeps 'ß' as is, not "ss".
            'ß' => key.push_str("ss"),
            'ä' | 'á' | 'à' | 'â' | 'å' | 'ã' => key.push('a'),
            // è é ê ë: contiguous range.
            'è'..='ë' => key.push('e'),
            // ì í î ï
            'ì'..='ï' => key.push('i'),
            // ò ó ô õ, plus ö (F6, outside range).
            'ò'..='õ' | 'ö' => key.push('o'),
            // ù ú û, plus ü (FC) likewise.
            'ù'..='û' | 'ü' => key.push('u'),
            'ç' => key.push('c'),
            'ñ' => key.push('n'),
            other => key.push(other),
        }
    }
    key
}

// ------------------------------------------------------------- categories

/// Insert or update server category, stamped with sync `generation` that saw
/// it.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if write fails.
pub fn upsert_category(tx: &Transaction<'_>, c: &Category, generation: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO categories (id, title, hide_globally, last_seen_sync)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
             title = excluded.title,
             hide_globally = excluded.hide_globally,
             last_seen_sync = excluded.last_seen_sync",
        rusqlite::params![c.id.get(), c.title, i64::from(c.hide_globally), generation],
    )?;
    Ok(())
}

/// Every category in mirror, [`name_sort_key`] order.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn categories(tx: &rusqlite::Connection) -> Result<Vec<Category>> {
    let mut stmt = tx.prepare("SELECT id, title, hide_globally FROM categories")?;
    let rows = stmt.query_map([], |r| {
        Ok(Category {
            id: CategoryId(r.get(0)?),
            title: r.get(1)?,
            hide_globally: r.get::<_, i64>(2)? != 0,
        })
    })?;
    let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    // `sort_by_cached_key`, not `sort_by`: key is fresh String, rebuilt per
    // comparison otherwise.
    out.sort_by_cached_key(|c| name_sort_key(&c.title));
    Ok(out)
}

// ------------------------------------------------------------------ feeds

/// Insert or update server feed, stamped with sync `generation` that saw it.
///
/// Category feed names but mirror lacks: placeholder with empty title, so FK
/// can't abort sync.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if either write fails.
pub fn upsert_feed(tx: &Transaction<'_>, f: &Feed, generation: i64) -> Result<()> {
    // `feeds.category_id` references `categories(id)`; listings fetched
    // separately, so category created between calls is missing and FK would
    // abort sync over one row. Placeholder; next taxonomy pull fills title.
    if let Some(category) = f.category_id {
        tx.execute(
            "INSERT INTO categories (id, title) VALUES (?1, '')
             ON CONFLICT(id) DO NOTHING",
            [category.get()],
        )?;
    }
    tx.execute(
        "INSERT INTO feeds (id, category_id, title, site_url, feed_url, icon_id, checked_at,
                            parsing_error_message, parsing_error_count, disabled, hide_globally,
                            crawler, last_seen_sync)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET
             category_id = excluded.category_id,
             title = excluded.title,
             site_url = excluded.site_url,
             feed_url = excluded.feed_url,
             icon_id = excluded.icon_id,
             checked_at = excluded.checked_at,
             parsing_error_message = excluded.parsing_error_message,
             parsing_error_count = excluded.parsing_error_count,
             disabled = excluded.disabled,
             hide_globally = excluded.hide_globally,
             crawler = excluded.crawler,
             last_seen_sync = excluded.last_seen_sync",
        rusqlite::params![
            f.id.get(),
            f.category_id.map(CategoryId::get),
            f.title,
            f.site_url.as_ref().map(crate::content::MediaUrl::as_str),
            f.feed_url.as_ref().map(crate::content::MediaUrl::as_str),
            f.icon_id.map(IconId::get),
            ts(f.checked_at),
            f.parsing_error_message,
            i64::from(f.parsing_error_count),
            i64::from(f.disabled),
            i64::from(f.hide_globally),
            i64::from(f.crawler),
            generation,
        ],
    )?;
    Ok(())
}

/// Every feed in mirror, [`name_sort_key`] order.
///
/// Stored URL no longer passing [`MediaUrl::parse`](crate::content::MediaUrl::parse) reads
/// as `None`.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn feeds(conn: &rusqlite::Connection) -> Result<Vec<Feed>> {
    let mut stmt = conn.prepare(
        "SELECT id, category_id, title, site_url, feed_url, icon_id, checked_at,
                parsing_error_message, parsing_error_count, disabled, hide_globally,
                crawler
         FROM feeds",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Feed {
            id: FeedId(r.get(0)?),
            category_id: r.get::<_, Option<i64>>(1)?.map(CategoryId),
            title: r.get(2)?,
            site_url: r
                .get::<_, Option<String>>(3)?
                .and_then(|s| crate::content::MediaUrl::parse(&s)),
            feed_url: r
                .get::<_, Option<String>>(4)?
                .and_then(|s| crate::content::MediaUrl::parse(&s)),
            icon_id: r.get::<_, Option<i64>>(5)?.map(IconId),
            checked_at: from_ts(r.get(6)?),
            parsing_error_message: r.get(7)?,
            parsing_error_count: saturating_i32(r.get(8)?),
            disabled: r.get::<_, i64>(9)? != 0,
            hide_globally: r.get::<_, i64>(10)? != 0,
            crawler: r.get::<_, i64>(11)? != 0,
        })
    })?;
    let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    // Sorted here, not SQL (see `name_sort_key`). Cached: key is fresh String.
    out.sort_by_cached_key(|f| name_sort_key(&f.title));
    Ok(out)
}

/// Remove feed, its entries, and intents queued against them.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if any delete fails.
pub fn delete_feed(tx: &Transaction<'_>, id: FeedId) -> Result<()> {
    // Outbox rows first, mandatory (same rule as `delete_entry`): intent for
    // entry server lacks never confirms (Miniflux ignores unknown ids, 204,
    // `confirm` matches nothing); row would sit forever in "changes waiting to
    // be sent".
    tx.execute(
        "DELETE FROM outbox WHERE entry_id IN (SELECT id FROM entries WHERE feed_id = ?1)",
        [id.get()],
    )?;
    // Entries have no FK to feeds (feed may arrive before or after its
    // entries), so removed explicitly.
    tx.execute("DELETE FROM entries WHERE feed_id = ?1", [id.get()])?;
    tx.execute("DELETE FROM feeds WHERE id = ?1", [id.get()])?;
    Ok(())
}

// ---------------------------------------------------------------- entries

/// Whether entry reaching mirror for FIRST time is news.
///
/// Caller decides: pull knows if this is mirror's first pass and where window
/// began. Rule: [`crate::sync::pull::arrival_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// INSERT marks entry arrived, if unread. Update of held row changes
    /// nothing.
    Announce,
    /// Insert without marking. Everything except incremental pull.
    Quiet,
}

/// Insert or update entry, honouring pending local intents.
///
/// Conflict rule per field (module docs): field with pending outbox row keeps
/// local value; rest take server's.
///
/// `arrival` reaches only INSERT half. `DO UPDATE` doesn't name `arrived`: held
/// row keeps its value, so "new" = new to this phone, not seen again this pass.
/// Cursor re-reads a minute of overlap each pass; those rows mustn't return as
/// news.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if pending intents can't be read or
/// any write to `entries`/`enclosures` fails.
pub fn upsert_entry(
    tx: &Transaction<'_>,
    e: &Entry,
    generation: i64,
    arrival: Arrival,
) -> Result<()> {
    let pending_status = outbox::pending_for(tx, e.id, OutboxField::Status)?;
    let pending_starred = outbox::pending_for(tx, e.id, OutboxField::Starred)?;

    let status = match pending_status {
        Some(DesiredValue::Status(s)) => s,
        _ => e.status,
    };
    let starred = match pending_starred {
        Some(DesiredValue::Starred(b)) => b,
        _ => e.starred,
    };

    let tags = serde_json::to_string(&e.tags).unwrap_or_else(|_| "[]".to_owned());

    // Only unread arrival is news. One already read (other device) isn't.
    let arrived = arrival == Arrival::Announce && status == EntryStatus::Unread;

    tx.execute(
        "INSERT INTO entries (id, feed_id, status, starred, title, url, comments_url, author,
                              content, published_at, created_at, changed_at, reading_time, tags,
                              last_seen_sync, arrived)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         -- `arrived` is absent below ON PURPOSE: see the doc comment.
         ON CONFLICT(id) DO UPDATE SET
             feed_id = excluded.feed_id,
             status = excluded.status,
             starred = excluded.starred,
             title = excluded.title,
             url = excluded.url,
             comments_url = excluded.comments_url,
             author = excluded.author,
             -- Never clobber a body the user asked the server to scrape.
             -- fetch-content is not persisted server-side, so the feed own
             -- (usually truncated) content is what a sync brings back; taking
             -- it would silently undo fetch-original on the next refresh.
             content = CASE WHEN entries.content_scraped = 1
                            THEN entries.content
                            ELSE excluded.content END,
             published_at = excluded.published_at,
             created_at = excluded.created_at,
             changed_at = excluded.changed_at,
             reading_time = excluded.reading_time,
             tags = excluded.tags,
             last_seen_sync = excluded.last_seen_sync",
        rusqlite::params![
            e.id.get(),
            e.feed_id.get(),
            status.as_api_str(),
            i64::from(starred),
            e.title,
            e.url.as_ref().map(crate::content::MediaUrl::as_str),
            e.comments_url
                .as_ref()
                .map(crate::content::MediaUrl::as_str),
            e.author,
            e.content,
            ts(e.published_at),
            ts(e.created_at),
            ts(e.changed_at),
            i64::from(e.reading_time),
            tags,
            generation,
            i64::from(arrived),
        ],
    )?;

    tx.execute("DELETE FROM enclosures WHERE entry_id = ?1", [e.id.get()])?;
    for enc in &e.enclosures {
        tx.execute(
            "INSERT OR REPLACE INTO enclosures (id, entry_id, url, mime_type, size)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                enc.id,
                e.id.get(),
                enc.url.as_ref().map(crate::content::MediaUrl::as_str),
                enc.mime_type,
                enc.size
            ],
        )?;
    }
    Ok(())
}

/// Delete entry server no longer has.
///
/// Pending outbox intent goes too: replaying against deleted id is pointless,
/// and Miniflux ignores unknown ids, so row would never clear.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if either delete fails.
pub fn delete_entry(tx: &Transaction<'_>, id: EntryId) -> Result<()> {
    tx.execute("DELETE FROM outbox WHERE entry_id = ?1", [id.get()])?;
    tx.execute("DELETE FROM entries WHERE id = ?1", [id.get()])?;
    Ok(())
}

/// Forget read, unfavourited entries published before `cutoff`.
///
/// Mirror = cache of server, not archive; only thing that shrinks it (else file
/// grows forever).
///
/// Never touched, each exclusion load-bearing:
///
/// - **Unread.** Not seen yet; deleting = app losing it.
/// - **Favourited.** User said "keep this". Deleting favourites = bug with a
///   settings page.
/// - **Queued intent.** Outbox row = unconfirmed local change; deleting entry
///   drops user's tap. Just-read article survives until `read` reaches server,
///   pruned later.
///
/// No feed date: aged from `created_at`; neither: older than any cutoff (read,
/// unfavourited, undated = exactly what this collects).
///
/// Sync-safe: pull cursor `changed_after` only moves forward; reconcile only
/// deletes rows server lacks. Nothing pruned returns; nothing here triggers
/// reconcile deletes.
///
/// Returns count removed.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if delete fails.
pub fn prune_entries(tx: &Transaction<'_>, cutoff: i64) -> Result<usize> {
    // Enclosures follow via `ON DELETE CASCADE`; `foreign_keys` ON for every
    // connection here (`db::Database::configure`).
    let removed = tx.execute(
        "DELETE FROM entries \
          WHERE status = 'read' \
            AND starred = 0 \
            AND COALESCE(published_at, created_at, 0) < ?1 \
            AND id NOT IN (SELECT entry_id FROM outbox)",
        [cutoff],
    )?;
    Ok(removed)
}

/// Every entry id Vuo holds locally.
///
/// # Errors
///
/// Returns [`Error::Db`](crate::error::Error::Db) if the query fails.
pub fn local_entry_ids(conn: &rusqlite::Connection) -> Result<Vec<EntryId>> {
    let mut stmt = conn.prepare("SELECT id FROM entries")?;
    let rows = stmt.query_map([], |r| Ok(EntryId(r.get(0)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Per-feed entry counts, for cheap divergence check against `GET
/// /v1/feeds/counters`.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn entry_counts_by_feed(
    conn: &rusqlite::Connection,
) -> Result<std::collections::HashMap<i64, i64>> {
    let mut stmt = conn.prepare("SELECT feed_id, COUNT(*) FROM entries GROUP BY feed_id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (k, v) = row?;
        out.insert(k, v);
    }
    Ok(out)
}

/// Per-feed unread counts, for the feed list's badges.
///
/// # Errors
///
/// Returns [`Error::Db`](crate::error::Error::Db) if the query fails.
pub fn unread_counts_by_feed(
    conn: &rusqlite::Connection,
) -> Result<std::collections::HashMap<i64, i64>> {
    let mut stmt = conn.prepare(
        "SELECT feed_id, COUNT(*) FROM entries WHERE status = 'unread' GROUP BY feed_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (k, v) = row?;
        out.insert(k, v);
    }
    Ok(out)
}

/// How UI wants entries listed.
///
/// `Unread` and `All` omit globally hidden feeds (`hide_globally` on feed or
/// category), as Miniflux's unread list (`WithGloballyVisible`). Other three
/// don't: Miniflux keeps hidden feeds on starred, feed, category pages; empty
/// feed opened by name would look broken. Entry whose feed mirror lacks never
/// hidden: `NOT IN` hidden ids, not `IN` visible ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryFilter {
    /// Unread entries in feeds not hidden globally.
    Unread,
    /// Starred entries, from every feed.
    Starred,
    /// Every entry in feeds not hidden globally.
    All,
    /// Every entry in the feed with this id.
    Feed(i64),
    /// Every entry in a feed of the category with this id.
    Category(i64),
}

/// Row as entry LIST needs it: everything list draws, no article body.
///
/// Body is why this exists. List capped at 500 rows, bodies run tens of KB:
/// reading [`Entry`] for lists hauled MBs from SQLite per reload, discarded
/// unread, ×4 (one model per scope tab, all reload on same generation bump).
/// 5,357-entry mirror, 10 KB bodies: 20 MB per bump at QML poll rate. Allocator
/// keeps it: resident memory only grows.
///
/// Body needed → ask for one entry by id ([`entry`]), as opening article does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryListRow {
    /// The entry.
    pub id: EntryId,
    /// The feed it belongs to.
    pub feed_id: FeedId,
    /// Read or unread, pending local change applied.
    pub status: EntryStatus,
    /// Whether it is starred.
    pub starred: bool,
    /// FOREIGN TEXT: feed's own words.
    pub title: String,
    /// Article link, re-checked against URL policy every read; `None` when
    /// absent or no longer passing.
    pub url: Option<String>,
    /// FOREIGN TEXT.
    pub author: String,
    /// When the feed says it was published, in Unix seconds.
    pub published_at: Option<i64>,
    /// The server's estimate of reading time, in minutes.
    pub reading_time: i32,
}

/// List entries for UI, newest first.
///
/// Each filter = own complete literal statement. Shared column list via
/// `format!` would be harmless (constant text), but §9.4 is absolute: no
/// `format!` query, anywhere. Repetition = price of checkable guarantee;
/// `db::tests::no_sql_in_this_crate_is_built_by_formatting` checks it.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn list_entries(
    conn: &rusqlite::Connection,
    filter: EntryFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<EntryListRow>> {
    const UNREAD: &str = "SELECT id, feed_id, status, starred, title, url, author, published_at, \
        reading_time FROM entries \
        WHERE status = 'unread' \
        AND feed_id NOT IN (SELECT id FROM feeds WHERE hide_globally = 1 \
        OR category_id IN (SELECT id FROM categories WHERE hide_globally = 1)) \
        ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const STARRED: &str = "SELECT id, feed_id, status, starred, title, url, author, published_at, \
        reading_time FROM entries \
        WHERE starred = 1 ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const ALL: &str = "SELECT id, feed_id, status, starred, title, url, author, published_at, \
        reading_time FROM entries \
        WHERE feed_id NOT IN (SELECT id FROM feeds WHERE hide_globally = 1 \
        OR category_id IN (SELECT id FROM categories WHERE hide_globally = 1)) \
        ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const BY_FEED: &str = "SELECT id, feed_id, status, starred, title, url, author, published_at, \
        reading_time FROM entries \
        WHERE feed_id = ?3 ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const BY_CATEGORY: &str = "SELECT id, feed_id, status, starred, title, url, author, \
        published_at, reading_time FROM entries \
        WHERE feed_id IN (SELECT id FROM feeds WHERE category_id = ?3) \
        ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";

    let mut out = Vec::new();
    match filter {
        EntryFilter::Unread | EntryFilter::Starred | EntryFilter::All => {
            let sql = match filter {
                EntryFilter::Unread => UNREAD,
                EntryFilter::Starred => STARRED,
                _ => ALL,
            };
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query(rusqlite::params![limit, offset])?;
            while let Some(row) = rows.next()? {
                out.push(row_to_list_row(row)?);
            }
        }
        EntryFilter::Feed(id) | EntryFilter::Category(id) => {
            let sql = if matches!(filter, EntryFilter::Feed(_)) {
                BY_FEED
            } else {
                BY_CATEGORY
            };
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query(rusqlite::params![limit, offset, id])?;
            while let Some(row) = rows.next()? {
                out.push(row_to_list_row(row)?);
            }
        }
    }
    Ok(out)
}

/// One list row by id, for a row list holds that its query no longer returns
/// (article reader just finished).
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn entry_list_row(conn: &rusqlite::Connection, id: EntryId) -> Result<Option<EntryListRow>> {
    let row = conn
        .query_row(
            "SELECT id, feed_id, status, starred, title, url, author, published_at, \
             reading_time FROM entries WHERE id = ?1",
            [id.get()],
            row_to_list_row,
        )
        .optional()?;
    Ok(row)
}

/// One article search matched: where, which. All search keeps for unshown
/// result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Where it matched = group it's listed in.
    pub kind: MatchKind,
    /// The article.
    pub id: EntryId,
}

/// One feed search matched by name (see [`search_feeds`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedMatch {
    /// The feed.
    pub id: FeedId,
    /// FOREIGN TEXT: the feed's name.
    pub title: String,
    /// How many of its articles are unread, as the feed list shows it.
    pub unread: i64,
}

/// Results per group: articles in two, feeds in third.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchCounts {
    /// Articles matched by title.
    pub title: usize,
    /// Feeds matched by name.
    pub feed: usize,
    /// Articles matched by body text only.
    pub text: usize,
}

impl SearchCounts {
    /// Count article `matches` by group, and `feeds` as the feed group.
    #[must_use]
    pub fn of(matches: &[SearchMatch], feeds: &[FeedMatch]) -> SearchCounts {
        let mut counts = SearchCounts {
            feed: feeds.len(),
            ..SearchCounts::default()
        };
        for m in matches {
            match m.kind {
                MatchKind::Title => counts.title = counts.title.saturating_add(1),
                MatchKind::Feed => counts.feed = counts.feed.saturating_add(1),
                MatchKind::Text => counts.text = counts.text.saturating_add(1),
            }
        }
        counts
    }

    /// The count for one group.
    #[must_use]
    pub fn get(&self, kind: MatchKind) -> usize {
        match kind {
            MatchKind::Title => self.title,
            MatchKind::Feed => self.feed,
            MatchKind::Text => self.text,
        }
    }
}

/// One found article, with what its row needs to say why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// What entry list would show for article.
    pub row: EntryListRow,
    /// Where it matched = group it's listed in.
    pub kind: MatchKind,
    /// FOREIGN TEXT: source feed name; empty if mirror no longer has feed.
    pub feed_title: String,
    /// Body lines around match, plain text. [`MatchKind::Text`] hits only
    /// (others: row shows match). See [`crate::search::SearchQuery::excerpt`].
    pub excerpt: Option<String>,
}

/// Every article search matches, by id: grouped by where (title, then body),
/// newest first in group. Feeds: [`search_feeds`].
///
/// What matches: [`crate::search::SearchQuery::classify`]. No terms = nothing:
/// empty field isn't request for whole mirror.
///
/// Every feed searched, incl. ones hidden from Unread/All: search = asking by
/// name; existing but unfound reads as nonexistent.
///
/// Keeps only id and group per match (few bytes): enough for counts and order.
/// Every body examined, none held; shown rows read by [`search_hits`].
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails, incl. inside
/// `vuo_search_kind` SQL function.
pub fn search_matches(conn: &rusqlite::Connection, query: &str) -> Result<Vec<SearchMatch>> {
    if crate::search::SearchQuery::parse(query).is_none() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT e.id, e.published_at, \
         vuo_search_kind(?1, e.title, e.content) FROM entries e",
    )?;
    let mut rows = stmt.query([query])?;
    let mut found: Vec<(MatchKind, Option<i64>, i64)> = Vec::new();
    while let Some(row) = rows.next()? {
        if let Some(kind) = MatchKind::from_sql(row.get(2)?) {
            found.push((kind, row.get(1)?, row.get(0)?));
        }
    }
    // List's own order within group: newest first, undated after dated, id
    // breaks ties.
    found.sort_unstable_by_key(|&(kind, published, id)| {
        (kind, std::cmp::Reverse(published), std::cmp::Reverse(id))
    });
    Ok(found
        .into_iter()
        .map(|(kind, _, id)| SearchMatch {
            kind,
            id: EntryId(id),
        })
        .collect())
}

/// Every feed whose name holds every search term, feed-list order (see
/// [`crate::search::SearchQuery::matches`]).
///
/// Every feed, hidden or not, as [`search_matches`]. Held whole, not by id: few
/// dozen feeds, name and count each.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn search_feeds(conn: &rusqlite::Connection, query: &str) -> Result<Vec<FeedMatch>> {
    let Some(parsed) = crate::search::SearchQuery::parse(query) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT f.id, f.title, \
         (SELECT COUNT(*) FROM entries e WHERE e.feed_id = f.id AND e.status = 'unread') \
         FROM feeds f",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(FeedMatch {
            id: FeedId(r.get(0)?),
            title: r.get(1)?,
            unread: r.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        let feed = row?;
        if parsed.matches(&feed.title) {
            out.push(feed);
        }
    }
    // As `feeds` sorts, so group reads like feed list; id between equal names,
    // so order stable across reads.
    out.sort_by_cached_key(|f| (name_sort_key(&f.title), f.id.get()));
    Ok(out)
}

/// What result rows for `matches` show, same order: list row, feed name,
/// excerpt for body hit.
///
/// One statement per row (§9.4); body read only when excerpt needs it. Match
/// whose article vanished since [`search_matches`] (sync in between) left out.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if a query fails.
pub fn search_hits(
    conn: &rusqlite::Connection,
    query: &str,
    matches: &[SearchMatch],
) -> Result<Vec<SearchHit>> {
    let Some(parsed) = crate::search::SearchQuery::parse(query) else {
        return Ok(Vec::new());
    };
    let mut read = conn.prepare_cached(
        "SELECT e.id, e.feed_id, e.status, e.starred, e.title, e.url, e.author, \
         e.published_at, e.reading_time, COALESCE(f.title, ''), \
         CASE WHEN ?2 THEN e.content END \
         FROM entries e LEFT JOIN feeds f ON f.id = e.feed_id WHERE e.id = ?1",
    )?;
    let mut hits = Vec::with_capacity(matches.len());
    for m in matches {
        let hit = read
            .query_row(
                rusqlite::params![m.id.get(), m.kind == MatchKind::Text],
                |r| {
                    Ok(SearchHit {
                        row: row_to_list_row(r)?,
                        kind: m.kind,
                        feed_title: r.get(9)?,
                        excerpt: r
                            .get::<_, Option<String>>(10)?
                            .and_then(|body| parsed.excerpt(&body)),
                    })
                },
            )
            .optional()?;
        hits.extend(hit);
    }
    Ok(hits)
}

/// Ids of every entry filter matches, nothing else.
///
/// Caller: "Mark all as read", acting on whole scope, needs only ids.
/// [`EntryListRow`]s would haul every title and URL; [`Entry`]s haul every BODY
/// (54 MB single allocation on 5,357-entry mirror; allocator never returns it).
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn entry_ids_matching(
    conn: &rusqlite::Connection,
    filter: EntryFilter,
) -> Result<Vec<EntryId>> {
    const UNREAD: &str = "SELECT id FROM entries WHERE status = 'unread' \
        AND feed_id NOT IN (SELECT id FROM feeds WHERE hide_globally = 1 \
        OR category_id IN (SELECT id FROM categories WHERE hide_globally = 1))";
    const STARRED: &str = "SELECT id FROM entries WHERE starred = 1";
    const ALL: &str = "SELECT id FROM entries \
        WHERE feed_id NOT IN (SELECT id FROM feeds WHERE hide_globally = 1 \
        OR category_id IN (SELECT id FROM categories WHERE hide_globally = 1))";
    const BY_FEED: &str = "SELECT id FROM entries WHERE feed_id = ?1";
    const BY_CATEGORY: &str =
        "SELECT id FROM entries WHERE feed_id IN (SELECT id FROM feeds WHERE category_id = ?1)";

    let mut out = Vec::new();
    match filter {
        EntryFilter::Unread | EntryFilter::Starred | EntryFilter::All => {
            let sql = match filter {
                EntryFilter::Unread => UNREAD,
                EntryFilter::Starred => STARRED,
                _ => ALL,
            };
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                out.push(EntryId(row.get(0)?));
            }
        }
        EntryFilter::Feed(id) | EntryFilter::Category(id) => {
            let sql = if matches!(filter, EntryFilter::Feed(_)) {
                BY_FEED
            } else {
                BY_CATEGORY
            };
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query([id])?;
            while let Some(row) = rows.next()? {
                out.push(EntryId(row.get(0)?));
            }
        }
    }
    Ok(out)
}

fn row_to_list_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EntryListRow> {
    Ok(EntryListRow {
        id: EntryId(r.get(0)?),
        feed_id: FeedId(r.get(1)?),
        status: if r.get::<_, String>(2)? == "read" {
            EntryStatus::Read
        } else {
            EntryStatus::Unread
        },
        starred: r.get::<_, i64>(3)? != 0,
        title: r.get(4)?,
        // Re-parsed like full `Entry` read path: string reaches QML, tap opens
        // it in browser, and older build's row may hold something current
        // policy rejects. Failing URL dropped.
        url: r
            .get::<_, Option<String>>(5)?
            .and_then(|s| crate::content::MediaUrl::parse(&s))
            .map(|u| u.as_str().to_owned()),
        author: r.get(6)?,
        published_at: r.get(7)?,
        reading_time: saturating_i32(r.get(8)?),
    })
}

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    let tags: String = r.get(13)?;
    Ok(Entry {
        id: EntryId(r.get(0)?),
        feed_id: FeedId(r.get(1)?),
        status: if r.get::<_, String>(2)? == "read" {
            EntryStatus::Read
        } else {
            EntryStatus::Unread
        },
        starred: r.get::<_, i64>(3)? != 0,
        title: r.get(4)?,
        url: r
            .get::<_, Option<String>>(5)?
            .and_then(|s| crate::content::MediaUrl::parse(&s)),
        comments_url: r
            .get::<_, Option<String>>(6)?
            .and_then(|s| crate::content::MediaUrl::parse(&s)),
        author: r.get(7)?,
        content: r.get(8)?,
        published_at: from_ts(r.get(9)?),
        created_at: from_ts(r.get(10)?),
        changed_at: from_ts(r.get(11)?),
        reading_time: saturating_i32(r.get(12)?),
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        enclosures: Vec::new(),
    })
}

/// One entry by id, body included; `None` if mirror lacks it.
///
/// `enclosures` always empty (not read). Stored URLs failing URL policy →
/// `None`; unreadable tags → none.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn entry(conn: &rusqlite::Connection, id: EntryId) -> Result<Option<Entry>> {
    let row = conn
        .query_row(
            "SELECT id, feed_id, status, starred, title, url, comments_url, author, content,
                    published_at, created_at, changed_at, reading_time, tags
             FROM entries WHERE id = ?1",
            [id.get()],
            row_to_entry,
        )
        .optional()?;
    Ok(row)
}

/// Unread tab count: cover number and new-articles notification total. Hidden
/// feeds left out as in [`EntryFilter::Unread`], so the two never disagree.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn unread_count(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM entries WHERE status = 'unread' \
         AND feed_id NOT IN (SELECT id FROM feeds WHERE hide_globally = 1 \
         OR category_id IN (SELECT id FROM categories WHERE hide_globally = 1))",
        [],
        |r| r.get(0),
    )?)
}

// --------------------------------------------------------------- arrivals

/// Articles arrived since reader last looked, still unread, newest first.
///
/// "Still unread" read when asked, not on arrival: article read elsewhere
/// between sync and notification isn't counted.
///
/// Hidden feeds not announced: notification sending reader to Unread tab
/// without them announces nothing. Stay flagged; opening Vuo clears them with
/// rest.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn arrivals(conn: &rusqlite::Connection) -> Result<Vec<EntryId>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM entries WHERE arrived = 1 AND status = 'unread' \
         AND feed_id NOT IN (SELECT id FROM feeds WHERE hide_globally = 1 \
         OR category_id IN (SELECT id FROM categories WHERE hide_globally = 1)) \
         ORDER BY published_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok(EntryId(r.get(0)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Stop treating these entries as new.
///
/// By id, only these: dismissed notification covered exactly them. Clearing all
/// would swallow what sync brought between notification and swipe.
///
/// Returns flags actually cleared.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if an update fails.
pub fn acknowledge_arrivals(tx: &Transaction<'_>, ids: &[EntryId]) -> Result<usize> {
    let mut stmt = tx.prepare("UPDATE entries SET arrived = 0 WHERE id = ?1 AND arrived = 1")?;
    let mut cleared = 0usize;
    for id in ids {
        cleared = cleared.saturating_add(stmt.execute([id.get()])?);
    }
    Ok(cleared)
}

/// Whether anything is marked arrived, read or not.
///
/// Asked before [`acknowledge_all_arrivals`] (Qt thread, every mirror change
/// while open). That write nearly always finds nothing, yet takes write lock
/// and waits behind worker. This is a read over partial index, empty then.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn any_arrivals(conn: &rusqlite::Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM entries WHERE arrived = 1)",
        [],
        |r| r.get(0),
    )?)
}

/// Stop treating anything as new: reader has Vuo open.
///
/// Returns flags cleared.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if update fails.
pub fn acknowledge_all_arrivals(tx: &Transaction<'_>) -> Result<usize> {
    Ok(tx.execute("UPDATE entries SET arrived = 0 WHERE arrived = 1", [])?)
}

// ------------------------------------------------------------------ icons

/// Store a downloaded icon, replacing any held under the same id.
///
/// # Errors
///
/// Returns [`Error::Db`](crate::error::Error::Db) if the write fails.
pub fn upsert_icon(tx: &Transaction<'_>, icon: &Icon) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO icons (id, format, bytes, width, height) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            icon.id.get(),
            icon.format.mime_type(),
            icon.bytes,
            icon.dimensions.map(|(w, _)| i64::from(w)),
            icon.dimensions.map(|(_, h)| i64::from(h)),
        ],
    )?;
    Ok(())
}

/// Record feed icon couldn't be fetched or decoded.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if update fails.
pub fn record_icon_failure(tx: &Transaction<'_>, feed_id: FeedId) -> Result<()> {
    tx.execute(
        "UPDATE feeds SET icon_failures = icon_failures + 1 WHERE id = ?1",
        [feed_id.get()],
    )?;
    Ok(())
}

/// Clear feed icon failure count, e.g. server reports new icon.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if update fails.
pub fn clear_icon_failures(tx: &Transaction<'_>, feed_id: FeedId) -> Result<()> {
    tx.execute(
        "UPDATE feeds SET icon_failures = 0 WHERE id = ?1",
        [feed_id.get()],
    )?;
    Ok(())
}

/// What list row shows about entry's feed.
///
/// One query for all feeds, not join onto entry list: 500 rows vs tens of
/// feeds; join would haul same icon blob hundreds of times.
#[derive(Debug, Clone)]
pub struct FeedChrome {
    /// The feed's server id.
    pub feed_id: i64,
    /// Feed name as user (or feed) set it. FOREIGN TEXT.
    pub title: String,
    /// Icon MIME type and bytes, when mirror has one.
    pub icon: Option<(String, Vec<u8>)>,
}

/// Feed names and icons, for decorating entry list.
///
/// Every feed, by id. Stored icon with no bytes = none.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn feed_chrome(conn: &rusqlite::Connection) -> Result<Vec<FeedChrome>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.title, i.format, i.bytes
           FROM feeds f
           LEFT JOIN icons i ON i.id = f.icon_id
          ORDER BY f.id",
    )?;
    let rows = stmt.query_map([], |r| {
        let mime: Option<String> = r.get(2)?;
        let bytes: Option<Vec<u8>> = r.get(3)?;
        Ok(FeedChrome {
            feed_id: r.get(0)?,
            title: r.get(1)?,
            icon: match (mime, bytes) {
                (Some(m), Some(b)) if !b.is_empty() => Some((m, b)),
                _ => None,
            },
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// One stored icon by id; `None` if not downloaded.
///
/// Unrecognised MIME type read as ICO; dimensions not fitting `u32` unknown.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn icon(conn: &rusqlite::Connection, id: IconId) -> Result<Option<Icon>> {
    let row = conn
        .query_row(
            "SELECT id, format, bytes, width, height FROM icons WHERE id = ?1",
            [id.get()],
            |r| {
                let mime: String = r.get(1)?;
                Ok(Icon {
                    id: IconId(r.get(0)?),
                    format: match mime.as_str() {
                        "image/png" => ImageFormat::Png,
                        "image/jpeg" => ImageFormat::Jpeg,
                        "image/gif" => ImageFormat::Gif,
                        "image/webp" => ImageFormat::WebP,
                        "image/bmp" => ImageFormat::Bmp,
                        _ => ImageFormat::Ico,
                    },
                    bytes: r.get(2)?,
                    dimensions: match (r.get::<_, Option<i64>>(3)?, r.get::<_, Option<i64>>(4)?) {
                        // Written from u32s; value no longer fitting → unknown,
                        // not wrapped.
                        (Some(w), Some(h)) => u32::try_from(w).ok().zip(u32::try_from(h).ok()),
                        _ => None,
                    },
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Feed ids whose icon isn't downloaded yet.
///
/// §11 thundering herd on first sync: icons fetched lazily from this list, few
/// at a time, not all behind first pull.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn feeds_missing_icons(
    conn: &rusqlite::Connection,
    limit: i64,
) -> Result<Vec<(FeedId, IconId)>> {
    // Ordered by failure count, give up after a few. Undecodable icon never
    // decodes; retrying every sync starves feeds behind it.
    let mut stmt = conn.prepare(
        "SELECT f.id, f.icon_id FROM feeds f
         WHERE f.icon_id IS NOT NULL
           AND f.icon_failures < 3
           AND NOT EXISTS (SELECT 1 FROM icons i WHERE i.id = f.icon_id)
         ORDER BY f.icon_failures ASC, f.id ASC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |r| Ok((FeedId(r.get(0)?), IconId(r.get(1)?))))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ------------------------------------------------------------- sync state

/// Sync engine bookkeeping: mirror's single `sync_state` row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    /// `changed_after` cursor next incremental pull starts from, Unix seconds;
    /// `None` before first pull.
    pub cursor_changed_after: Option<i64>,
    /// Last pass's generation; each pass stamps written rows with next one, in
    /// `last_seen_sync`.
    pub sync_generation: i64,
    /// Last full reconcile, Unix seconds.
    pub last_full_reconcile_at: Option<i64>,
    /// Server's deletion regime, label for diagnosis.
    pub server_era: Option<String>,
    /// Miniflux version server last reported.
    pub server_version: Option<String>,
    /// When `server_version` last asked, Unix seconds.
    ///
    /// `None` = never asked, or by build predating column: version of unknown
    /// age, next pass re-checks.
    pub server_version_checked_at: Option<i64>,
}

/// Read sync bookkeeping.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails, incl. missing
/// `sync_state` row (migrations always create it).
pub fn sync_state(conn: &rusqlite::Connection) -> Result<SyncState> {
    Ok(conn.query_row(
        "SELECT cursor_changed_after, sync_generation, last_full_reconcile_at, server_era,
                server_version, server_version_checked_at
         FROM sync_state WHERE id = 1",
        [],
        |r| {
            Ok(SyncState {
                cursor_changed_after: r.get(0)?,
                sync_generation: r.get(1)?,
                last_full_reconcile_at: r.get(2)?,
                server_era: r.get(3)?,
                server_version: r.get(4)?,
                server_version_checked_at: r.get(5)?,
            })
        },
    )?)
}

/// Overwrite sync bookkeeping with `state`, all fields.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if update fails.
pub fn set_sync_state(tx: &Transaction<'_>, state: &SyncState) -> Result<()> {
    tx.execute(
        "UPDATE sync_state SET cursor_changed_after = ?1, sync_generation = ?2,
             last_full_reconcile_at = ?3, server_era = ?4, server_version = ?5,
             server_version_checked_at = ?6
         WHERE id = 1",
        rusqlite::params![
            state.cursor_changed_after,
            state.sync_generation,
            state.last_full_reconcile_at,
            state.server_era,
            state.server_version,
            state.server_version_checked_at,
        ],
    )?;
    Ok(())
}

// --------------------------------------------------------- media consent

/// Record reader agreed to load un-proxied media from `origin` (§9.3) at `now`.
/// Granting again refreshes time.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if write fails.
pub fn grant_media_consent(tx: &Transaction<'_>, origin: &str, now: i64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO media_consent (origin, granted_at) VALUES (?1, ?2)",
        rusqlite::params![origin, now],
    )?;
    Ok(())
}

/// Every origin reader agreed to load un-proxied media from.
///
/// Stored origin no longer parsing as URL skipped.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if query fails.
pub fn media_consent(conn: &rusqlite::Connection) -> Result<Vec<url::Url>> {
    let mut stmt = conn.prepare("SELECT origin FROM media_consent")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        if let Ok(url) = url::Url::parse(&row?) {
            out.push(url);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! `store` = UI's whole read path. These tests pin unread filter and order,
    //! and category browsing: wrong `status`, wrong sort direction, or category
    //! routed through by-feed statement must fail here.

    use super::*;
    use crate::db::outbox;
    use crate::db::Database;
    use crate::model::{Category, Enclosure, EntryStatus};

    fn ts(secs: i64) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::from_timestamp(secs, 0)
    }

    fn entry_at(id: i64, feed: i64, status: EntryStatus, starred: bool, published: i64) -> Entry {
        Entry {
            id: EntryId(id),
            feed_id: FeedId(feed),
            status,
            starred,
            title: format!("entry {id}"),
            url: None,
            comments_url: None,
            author: String::new(),
            content: String::new(),
            published_at: ts(published),
            created_at: ts(published),
            changed_at: ts(published),
            reading_time: 1,
            tags: Vec::new(),
            enclosures: Vec::new(),
        }
    }

    fn feed_in(id: i64, category: Option<i64>) -> Feed {
        Feed {
            id: FeedId(id),
            category_id: category.map(CategoryId),
            title: format!("feed {id}"),
            site_url: None,
            feed_url: None,
            icon_id: None,
            checked_at: None,
            parsing_error_message: String::new(),
            parsing_error_count: 0,
            disabled: false,
            hide_globally: false,
            crawler: false,
        }
    }

    /// Two categories, three feeds, six entries with distinct publish times.
    fn populated() -> Database {
        let mut db = Database::open_in_memory().expect("mirror");
        db.with_tx(|tx| {
            for (id, title) in [(1i64, "News"), (2, "Code")] {
                upsert_category(
                    tx,
                    &Category {
                        id: CategoryId(id),
                        title: title.to_owned(),
                        hide_globally: false,
                    },
                    1,
                )?;
            }
            upsert_feed(tx, &feed_in(10, Some(1)), 1)?;
            upsert_feed(tx, &feed_in(11, Some(1)), 1)?;
            upsert_feed(tx, &feed_in(20, Some(2)), 1)?;

            // `published_at` ascending with id: "newest first" = id descending.
            upsert_entry(
                tx,
                &entry_at(1, 10, EntryStatus::Unread, false, 100),
                1,
                Arrival::Quiet,
            )?;
            upsert_entry(
                tx,
                &entry_at(2, 10, EntryStatus::Read, true, 200),
                1,
                Arrival::Quiet,
            )?;
            upsert_entry(
                tx,
                &entry_at(3, 11, EntryStatus::Unread, true, 300),
                1,
                Arrival::Quiet,
            )?;
            upsert_entry(
                tx,
                &entry_at(4, 11, EntryStatus::Read, false, 400),
                1,
                Arrival::Quiet,
            )?;
            upsert_entry(
                tx,
                &entry_at(5, 20, EntryStatus::Unread, false, 500),
                1,
                Arrival::Quiet,
            )?;
            upsert_entry(
                tx,
                &entry_at(6, 20, EntryStatus::Read, false, 600),
                1,
                Arrival::Quiet,
            )?;
            Ok(())
        })
        .expect("seed");
        db
    }

    fn ids(entries: &[EntryListRow]) -> Vec<i64> {
        entries.iter().map(|e| e.id.get()).collect()
    }

    fn ids_of(entries: &[EntryId]) -> Vec<i64> {
        entries.iter().map(|id| id.get()).collect()
    }

    /// §retention drops what the reader is done with, and nothing else.
    ///
    /// Only thing shrinking mirror besides server deletion, so what it REFUSES
    /// to touch is the interesting half.
    #[test]
    fn retention_keeps_the_unread_the_favourites_and_the_unsent() {
        let mut db = populated();

        // Queued intent on entry 6, else oldest thing wide cutoff takes: `read`
        // not at server yet; deleting row drops reader's tap.
        db.with_tx(|tx| {
            outbox::queue(
                tx,
                EntryId(6),
                DesiredValue::Status(EntryStatus::Read),
                1_000,
            )
        })
        .expect("queue");

        // Past every fixture entry: only exclusions decide.
        let pruned = db.with_tx(|tx| prune_entries(tx, 10_000)).expect("prune");
        assert_eq!(pruned, 1, "only entry 4 is read, unfavourited and settled");

        let mut left = ids_of(&local_entry_ids(db.conn()).expect("ids"));
        left.sort_unstable();
        assert_eq!(
            left,
            vec![1, 2, 3, 5, 6],
            "unread (1, 3, 5), favourited (2) and unflushed (6) all survive"
        );
    }

    /// §retention is a window, not a purge.
    #[test]
    fn retention_only_reaches_past_the_cutoff() {
        let mut db = populated();

        // Entry 4 at 400, entry 6 at 600, both read and unfavourited. Cutoff
        // between takes exactly one.
        let pruned = db.with_tx(|tx| prune_entries(tx, 500)).expect("prune");
        assert_eq!(pruned, 1);
        let mut left = ids_of(&local_entry_ids(db.conn()).expect("ids"));
        left.sort_unstable();
        assert_eq!(
            left,
            vec![1, 2, 3, 5, 6],
            "the article inside the window stays"
        );

        // Nothing when window covers everything.
        let pruned = db.with_tx(|tx| prune_entries(tx, 0)).expect("prune");
        assert_eq!(pruned, 0);
    }

    /// §an enclosure does not outlive the entry retention dropped.
    ///
    /// FK `ON DELETE CASCADE` with `foreign_keys` ON: pragma load-bearing
    /// (other side: `db::tests::foreign_keys_are_enforced`).
    #[test]
    fn retention_takes_the_enclosures_with_it() {
        let mut db = populated();
        db.with_tx(|tx| {
            let mut e = entry_at(4, 11, EntryStatus::Read, false, 400);
            e.enclosures = vec![Enclosure {
                id: 99,
                entry_id: EntryId(4),
                url: crate::content::MediaUrl::parse("https://example.test/a.mp3"),
                mime_type: "audio/mpeg".to_owned(),
                size: 1,
            }];
            upsert_entry(tx, &e, 2, Arrival::Quiet)
        })
        .expect("enclosure");

        let count = |db: &Database| -> i64 {
            db.conn()
                .query_row("SELECT COUNT(*) FROM enclosures", [], |r| r.get(0))
                .expect("count")
        };
        assert_eq!(count(&db), 1, "the fixture must have one to lose");

        db.with_tx(|tx| prune_entries(tx, 10_000)).expect("prune");
        assert_eq!(count(&db), 0, "the enclosure went with its entry");
    }

    #[test]
    fn each_filter_selects_its_own_rows_newest_first() {
        let db = populated();
        let list = |f| list_entries(db.conn(), f, 100, 0).expect("list");

        assert_eq!(
            ids(&list(EntryFilter::Unread)),
            vec![5, 3, 1],
            "unread only, newest first"
        );
        assert_eq!(ids(&list(EntryFilter::Starred)), vec![3, 2], "starred only");
        assert_eq!(
            ids(&list(EntryFilter::All)),
            vec![6, 5, 4, 3, 2, 1],
            "everything, newest first"
        );
        assert_eq!(ids(&list(EntryFilter::Feed(11))), vec![4, 3], "one feed");
        assert_eq!(
            ids(&list(EntryFilter::Category(1))),
            vec![4, 3, 2, 1],
            "a category spans its feeds. Routing this through the by-feed \
             statement -- so it matched feed_id 1, which does not exist -- \
             returned an empty list, and nothing noticed."
        );
        assert_eq!(ids(&list(EntryFilter::Category(2))), vec![6, 5]);
    }

    /// §`hide_globally`: a hidden feed leaves Unread and All, and nothing else.
    ///
    /// Switch must keep feed out of Unread and All on this phone. Miniflux
    /// hides on either flag (feed's or category's): both exercised.
    #[test]
    fn hidden_feeds_leave_unread_and_all_but_nothing_else() {
        let mut db = populated();
        db.with_tx(|tx| {
            // Feed 11 by own flag; feed 20 by category's.
            let mut hidden = feed_in(11, Some(1));
            hidden.hide_globally = true;
            upsert_feed(tx, &hidden, 2)?;
            upsert_category(
                tx,
                &Category {
                    id: CategoryId(2),
                    title: "Code".to_owned(),
                    hide_globally: true,
                },
                2,
            )?;
            // Feed mirror doesn't hold yet isn't hidden.
            upsert_entry(
                tx,
                &entry_at(7, 99, EntryStatus::Unread, false, 700),
                2,
                Arrival::Announce,
            )?;
            upsert_entry(
                tx,
                &entry_at(8, 11, EntryStatus::Unread, false, 800),
                2,
                Arrival::Announce,
            )
        })
        .expect("hide");
        let list = |f| ids(&list_entries(db.conn(), f, 100, 0).expect("list"));
        let matching = |f| {
            let mut got = ids_of(&entry_ids_matching(db.conn(), f).expect("ids"));
            got.sort_unstable();
            got
        };

        assert_eq!(list(EntryFilter::Unread), vec![7, 1]);
        assert_eq!(list(EntryFilter::All), vec![7, 2, 1]);
        assert_eq!(
            matching(EntryFilter::Unread),
            vec![1, 7],
            "mark all as read on Unread must not reach what Unread does not show"
        );
        assert_eq!(matching(EntryFilter::All), vec![1, 2, 7]);
        assert_eq!(
            unread_count(db.conn()).expect("count"),
            2,
            "the cover's number is the Unread tab's"
        );
        assert_eq!(
            ids_of(&arrivals(db.conn()).expect("arrivals")),
            vec![7],
            "a notification announces only what Unread will show"
        );

        assert_eq!(list(EntryFilter::Starred), vec![3, 2], "starred keeps them");
        assert_eq!(
            list(EntryFilter::Feed(11)),
            vec![8, 4, 3],
            "so does the feed"
        );
        assert_eq!(
            list(EntryFilter::Category(2)),
            vec![6, 5],
            "and the category"
        );
        assert_eq!(
            unread_counts_by_feed(db.conn()).expect("per feed").get(&11),
            Some(&2),
            "the feed list's badge counts the feed's own unread"
        );
    }

    #[test]
    fn limit_and_offset_page_without_skipping_or_repeating() {
        let db = populated();
        let page = |limit, offset| {
            ids(&list_entries(db.conn(), EntryFilter::All, limit, offset).expect("list"))
        };
        assert_eq!(page(2, 0), vec![6, 5]);
        assert_eq!(page(2, 2), vec![4, 3]);
        assert_eq!(page(2, 4), vec![2, 1]);
        assert_eq!(page(2, 6), Vec::<i64>::new());
    }

    /// §search finds articles by what the reader saw, grouped by where it saw
    /// it, newest first within each group.
    // One fixture, five articles a to e, each landing in different group.
    #[allow(clippy::many_single_char_names)]
    #[test]
    fn search_groups_by_where_it_matched_newest_first() {
        let mut db = populated();
        db.with_tx(|tx| {
            let mut a = entry_at(7, 10, EntryStatus::Read, false, 700);
            a.title = "Harbour at dusk".to_owned();
            a.content = "<p>A quiet evening.</p>".to_owned();
            upsert_entry(tx, &a, 1, Arrival::Quiet)?;
            let mut b = entry_at(8, 20, EntryStatus::Unread, false, 800);
            b.content =
                r#"<p>A <a href="https://x.example/">walk</a> by the harbour.</p>"#.to_owned();
            upsert_entry(tx, &b, 1, Arrival::Quiet)?;
            let mut c = entry_at(9, 11, EntryStatus::Unread, false, 50);
            c.content = r#"<div class="harbour">nothing to see</div>"#.to_owned();
            upsert_entry(tx, &c, 1, Arrival::Quiet)?;
            // Hidden from Unread and All, not from search.
            let mut hidden = feed_in(30, None);
            hidden.hide_globally = true;
            upsert_feed(tx, &hidden, 1)?;
            let mut d = entry_at(10, 30, EntryStatus::Unread, false, 900);
            d.title = "Hidden HARBOUR".to_owned();
            upsert_entry(tx, &d, 1, Arrival::Quiet)?;
            // Newest of all, term in feed name: finds feed, not article.
            let mut port = feed_in(40, None);
            port.title = "Harbour Gazette".to_owned();
            upsert_feed(tx, &port, 1)?;
            let e = entry_at(11, 40, EntryStatus::Unread, false, 1000);
            upsert_entry(tx, &e, 1, Arrival::Quiet)?;
            // Read: not in feed's unread count.
            upsert_entry(
                tx,
                &entry_at(12, 40, EntryStatus::Read, false, 10),
                1,
                Arrival::Quiet,
            )?;
            let mut times = feed_in(41, None);
            times.title = "harbour times".to_owned();
            upsert_feed(tx, &times, 1)
        })
        .expect("seed");
        let search = |q: &str| search_matches(db.conn(), q).expect("search");
        let ids = |r: &[SearchMatch]| r.iter().map(|m| m.id.get()).collect::<Vec<_>>();

        let found = search("harbour");
        assert_eq!(
            ids(&found),
            vec![10, 7, 8],
            "titles, then body text, any case, newest first within each -- and \
             markup does not match, nor does the feed's name"
        );
        assert_eq!(
            found.iter().map(|m| m.kind).collect::<Vec<_>>(),
            vec![MatchKind::Title, MatchKind::Title, MatchKind::Text]
        );

        // Feeds found by name, feed-list order.
        let feeds = search_feeds(db.conn(), "HARBOUR").expect("feeds");
        assert_eq!(
            feeds
                .iter()
                .map(|f| (f.id.get(), f.title.as_str(), f.unread))
                .collect::<Vec<_>>(),
            vec![(40, "Harbour Gazette", 1), (41, "harbour times", 0)],
            "by name, any case, with each feed's unread count"
        );
        assert_eq!(
            search_feeds(db.conn(), "harbour gaz")
                .expect("feeds")
                .iter()
                .map(|f| f.id.get())
                .collect::<Vec<_>>(),
            vec![40],
            "every term must be in the name"
        );
        assert_eq!(search_feeds(db.conn(), " ").expect("feeds"), Vec::new());
        assert_eq!(
            SearchCounts::of(&found, &feeds),
            SearchCounts {
                title: 2,
                feed: 2,
                text: 1
            }
        );
        let hits = search_hits(db.conn(), "harbour", &found).expect("hits");
        assert_eq!(
            hits.iter().map(|h| h.row.id.get()).collect::<Vec<_>>(),
            ids(&found),
            "the rows come back in the order they were asked for"
        );
        let by_id = |id: i64| hits.iter().find(|h| h.row.id.get() == id).expect("hit");
        assert_eq!(by_id(10).feed_title, "feed 30");
        assert_eq!(
            by_id(8).excerpt.as_deref(),
            Some("A walk by the harbour."),
            "a body hit says what the body says around the match"
        );
        assert_eq!(
            by_id(7).excerpt,
            None,
            "a title hit shows its match in the title; its body is not read"
        );

        assert_eq!(
            ids(&search("harbour walk")),
            vec![8],
            "every term must match"
        );
        assert_eq!(
            ids(&search("harbour evening")),
            vec![7],
            "terms may match in different places, and the title decides the group"
        );
        assert_eq!(
            ids(&search("href")),
            Vec::<i64>::new(),
            "an attribute is not text"
        );
        assert_eq!(
            ids(&search("feed 20")),
            Vec::<i64>::new(),
            "the feed's name does not find its articles"
        );
        assert_eq!(search("   "), Vec::new(), "no terms, no results");

        // Only requested rows read (what list shows per group); article gone
        // since search left out.
        let shown = [found[1], found[2]];
        let hits = search_hits(db.conn(), "harbour", &shown).expect("hits");
        assert_eq!(
            hits.iter().map(|h| h.row.id.get()).collect::<Vec<_>>(),
            vec![7, 8]
        );
        db.with_tx(|tx| {
            tx.execute("DELETE FROM entries WHERE id = 7", [])?;
            Ok(())
        })
        .expect("delete");
        let hits = search_hits(db.conn(), "harbour", &shown).expect("hits");
        assert_eq!(
            hits.iter().map(|h| h.row.id.get()).collect::<Vec<_>>(),
            vec![8]
        );
        assert_eq!(
            search_hits(db.conn(), " ", &shown).expect("hits"),
            Vec::new()
        );
    }

    /// LIKE wildcards not ours: `%` is percent sign.
    #[test]
    fn search_terms_are_literal() {
        let mut db = populated();
        db.with_tx(|tx| {
            let mut a = entry_at(7, 10, EntryStatus::Unread, false, 700);
            a.title = "Up 50% on the year".to_owned();
            upsert_entry(tx, &a, 1, Arrival::Quiet)
        })
        .expect("seed");
        let search = |q: &str| {
            search_matches(db.conn(), q)
                .expect("search")
                .iter()
                .map(|m| m.id.get())
                .collect::<Vec<_>>()
        };
        assert_eq!(search("50%"), vec![7]);
        assert_eq!(search("%"), vec![7]);
        assert_eq!(search("_"), Vec::<i64>::new());
    }

    #[test]
    fn a_pending_intent_wins_per_field_in_both_directions() {
        // §8.3: server change to locally mutated entry resolves by stated rule:
        // local intent wins PER FIELD.
        //
        // Both directions. Common case: mark read offline, pull echoes entry
        // back unread; mark must not vanish while outbox holds it.
        let mut db = populated();

        // Pending STATUS, remote changes STARRED.
        db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 1))
            .expect("queue status");
        db.with_tx(|tx| {
            let mut remote = entry_at(1, 10, EntryStatus::Unread, true, 100);
            remote.title = "remote title".to_owned();
            upsert_entry(tx, &remote, 2, Arrival::Quiet)
        })
        .expect("pull");
        let e = entry(db.conn(), EntryId(1))
            .expect("read")
            .expect("present");
        assert_eq!(
            e.status,
            EntryStatus::Read,
            "the pending status must win over the server's"
        );
        assert!(e.starred, "and the field with no intent takes the server's");
        assert_eq!(e.title, "remote title", "as does everything else");

        // Pending STARRED, remote changes STATUS. Mirror case.
        db.with_tx(|tx| outbox::queue(tx, EntryId(5), DesiredValue::Starred(true), 1))
            .expect("queue star");
        db.with_tx(|tx| {
            upsert_entry(
                tx,
                &entry_at(5, 20, EntryStatus::Read, false, 500),
                2,
                Arrival::Quiet,
            )
        })
        .expect("pull");
        let e = entry(db.conn(), EntryId(5))
            .expect("read")
            .expect("present");
        assert!(e.starred, "the pending star must win");
        assert_eq!(
            e.status,
            EntryStatus::Read,
            "and the field with no intent takes the server's"
        );
    }

    #[test]
    fn media_consent_round_trips_per_origin() {
        // §9.3 consent store: agreeing to one host isn't agreeing to all.
        let mut db = populated();
        assert_eq!(media_consent(db.conn()).expect("read"), Vec::new());

        db.with_tx(|tx| {
            grant_media_consent(tx, "https://images.example", 1)?;
            grant_media_consent(tx, "https://cdn.example", 2)
        })
        .expect("grant");

        let mut got: Vec<String> = media_consent(db.conn())
            .expect("read")
            .iter()
            .map(url::Url::to_string)
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                "https://cdn.example/".to_owned(),
                "https://images.example/".to_owned()
            ]
        );

        // Same origin granted twice: no duplicate.
        db.with_tx(|tx| grant_media_consent(tx, "https://cdn.example", 3))
            .expect("regrant");
        assert_eq!(media_consent(db.conn()).expect("read").len(), 2);
    }

    /// §"Mark all as read" acts on the scope, not on the page a list shows.
    ///
    /// Must return EVERY id in scope, not first page, without building full
    /// entries.
    #[test]
    fn the_ids_in_a_scope_are_all_of_them() {
        let db = populated();
        let mut got = entry_ids_matching(db.conn(), EntryFilter::Unread).expect("unread ids");
        got.sort_unstable();
        assert_eq!(ids_of(&got), vec![1, 3, 5]);

        let mut got = entry_ids_matching(db.conn(), EntryFilter::Starred).expect("starred ids");
        got.sort_unstable();
        assert_eq!(ids_of(&got), vec![2, 3]);

        let mut got = entry_ids_matching(db.conn(), EntryFilter::All).expect("all ids");
        got.sort_unstable();
        assert_eq!(ids_of(&got), vec![1, 2, 3, 4, 5, 6]);

        let mut got = entry_ids_matching(db.conn(), EntryFilter::Feed(11)).expect("feed ids");
        got.sort_unstable();
        assert_eq!(ids_of(&got), vec![3, 4]);

        let mut got = entry_ids_matching(db.conn(), EntryFilter::Category(1)).expect("cat ids");
        got.sort_unstable();
        assert_eq!(
            ids_of(&got),
            vec![1, 2, 3, 4],
            "a category spans its feeds, as the listing does"
        );
    }

    /// §a list reads what a list draws, and the body is not part of that.
    ///
    /// Body is why [`EntryListRow`] exists. No `content` field is
    /// compile-checked; this pins other half: columns a list DOES need survived
    /// narrowing, incl. ones older builds spelled differently.
    #[test]
    fn a_list_row_carries_what_a_list_draws() {
        let mut db = Database::open_in_memory().expect("mirror");
        db.with_tx(|tx| {
            upsert_feed(tx, &feed_in(10, None), 1)?;
            let mut e = entry_at(1, 10, EntryStatus::Unread, true, 4_200);
            e.title = "Ein Titel".to_owned();
            e.author = "Eine Autorin".to_owned();
            e.reading_time = 7;
            e.url = crate::content::MediaUrl::parse("https://example.test/a");
            // Body far bigger than row should carry.
            e.content = "x".repeat(200_000);
            upsert_entry(tx, &e, 1, Arrival::Quiet)
        })
        .expect("seed");

        let rows = list_entries(db.conn(), EntryFilter::All, 10, 0).expect("list");
        let row = rows.first().expect("one row");
        assert_eq!(row.id, EntryId(1));
        assert_eq!(row.feed_id, FeedId(10));
        assert_eq!(row.status, EntryStatus::Unread);
        assert!(row.starred);
        assert_eq!(row.title, "Ein Titel");
        assert_eq!(row.author, "Eine Autorin");
        assert_eq!(row.published_at, Some(4_200));
        assert_eq!(row.reading_time, 7);
        assert_eq!(row.url.as_deref(), Some("https://example.test/a"));

        // Same row by id: path list takes for article reader finished but list
        // still holds.
        let one = entry_list_row(db.conn(), EntryId(1))
            .expect("read")
            .expect("the row is there");
        assert_eq!(&one, row, "both reads must agree, or a kept row redraws");
        assert!(
            entry_list_row(db.conn(), EntryId(404))
                .expect("read")
                .is_none(),
            "an id the mirror does not have is None, not an error"
        );
    }

    #[test]
    fn deleting_a_feed_takes_its_entries_with_it() {
        let mut db = populated();
        db.with_tx(|tx| delete_feed(tx, FeedId(10)))
            .expect("delete");

        assert_eq!(
            ids(&list_entries(db.conn(), EntryFilter::All, 100, 0).expect("list")),
            vec![6, 5, 4, 3],
            "the feed's entries go with it, and no others"
        );
        assert!(feeds(db.conn())
            .expect("feeds")
            .iter()
            .all(|f| f.id != FeedId(10)));
    }

    #[test]
    fn unread_counts_are_reported_per_feed_and_in_total() {
        let db = populated();
        assert_eq!(unread_count(db.conn()).expect("count"), 3);

        let per_feed = unread_counts_by_feed(db.conn()).expect("per feed");
        assert_eq!(per_feed.get(&10), Some(&1));
        assert_eq!(per_feed.get(&11), Some(&1));
        assert_eq!(per_feed.get(&20), Some(&1));

        let totals = entry_counts_by_feed(db.conn()).expect("totals");
        assert_eq!(totals.get(&10), Some(&2));
        assert_eq!(totals.get(&11), Some(&2));
        assert_eq!(totals.get(&20), Some(&2));
    }

    #[test]
    fn enclosures_are_replaced_rather_than_accumulated() {
        // Pull upserts same entry every pass it appears. INSERT without
        // clearing first would grow table unbounded.
        let mut db = populated();
        let with_two = |n: usize| {
            let mut e = entry_at(1, 10, EntryStatus::Unread, false, 100);
            e.enclosures = (0..n)
                .map(|i| Enclosure {
                    id: i64::try_from(i).unwrap_or(0),
                    entry_id: EntryId(1),
                    url: None,
                    mime_type: "audio/mpeg".to_owned(),
                    size: 1,
                })
                .collect();
            e
        };
        let count = |db: &Database| -> i64 {
            db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM enclosures WHERE entry_id = 1",
                    [],
                    |r| r.get(0),
                )
                .expect("count")
        };

        db.with_tx(|tx| upsert_entry(tx, &with_two(3), 2, Arrival::Quiet))
            .expect("first");
        assert_eq!(count(&db), 3);
        db.with_tx(|tx| upsert_entry(tx, &with_two(1), 3, Arrival::Quiet))
            .expect("second");
        assert_eq!(
            count(&db),
            1,
            "the previous set must be replaced, not added to"
        );
    }
    /// §what a notification reads: unread arrivals, newest first.
    ///
    /// Read when asked: arrival read since (here or elsewhere via sync) not
    /// counted; reader not told about something already read.
    #[test]
    fn arrivals_are_the_unread_ones_newest_first_with_a_few_titles() {
        let mut db = Database::open_in_memory().expect("mirror");
        db.with_tx(|tx| {
            upsert_feed(tx, &feed_in(10, None), 1)?;
            for (id, published) in [(1, 100), (2, 300), (3, 200), (4, 400)] {
                upsert_entry(
                    tx,
                    &entry_at(id, 10, EntryStatus::Unread, false, published),
                    1,
                    Arrival::Announce,
                )?;
            }
            // Held before reader last looked: not news.
            upsert_entry(
                tx,
                &entry_at(5, 10, EntryStatus::Unread, false, 500),
                1,
                Arrival::Quiet,
            )
        })
        .expect("seed");

        // 4 read after arriving, as sync from another device would.
        db.with_tx(|tx| {
            upsert_entry(
                tx,
                &entry_at(4, 10, EntryStatus::Read, false, 400),
                2,
                Arrival::Announce,
            )
        })
        .expect("read elsewhere");

        assert_eq!(
            ids_of(&arrivals(db.conn()).expect("arrivals")),
            vec![2, 3, 1],
            "unread arrivals, newest first"
        );
    }

    /// §a dismissal acknowledges what it covered, and nothing that came after.
    #[test]
    fn acknowledging_clears_exactly_the_ids_given_or_everything() {
        let mut db = Database::open_in_memory().expect("mirror");
        db.with_tx(|tx| {
            upsert_feed(tx, &feed_in(10, None), 1)?;
            for id in 1..=4 {
                upsert_entry(
                    tx,
                    &entry_at(id, 10, EntryStatus::Unread, false, id * 100),
                    1,
                    Arrival::Announce,
                )?;
            }
            Ok(())
        })
        .expect("seed");

        // 3 arrived after notification went up: not in dismissal's list. 99 not
        // in mirror at all.
        let cleared = db
            .with_tx(|tx| acknowledge_arrivals(tx, &[EntryId(1), EntryId(2), EntryId(99)]))
            .expect("acknowledge");
        assert_eq!(cleared, 2);
        assert_eq!(ids_of(&arrivals(db.conn()).expect("arrivals")), vec![4, 3]);

        assert!(any_arrivals(db.conn()).expect("any"));
        let cleared = db
            .with_tx(acknowledge_all_arrivals)
            .expect("acknowledge all");
        assert_eq!(cleared, 2);
        assert_eq!(arrivals(db.conn()).expect("arrivals"), Vec::new());
        assert!(!any_arrivals(db.conn()).expect("any"));
    }
}

#[cfg(test)]
mod name_sort_tests {
    use super::*;

    /// §the feed list is sorted the way a reader expects.
    ///
    /// Device: uppercase entries first, then lowercase: `ORDER BY title` byte
    /// comparison.
    #[test]
    fn names_sort_by_letter_not_by_byte() {
        let mut names = vec![
            "heise online",
            "Zeit Online",
            "Ärzteblatt",
            "tagesschau",
            "Der Standard",
            "ÖRF",
        ];
        names.sort_by_key(|n| name_sort_key(n));
        assert_eq!(
            names,
            vec![
                "Ärzteblatt",
                "Der Standard",
                "heise online",
                "ÖRF",
                "tagesschau",
                "Zeit Online",
            ],
            "case must not decide the order, and an umlaut sorts with its base \
             letter rather than after Z"
        );
    }

    #[test]
    fn eszett_sorts_as_ss() {
        assert_eq!(name_sort_key("Straße"), "strasse");
        assert_eq!(name_sort_key("STRASSE"), "strasse");
    }

    /// Every char `name_sort_key` folds, and ones it must not.
    ///
    /// e/i/o/u arms are RANGES (`'è'..='ë'` etc.): correct only while each
    /// covers exactly intended chars. One codepoint too wide swallows a
    /// neighbour: `ö` (U+00F6) one past `'ò'..='õ'`, `ü` (U+00FC) one past
    /// `'ù'..='û'`, both fold differently.
    ///
    /// So asserts gaps too: `ð` (U+00F0) between i- and o-range untouched; `æ`
    /// (U+00E6) below `ç`; `ý` (U+00FD) above `ü`.
    #[test]
    fn accents_fold_to_their_base_letter_and_stop_where_they_should() {
        for (accents, base) in [
            (['á', 'à', 'â', 'å', 'ã'].as_slice(), "a"),
            (['è', 'é', 'ê', 'ë'].as_slice(), "e"),
            (['ì', 'í', 'î', 'ï'].as_slice(), "i"),
            (['ò', 'ó', 'ô', 'õ'].as_slice(), "o"),
            (['ù', 'ú', 'û'].as_slice(), "u"),
            (['ç'].as_slice(), "c"),
            (['ñ'].as_slice(), "n"),
        ] {
            for c in accents {
                assert_eq!(
                    name_sort_key(&c.to_string()),
                    base,
                    "{c} should fold to {base}"
                );
            }
        }

        // German three keep DIN 5007-1 answers: no range may reach them.
        assert_eq!(name_sort_key("ä"), "a");
        assert_eq!(name_sort_key("ö"), "o");
        assert_eq!(name_sort_key("ü"), "u");
        assert_eq!(name_sort_key("ß"), "ss");

        // Gaps: nothing folds what no arm names.
        for untouched in ['æ', 'ð', 'ý', 'þ'] {
            assert_eq!(
                name_sort_key(&untouched.to_string()),
                untouched.to_string(),
                "{untouched} is not folded by any arm and must come through"
            );
        }
    }
}
