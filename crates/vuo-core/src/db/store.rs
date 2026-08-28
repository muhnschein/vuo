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
    let mut stmt = tx.prepare("SELECT id, title, hide_globally FROM categories ORDER BY title")?;
    let rows = stmt.query_map([], |r| {
        Ok(Category {
            id: CategoryId(r.get(0)?),
            title: r.get(1)?,
            hide_globally: r.get::<_, i64>(2)? != 0,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ------------------------------------------------------------------ feeds

pub fn upsert_feed(tx: &Transaction<'_>, f: &Feed, generation: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO feeds (id, category_id, title, site_url, feed_url, icon_id, checked_at,
                            parsing_error_message, parsing_error_count, disabled, hide_globally,
                            last_seen_sync)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
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
            generation,
        ],
    )?;
    Ok(())
}

pub fn feeds(conn: &rusqlite::Connection) -> Result<Vec<Feed>> {
    let mut stmt = conn.prepare(
        "SELECT id, category_id, title, site_url, feed_url, icon_id, checked_at,
                parsing_error_message, parsing_error_count, disabled, hide_globally
         FROM feeds ORDER BY title",
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
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Remove a feed and, by cascade, its entries.
pub fn delete_feed(tx: &Transaction<'_>, id: FeedId) -> Result<()> {
    // Entries do not declare an FK to feeds (a feed can arrive after its
    // entries during a sync), so they are removed explicitly.
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
             content = excluded.content,
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

/// Mark an entry as still present on the server without rewriting it.
pub fn touch_entry(tx: &Transaction<'_>, id: EntryId, generation: i64) -> Result<()> {
    tx.execute(
        "UPDATE entries SET last_seen_sync = ?2 WHERE id = ?1",
        rusqlite::params![id.get(), generation],
    )?;
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
    let mut stmt = conn.prepare(
        "SELECT f.id, f.icon_id FROM feeds f
         WHERE f.icon_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM icons i WHERE i.id = f.icon_id)
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
