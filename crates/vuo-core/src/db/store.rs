//! Reads and writes against the mirror.
//!
//! The one rule worth stating up front is the conflict rule, because it is the
//! only place where "the server said X" and "the user did Y" have to be
//! reconciled and getting it wrong silently discards user actions.
//!
//! **Conflict resolution is per field, never per entry.** When a pulled entry
//! carries a field that also has a pending outbox intent, the local intent
//! wins for *that field only* and the server's value is taken for every other
//! field. Resolving per entry would mean a remote read-status change clobbers a
//! local pending star, or vice versa — the user marks something starred
//! offline, an unrelated read-status sync arrives, and the star vanishes.
//!
//! This matters more than it looks because Vuo sees its *own* writes come back:
//! every mutation bumps the server's `changed_at`, so a replayed outbox row
//! echoes into the next incremental pull. The rule above makes that echo a
//! no-op instead of a race.

use rusqlite::{OptionalExtension as _, Transaction};

use crate::db::outbox::{self, DesiredValue, OutboxField};
use crate::error::Result;
use crate::model::{
    Category, CategoryId, Entry, EntryId, EntryStatus, Feed, FeedId, Icon, IconId, ImageFormat,
};

fn ts(t: Option<chrono::DateTime<chrono::Utc>>) -> Option<i64> {
    t.map(|t| t.timestamp())
}

fn from_ts(secs: Option<i64>) -> Option<chrono::DateTime<chrono::Utc>> {
    secs.and_then(|s| chrono::DateTime::from_timestamp(s, 0))
}

/// A sort key for a user-visible name.
///
/// `ORDER BY title` in SQLite is a BYTE comparison, so it lists every
/// upper-case name before every lower-case one -- "Zeit" before "heise" --
/// which is what a device report called "sorting is broken". `COLLATE NOCASE`
/// only folds ASCII, so it fixes the case half and still files "Ärzteblatt"
/// after "Zeit".
///
/// This folds case and then the German umlauts the way DIN 5007-1 does, which
/// is what a German-language reader expects and is no worse than byte order
/// for anyone else. Deliberately not a full Unicode collation: that means a
/// large table and a dependency, for a list of a few dozen feed names.
#[must_use]
pub fn name_sort_key(title: &str) -> String {
    let mut key = String::with_capacity(title.len());
    for c in title.to_lowercase().chars() {
        match c {
            'ä' => key.push('a'),
            'ö' => key.push('o'),
            'ü' => key.push('u'),
            // `to_lowercase` already turns 'ß' into itself, not "ss".
            'ß' => key.push_str("ss"),
            'á' | 'à' | 'â' | 'å' | 'ã' => key.push('a'),
            'é' | 'è' | 'ê' | 'ë' => key.push('e'),
            'í' | 'ì' | 'î' | 'ï' => key.push('i'),
            'ó' | 'ò' | 'ô' | 'õ' => key.push('o'),
            'ú' | 'ù' | 'û' => key.push('u'),
            'ç' => key.push('c'),
            'ñ' => key.push('n'),
            other => key.push(other),
        }
    }
    key
}

// ------------------------------------------------------------- categories

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
    // `sort_by_cached_key`, not `sort_by`: the key is a fresh String, so
    // comparing with it would rebuild both sides on every comparison.
    out.sort_by_cached_key(|c| name_sort_key(&c.title));
    Ok(out)
}

// ------------------------------------------------------------------ feeds

pub fn upsert_feed(tx: &Transaction<'_>, f: &Feed, generation: i64) -> Result<()> {
    // `feeds.category_id` references `categories(id)`, and the two listings
    // are fetched separately: a category created between the two calls is
    // referenced by a feed but absent from the categories table, and the
    // foreign key would abort the entire sync over one row. Insert a
    // placeholder instead; the next taxonomy pull replaces it with the real
    // title.
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
            f.category_id.map(|c| c.get()),
            f.title,
            f.site_url.as_ref().map(|u| u.as_str()),
            f.feed_url.as_ref().map(|u| u.as_str()),
            f.icon_id.map(|i| i.get()),
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
            parsing_error_count: r.get::<_, i64>(8)? as i32,
            disabled: r.get::<_, i64>(9)? != 0,
            hide_globally: r.get::<_, i64>(10)? != 0,
            crawler: r.get::<_, i64>(11)? != 0,
        })
    })?;
    let mut out = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    // Sorted here rather than in SQL; see `name_sort_key`. Cached, because the
    // key is a fresh String and a plain `sort_by` would rebuild both sides on
    // every comparison.
    out.sort_by_cached_key(|f| name_sort_key(&f.title));
    Ok(out)
}

/// Remove a feed, its entries, and any intents queued against them.
pub fn delete_feed(tx: &Transaction<'_>, id: FeedId) -> Result<()> {
    // The outbox rows go first, and they are not optional: the same rule
    // `delete_entry` states applies here. An intent for an entry the server no
    // longer has can never be confirmed -- Miniflux silently ignores unknown
    // ids, so the flush gets its 204 and `confirm` matches nothing -- and the
    // row would sit in the queue forever, counted in "changes waiting to be
    // sent" that will never be sent.
    tx.execute(
        "DELETE FROM outbox WHERE entry_id IN (SELECT id FROM entries WHERE feed_id = ?1)",
        [id.get()],
    )?;
    // Entries do not declare an FK to feeds (a feed can arrive before or after
    // its entries during a sync), so they are removed explicitly.
    tx.execute("DELETE FROM entries WHERE feed_id = ?1", [id.get()])?;
    tx.execute("DELETE FROM feeds WHERE id = ?1", [id.get()])?;
    Ok(())
}

// ---------------------------------------------------------------- entries

/// Insert or update an entry, honouring pending local intents.
///
/// See the module docs: the conflict rule is per field. A field with a pending
/// outbox row keeps its local value; every other field takes the server's.
pub fn upsert_entry(tx: &Transaction<'_>, e: &Entry, generation: i64) -> Result<()> {
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

    tx.execute(
        "INSERT INTO entries (id, feed_id, status, starred, title, url, comments_url, author,
                              content, published_at, created_at, changed_at, reading_time, tags,
                              last_seen_sync)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
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
            e.url.as_ref().map(|u| u.as_str()),
            e.comments_url.as_ref().map(|u| u.as_str()),
            e.author,
            e.content,
            ts(e.published_at),
            ts(e.created_at),
            ts(e.changed_at),
            i64::from(e.reading_time),
            tags,
            generation,
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
                enc.url.as_ref().map(|u| u.as_str()),
                enc.mime_type,
                enc.size
            ],
        )?;
    }
    Ok(())
}

/// Delete an entry the server no longer has.
///
/// Any pending outbox intent for it goes too: replaying a mutation against an
/// id the server has deleted is pointless, and Miniflux silently ignores
/// unknown ids anyway, so the row would never clear.
pub fn delete_entry(tx: &Transaction<'_>, id: EntryId) -> Result<()> {
    tx.execute("DELETE FROM outbox WHERE entry_id = ?1", [id.get()])?;
    tx.execute("DELETE FROM entries WHERE id = ?1", [id.get()])?;
    Ok(())
}

/// Every entry id Vuo holds locally.
pub fn local_entry_ids(conn: &rusqlite::Connection) -> Result<Vec<EntryId>> {
    let mut stmt = conn.prepare("SELECT id FROM entries")?;
    let rows = stmt.query_map([], |r| Ok(EntryId(r.get(0)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Per-feed entry counts, for the cheap divergence check against
/// `GET /v1/feeds/counters`.
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

/// How the UI wants entries listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryFilter {
    Unread,
    Starred,
    All,
    Feed(i64),
    Category(i64),
}

/// List entries for the UI, newest first.
///
/// Each filter is its own complete literal statement. Sharing a column list
/// through `format!` would be harmless here -- the interpolated text is a
/// constant -- but §9.4 is absolute ("no query built with `format!`, anywhere,
/// ever"), and a rule with one blessed exception is a rule that grows more.
/// The repetition is the price of the guarantee being checkable, and
/// `db::tests::no_sql_in_this_crate_is_built_by_formatting` checks it.
pub fn list_entries(
    conn: &rusqlite::Connection,
    filter: EntryFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<Entry>> {
    const UNREAD: &str = "SELECT id, feed_id, status, starred, title, url, comments_url, author, \
        content, published_at, created_at, changed_at, reading_time, tags FROM entries \
        WHERE status = 'unread' ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const STARRED: &str = "SELECT id, feed_id, status, starred, title, url, comments_url, author, \
        content, published_at, created_at, changed_at, reading_time, tags FROM entries \
        WHERE starred = 1 ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const ALL: &str = "SELECT id, feed_id, status, starred, title, url, comments_url, author, \
        content, published_at, created_at, changed_at, reading_time, tags FROM entries \
        ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const BY_FEED: &str = "SELECT id, feed_id, status, starred, title, url, comments_url, author, \
        content, published_at, created_at, changed_at, reading_time, tags FROM entries \
        WHERE feed_id = ?3 ORDER BY published_at DESC, id DESC LIMIT ?1 OFFSET ?2";
    const BY_CATEGORY: &str = "SELECT id, feed_id, status, starred, title, url, comments_url, \
        author, content, published_at, created_at, changed_at, reading_time, tags FROM entries \
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
                out.push(row_to_entry(row)?);
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
                out.push(row_to_entry(row)?);
            }
        }
    }
    Ok(out)
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
        reading_time: r.get::<_, i64>(12)? as i32,
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        enclosures: Vec::new(),
    })
}

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

pub fn unread_count(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM entries WHERE status = 'unread'",
        [],
        |r| r.get(0),
    )?)
}

// ------------------------------------------------------------------ icons

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

/// Record that a feed's icon could not be fetched or decoded.
pub fn record_icon_failure(tx: &Transaction<'_>, feed_id: FeedId) -> Result<()> {
    tx.execute(
        "UPDATE feeds SET icon_failures = icon_failures + 1 WHERE id = ?1",
        [feed_id.get()],
    )?;
    Ok(())
}

/// Clear a feed's icon failure count, e.g. when the server reports a new icon.
pub fn clear_icon_failures(tx: &Transaction<'_>, feed_id: FeedId) -> Result<()> {
    tx.execute(
        "UPDATE feeds SET icon_failures = 0 WHERE id = ?1",
        [feed_id.get()],
    )?;
    Ok(())
}

/// What a list row needs to show about the feed an entry came from.
///
/// One query for every feed rather than a join onto the entry list: an entry
/// list is capped at 500 rows but a mirror has tens of feeds, so joining would
/// carry the same icon blob back hundreds of times.
#[derive(Debug, Clone)]
pub struct FeedChrome {
    pub feed_id: i64,
    /// The feed name as the user (or the feed) set it. FOREIGN TEXT.
    pub title: String,
    /// The icon's MIME type and bytes, when the mirror has fetched one.
    pub icon: Option<(String, Vec<u8>)>,
}

/// Feed names and icons, for decorating an entry list.
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
                        (Some(w), Some(h)) => Some((w as u32, h as u32)),
                        _ => None,
                    },
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Feed ids whose icon has not been downloaded yet.
///
/// §11 asks how to avoid a thundering herd on first sync; the answer is that
/// icons are fetched lazily from this list, a few at a time, rather than all
/// at once behind the first pull.
pub fn feeds_missing_icons(
    conn: &rusqlite::Connection,
    limit: i64,
) -> Result<Vec<(FeedId, IconId)>> {
    // Ordered by failure count, and giving up after a few attempts. An icon
    // that cannot be decoded never will be, and retrying it every sync starves
    // every feed behind it in the batch -- the feed list then shows default
    // icons forever while the bandwidth goes to the same broken PNG.
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncState {
    pub cursor_changed_after: Option<i64>,
    pub sync_generation: i64,
    pub last_full_reconcile_at: Option<i64>,
    pub server_era: Option<String>,
    pub server_version: Option<String>,
}

pub fn sync_state(conn: &rusqlite::Connection) -> Result<SyncState> {
    Ok(conn.query_row(
        "SELECT cursor_changed_after, sync_generation, last_full_reconcile_at, server_era,
                server_version
         FROM sync_state WHERE id = 1",
        [],
        |r| {
            Ok(SyncState {
                cursor_changed_after: r.get(0)?,
                sync_generation: r.get(1)?,
                last_full_reconcile_at: r.get(2)?,
                server_era: r.get(3)?,
                server_version: r.get(4)?,
            })
        },
    )?)
}

pub fn set_sync_state(tx: &Transaction<'_>, state: &SyncState) -> Result<()> {
    tx.execute(
        "UPDATE sync_state SET cursor_changed_after = ?1, sync_generation = ?2,
             last_full_reconcile_at = ?3, server_era = ?4, server_version = ?5
         WHERE id = 1",
        rusqlite::params![
            state.cursor_changed_after,
            state.sync_generation,
            state.last_full_reconcile_at,
            state.server_era,
            state.server_version,
        ],
    )?;
    Ok(())
}

// --------------------------------------------------------- media consent

pub fn grant_media_consent(tx: &Transaction<'_>, origin: &str, now: i64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO media_consent (origin, granted_at) VALUES (?1, ?2)",
        rusqlite::params![origin, now],
    )?;
    Ok(())
}

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
    //! `store` is the UI's whole read path, and had no tests at all.
    //!
    //! Only five of its ~20 functions were reached by any test, and only
    //! incidentally, through `sync`. `list_entries` -- every article list the
    //! app renders -- was called from `vuo-shim/src/models.rs` and nowhere
    //! else, and models.rs has no tests either. Measured: swapping the unread
    //! filter's `status = 'unread'` for `'read'` and its ordering from DESC to
    //! ASC, and routing `EntryFilter::Category` through the by-feed statement,
    //! left every one of the eleven test binaries green. The unread screen
    //! showing read articles oldest-first, and category browsing showing one
    //! feed, were both invisible.

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

            // published_at ascending with id, so "newest first" is id-descending.
            upsert_entry(tx, &entry_at(1, 10, EntryStatus::Unread, false, 100), 1)?;
            upsert_entry(tx, &entry_at(2, 10, EntryStatus::Read, true, 200), 1)?;
            upsert_entry(tx, &entry_at(3, 11, EntryStatus::Unread, true, 300), 1)?;
            upsert_entry(tx, &entry_at(4, 11, EntryStatus::Read, false, 400), 1)?;
            upsert_entry(tx, &entry_at(5, 20, EntryStatus::Unread, false, 500), 1)?;
            upsert_entry(tx, &entry_at(6, 20, EntryStatus::Read, false, 600), 1)?;
            Ok(())
        })
        .expect("seed");
        db
    }

    fn ids(entries: &[Entry]) -> Vec<i64> {
        entries.iter().map(|e| e.id.get()).collect()
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

    #[test]
    fn a_pending_intent_wins_per_field_in_both_directions() {
        // §8.3's "a server-side change to an entry mutated locally resolves by
        // a stated rule", and the rule is local intent wins PER FIELD.
        //
        // Only one direction was covered -- a pending STAR against a remote
        // status change. Making resolution per-ENTRY in the other direction
        // (honour a pending status only when a star intent also exists) left
        // all eleven binaries green, and that is the common case: mark read
        // offline, the pull echoes the entry back unread, the mark vanishes
        // from the list while the outbox still holds it.
        let mut db = populated();

        // Pending STATUS, remote changes STARRED.
        db.with_tx(|tx| {
            outbox::queue(
                tx,
                EntryId(1),
                outbox::DesiredValue::Status(EntryStatus::Read),
                1,
            )
        })
        .expect("queue status");
        db.with_tx(|tx| {
            let mut remote = entry_at(1, 10, EntryStatus::Unread, true, 100);
            remote.title = "remote title".to_owned();
            upsert_entry(tx, &remote, 2)
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

        // Pending STARRED, remote changes STATUS. The mirror case.
        db.with_tx(|tx| outbox::queue(tx, EntryId(5), outbox::DesiredValue::Starred(true), 1))
            .expect("queue star");
        db.with_tx(|tx| upsert_entry(tx, &entry_at(5, 20, EntryStatus::Read, false, 500), 2))
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
        // §9.3's consent store: agreeing to one host must not agree to every
        // host. Nothing tested it -- rewriting the SELECT left the crate green.
        let mut db = populated();
        assert!(media_consent(db.conn()).expect("read").is_empty());

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

        // Granting the same origin twice is not a duplicate.
        db.with_tx(|tx| grant_media_consent(tx, "https://cdn.example", 3))
            .expect("regrant");
        assert_eq!(media_consent(db.conn()).expect("read").len(), 2);
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
        // The pull upserts the same entry on every pass it appears in. An
        // INSERT that did not clear first would grow the table without bound.
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

        db.with_tx(|tx| upsert_entry(tx, &with_two(3), 2))
            .expect("first");
        assert_eq!(count(&db), 3);
        db.with_tx(|tx| upsert_entry(tx, &with_two(1), 3))
            .expect("second");
        assert_eq!(
            count(&db),
            1,
            "the previous set must be replaced, not added to"
        );
    }
}

#[cfg(test)]
mod name_sort_tests {
    use super::*;

    /// §the feed list is sorted the way a reader expects.
    ///
    /// Reported from a device: "Sorting is broken. Sorts alphabetically, but
    /// first uppercase entries, then lower case." That is `ORDER BY title`
    /// doing a byte comparison.
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
}
