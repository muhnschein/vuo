//! Offline write path.
//!
//! §5: local mutations (mark read, star, mark feed read) written locally, queued,
//! replayed to server in batches. Replay idempotent, survives kill mid-flight.
//!
//! # Map, not log
//!
//! Obvious design = append-only op log ("starred 7", "unstarred 7"). Wrong here,
//! because of server:
//!
//! Miniflux `PUT /v1/entries/{id}/star` = `SET starred = NOT starred`. **Toggle**,
//! not idempotent: replay after ambiguous timeout flips back. Toggle log can't be
//! replayed safely.
//!
//! Escape: `PUT /v1/entries` takes `{entry_ids, status?, starred?}`, SQL plain
//! `SET status = $1` / `SET starred = $1`: absolute set. So queue holds *desired
//! states*, not *operations*: keyed `(entry_id, field)`, queueing upserts; star,
//! unstar, star offline → one row holding `true`.
//!
//! `starred` half is **2.3.2+**: older servers know only `status`, answer 400,
//! which replay's discard rule reads as "malformed forever". Client refuses
//! entry-state writes below floor before sending: intent stays queued and visible.
//! See `api::client::update_entries`,
//! `ServerVersion::supports_entry_state_writes`.
//!
//! Three properties, exactly §5's:
//!
//! - **Idempotent replay.** Wire carries final value: resend after timeout can't
//!   double-apply. No request ids, dedup tokens, server idempotency keys.
//! - **Survives kill mid-flight.** Row deleted only after server confirms; kill
//!   between send and commit = harmless replay.
//! - **Bounded.** ≤ one row per entry per field, however long offline.
//!
//! # Mark-all-read expanded locally
//!
//! `PUT /v1/feeds/{id}/mark-all-as-read` must not be queued: server-side
//! `published_at < now()` cut-off taken *at request time*. Queued at noon,
//! replayed at six = also marks everything in between, never seen. So offline
//! mark-all expands to ids unread *now*: keeps intent, inherits idempotency.

use rusqlite::{Connection, OptionalExtension as _, Transaction};

use crate::error::Result;
use crate::model::{EntryId, EntryStatus};

/// Most ids Vuo puts in one `PUT /v1/entries`.
///
/// Server has no cap (ids = single array bind param, no Postgres param limit,
/// no request-size guard). Chosen for *retry granularity*: failed chunk resent
/// whole.
pub const MAX_IDS_PER_REQUEST: usize = 500;

/// Which field of an entry an intent concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OutboxField {
    /// Read or unread.
    Status,
    /// Starred or not.
    Starred,
}

impl OutboxField {
    fn as_str(self) -> &'static str {
        match self {
            OutboxField::Status => "status",
            OutboxField::Starred => "starred",
        }
    }
}

/// A desired absolute value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DesiredValue {
    /// The entry should end up with this read status.
    Status(EntryStatus),
    /// The entry should end up starred (`true`) or unstarred (`false`).
    Starred(bool),
}

impl DesiredValue {
    fn field(self) -> OutboxField {
        match self {
            DesiredValue::Status(_) => OutboxField::Status,
            DesiredValue::Starred(_) => OutboxField::Starred,
        }
    }

    /// Stored form. Textual: table legible when debugging user's database.
    fn as_str(self) -> &'static str {
        match self {
            DesiredValue::Status(EntryStatus::Read) => "read",
            DesiredValue::Status(EntryStatus::Unread) => "unread",
            DesiredValue::Starred(true) => "true",
            DesiredValue::Starred(false) => "false",
        }
    }

    fn parse(field: &str, value: &str) -> Option<Self> {
        match (field, value) {
            ("status", "read") => Some(DesiredValue::Status(EntryStatus::Read)),
            ("status", "unread") => Some(DesiredValue::Status(EntryStatus::Unread)),
            ("starred", "true") => Some(DesiredValue::Starred(true)),
            ("starred", "false") => Some(DesiredValue::Starred(false)),
            _ => None,
        }
    }
}

/// One queued intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMutation {
    /// The entry the intent is for.
    pub entry_id: EntryId,
    /// The value the user wants the server to hold.
    pub value: DesiredValue,
    /// Failed sends since value queued; reset when user queues new value for
    /// same field.
    pub attempts: i64,
}

/// Intents sharing field and value: one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// The value every entry in the batch should take.
    pub value: DesiredValue,
    /// Entries: ≤ [`MAX_IDS_PER_REQUEST`], never empty.
    pub entry_ids: Vec<EntryId>,
}

/// Queue intent **and apply locally**.
///
/// Local write: UI responds instantly, mirror stays UI's truth offline (§5).
/// Queue row: eventually tells server.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if either write fails. Entry missing
/// from mirror isn't error: outbox row written, local update touches nothing.
pub fn queue(tx: &Transaction<'_>, entry_id: EntryId, value: DesiredValue, now: i64) -> Result<()> {
    // Upsert, not insert: burst of toggles → one row, queue bounded. `attempts`
    // resets: new value = new intent, fresh retry budget.
    tx.execute(
        "INSERT INTO outbox (entry_id, field, value, queued_at, attempts, last_error)
         VALUES (?1, ?2, ?3, ?4, 0, NULL)
         ON CONFLICT(entry_id, field) DO UPDATE SET
             value      = excluded.value,
             queued_at  = excluded.queued_at,
             attempts   = 0,
             last_error = NULL",
        rusqlite::params![entry_id.get(), value.field().as_str(), value.as_str(), now],
    )?;

    match value {
        DesiredValue::Status(status) => {
            tx.execute(
                "UPDATE entries SET status = ?2 WHERE id = ?1",
                rusqlite::params![entry_id.get(), status.as_api_str()],
            )?;
        }
        DesiredValue::Starred(starred) => {
            tx.execute(
                "UPDATE entries SET starred = ?2 WHERE id = ?1",
                rusqlite::params![entry_id.get(), i64::from(starred)],
            )?;
        }
    }
    Ok(())
}

/// Mark every unread entry in feed read, offline-safe.
///
/// Expands to concrete ids, not server bulk endpoint (unsafe to replay; module
/// docs).
///
/// Returns count queued.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if reading unread ids or queueing
/// fails.
pub fn queue_mark_feed_read(tx: &Transaction<'_>, feed_id: i64, now: i64) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt =
            tx.prepare("SELECT id FROM entries WHERE feed_id = ?1 AND status = 'unread'")?;
        let rows = stmt.query_map([feed_id], |r| r.get::<_, i64>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in &ids {
        queue(
            tx,
            EntryId(*id),
            DesiredValue::Status(EntryStatus::Read),
            now,
        )?;
    }
    Ok(ids.len())
}

/// As [`queue_mark_feed_read`], for whole category.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if reading unread ids or queueing
/// fails.
pub fn queue_mark_category_read(tx: &Transaction<'_>, category_id: i64, now: i64) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT e.id FROM entries e
             JOIN feeds f ON f.id = e.feed_id
             WHERE f.category_id = ?1 AND e.status = 'unread'",
        )?;
        let rows = stmt.query_map([category_id], |r| r.get::<_, i64>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in &ids {
        queue(
            tx,
            EntryId(*id),
            DesiredValue::Status(EntryStatus::Read),
            now,
        )?;
    }
    Ok(ids.len())
}

/// Every pending intent, oldest first.
///
/// Rows this build can't parse (from newer Vuo) skipped.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if outbox can't be read.
pub fn pending(conn: &Connection) -> Result<Vec<PendingMutation>> {
    let mut stmt = conn.prepare(
        "SELECT entry_id, field, value, attempts FROM outbox ORDER BY queued_at ASC, entry_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (entry_id, field, value, attempts) = row?;
        // Unrecognised field/value only from newer Vuo. Skip, don't fail flush;
        // newer build sends it.
        if let Some(value) = DesiredValue::parse(&field, &value) {
            out.push(PendingMutation {
                entry_id: EntryId(entry_id),
                value,
                attempts,
            });
        }
    }
    Ok(out)
}

/// Pending intent for one entry and field, if any.
///
/// Pull resolves conflicts **per field**: remote read-status change mustn't
/// clobber local pending star.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if outbox can't be read.
pub fn pending_for(
    conn: &Connection,
    entry_id: EntryId,
    field: OutboxField,
) -> Result<Option<DesiredValue>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT field, value FROM outbox WHERE entry_id = ?1 AND field = ?2",
            rusqlite::params![entry_id.get(), field.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(f, v)| DesiredValue::parse(&f, &v)))
}

/// Group pending intents into requests: one per (field, value), chunked.
///
/// ≤ four groups (read, unread, starred, unstarred), however many entries.
#[must_use]
pub fn batches(pending: &[PendingMutation]) -> Vec<Batch> {
    let mut groups: std::collections::BTreeMap<DesiredValue, Vec<EntryId>> =
        std::collections::BTreeMap::new();
    for item in pending {
        groups.entry(item.value).or_default().push(item.entry_id);
    }

    let mut out = Vec::new();
    for (value, ids) in groups {
        for chunk in ids.chunks(MAX_IDS_PER_REQUEST) {
            // Never empty batch: `PUT /v1/entries` 400s on empty ids; retry
            // classifier reads that as permanent, drops work.
            if chunk.is_empty() {
                continue;
            }
            out.push(Batch {
                value,
                entry_ids: chunk.to_vec(),
            });
        }
    }
    out
}

/// Clear batch after server confirmed.
///
/// **Compare-and-delete, never blind.** User re-toggled in flight → row holds
/// different value, intent server hasn't seen. `value = ?` predicate keeps it.
///
/// Returns rows cleared; less than batch size exactly when re-toggled
/// mid-flight.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if a delete fails.
pub fn confirm(tx: &Transaction<'_>, batch: &Batch) -> Result<usize> {
    let field = batch.value.field().as_str();
    let value = batch.value.as_str();
    let mut cleared = 0usize;
    // One parameterised statement per id, not built `IN (...)`: §9.4 forbids
    // string-built SQL; transaction makes per-row cost irrelevant.
    let mut stmt =
        tx.prepare("DELETE FROM outbox WHERE entry_id = ?1 AND field = ?2 AND value = ?3")?;
    for id in &batch.entry_ids {
        cleared =
            cleared.saturating_add(stmt.execute(rusqlite::params![id.get(), field, value])?);
    }
    Ok(cleared)
}

/// Record failed attempt, keep intent.
///
/// Bumps `attempts`, stores `error` on rows still holding batch's value.
/// Matched like [`confirm`]: row re-toggled in flight is untried new intent;
/// old failure isn't its.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if an update fails.
pub fn record_failure(tx: &Transaction<'_>, batch: &Batch, error: &str) -> Result<()> {
    let field = batch.value.field().as_str();
    let value = batch.value.as_str();
    let mut stmt = tx.prepare(
        "UPDATE outbox SET attempts = attempts + 1, last_error = ?4
         WHERE entry_id = ?1 AND field = ?2 AND value = ?3",
    )?;
    for id in &batch.entry_ids {
        stmt.execute(rusqlite::params![id.get(), field, value, error])?;
    }
    Ok(())
}

/// Drop batch whose failure is permanent.
///
/// Only for unrecoverable rejections (400: rejected identically forever).
/// Deliberate, never on timeout.
///
/// Same compare-and-delete as [`confirm`]: row re-toggled since batch built
/// survives. Returns rows dropped.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if a delete fails.
pub fn discard(tx: &Transaction<'_>, batch: &Batch) -> Result<usize> {
    confirm(tx, batch)
}

/// How many intents waiting.
///
/// Counts every row, incl. ones [`pending`] skips as unrecognised.
///
/// # Errors
///
/// [`Error::Db`](crate::error::Error::Db) if count query fails.
pub fn len(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn db_with_entries(n: i64) -> Database {
        let mut db = Database::open_in_memory().unwrap();
        db.with_tx(|tx| {
            tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
            for i in 1..=n {
                tx.execute(
                    "INSERT INTO entries (id, feed_id, status, starred) VALUES (?1, 1, 'unread', 0)",
                    [i],
                )?;
            }
            Ok(())
        })
        .unwrap();
        db
    }

    #[test]
    fn queueing_applies_the_change_locally_too() {
        // UI reads mirror: offline action must show there at once, else looks
        // like nothing happened.
        let mut db = db_with_entries(1);
        db.with_tx(|tx| queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 100))
            .unwrap();

        let status: String = db
            .conn()
            .query_row("SELECT status FROM entries WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "read");
        assert_eq!(len(db.conn()).unwrap(), 1);
    }

    #[test]
    fn repeated_toggles_collapse_to_one_row() {
        // Queue stays bounded however long offline.
        let mut db = db_with_entries(1);
        db.with_tx(|tx| {
            for (i, starred) in [true, false, true, false, true].into_iter().enumerate() {
                queue(
                    tx,
                    EntryId(1),
                    DesiredValue::Starred(starred),
                    i64::try_from(i).unwrap(),
                )?;
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(
            len(db.conn()).unwrap(),
            1,
            "an op log would have five rows here"
        );
        let p = pending(db.conn()).unwrap();
        assert_eq!(
            p.first().map(|m| m.value),
            Some(DesiredValue::Starred(true))
        );
    }

    #[test]
    fn status_and_starred_are_independent_intents() {
        let mut db = db_with_entries(1);
        db.with_tx(|tx| {
            queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 1)?;
            queue(tx, EntryId(1), DesiredValue::Starred(true), 2)
        })
        .unwrap();
        assert_eq!(len(db.conn()).unwrap(), 2, "one row per (entry, field)");
    }

    #[test]
    fn batches_group_by_value_and_chunk() {
        let pending: Vec<PendingMutation> = (1..=1200)
            .map(|i| PendingMutation {
                entry_id: EntryId(i),
                value: DesiredValue::Status(EntryStatus::Read),
                attempts: 0,
            })
            .collect();
        let batches = batches(&pending);
        assert_eq!(batches.len(), 3, "1200 ids at 500 per request");
        assert_eq!(batches.first().map(|b| b.entry_ids.len()), Some(500));
        assert_eq!(batches.get(2).map(|b| b.entry_ids.len()), Some(200));
        assert!(
            batches.iter().all(|b| !b.entry_ids.is_empty()),
            "an empty batch is a 400"
        );
    }

    #[test]
    fn batches_never_mix_values() {
        let pending = vec![
            PendingMutation {
                entry_id: EntryId(1),
                value: DesiredValue::Status(EntryStatus::Read),
                attempts: 0,
            },
            PendingMutation {
                entry_id: EntryId(2),
                value: DesiredValue::Status(EntryStatus::Unread),
                attempts: 0,
            },
            PendingMutation {
                entry_id: EntryId(3),
                value: DesiredValue::Starred(true),
                attempts: 0,
            },
        ];
        let batches = batches(&pending);
        assert_eq!(batches.len(), 3);
        for b in &batches {
            assert_eq!(b.entry_ids.len(), 1);
        }
    }

    #[test]
    fn confirm_clears_only_what_was_actually_sent() {
        // Mid-flight re-toggle: why confirm compares value instead of deleting
        // by key.
        let mut db = db_with_entries(2);
        db.with_tx(|tx| {
            queue(tx, EntryId(1), DesiredValue::Starred(true), 1)?;
            queue(tx, EntryId(2), DesiredValue::Starred(true), 1)
        })
        .unwrap();

        let sent = Batch {
            value: DesiredValue::Starred(true),
            entry_ids: vec![EntryId(1), EntryId(2)],
        };

        // User unstars entry 2 while request in flight.
        db.with_tx(|tx| queue(tx, EntryId(2), DesiredValue::Starred(false), 2))
            .unwrap();

        let cleared = db.with_tx(|tx| confirm(tx, &sent)).unwrap();
        assert_eq!(cleared, 1, "only entry 1's intent was the one confirmed");
        assert_eq!(
            len(db.conn()).unwrap(),
            1,
            "entry 2's newer intent must survive: the server has not seen it"
        );
        let p = pending(db.conn()).unwrap();
        assert_eq!(
            p.first().map(|m| m.value),
            Some(DesiredValue::Starred(false))
        );
    }

    #[test]
    fn replay_survives_being_killed_mid_flight() {
        // Crash model: request sent, server applied, process died before
        // confirm. Row still queued; next flush resends, harmless because
        // payload absolute, not toggle.
        let mut db = db_with_entries(1);
        db.with_tx(|tx| queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
            .unwrap();

        let first = batches(&pending(db.conn()).unwrap());
        // ... process dies here, no confirm() ...
        let second = batches(&pending(db.conn()).unwrap());
        assert_eq!(
            first, second,
            "the intent must still be queued after a crash"
        );

        // Resend is server-side no-op; confirm afterwards clears it.
        db.with_tx(|tx| confirm(tx, first.first().unwrap()))
            .unwrap();
        assert_eq!(len(db.conn()).unwrap(), 0);
    }

    #[test]
    fn a_failed_attempt_keeps_the_intent_and_counts_it() {
        let mut db = db_with_entries(1);
        db.with_tx(|tx| queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
            .unwrap();
        let batch = Batch {
            value: DesiredValue::Starred(true),
            entry_ids: vec![EntryId(1)],
        };

        db.with_tx(|tx| record_failure(tx, &batch, "timed out"))
            .unwrap();
        let p = pending(db.conn()).unwrap();
        assert_eq!(p.len(), 1, "a failure must never lose the user's action");
        assert_eq!(p.first().map(|m| m.attempts), Some(1));
    }

    #[test]
    fn a_failure_is_not_charged_to_an_intent_queued_after_the_request() {
        // User stars, flush sends, user unstars before request fails. Unsent
        // unstar must not carry star's error or attempt.
        let mut db = db_with_entries(1);
        db.with_tx(|tx| queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
            .unwrap();
        let batch = Batch {
            value: DesiredValue::Starred(true),
            entry_ids: vec![EntryId(1)],
        };
        db.with_tx(|tx| queue(tx, EntryId(1), DesiredValue::Starred(false), 2))
            .unwrap();

        db.with_tx(|tx| record_failure(tx, &batch, "timed out"))
            .unwrap();
        let p = pending(db.conn()).unwrap();
        assert_eq!(
            p.first().map(|m| m.value),
            Some(DesiredValue::Starred(false))
        );
        assert_eq!(p.first().map(|m| m.attempts), Some(0));
        let last_error: Option<String> = db
            .conn()
            .query_row(
                "SELECT last_error FROM outbox WHERE entry_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last_error, None);
    }

    #[test]
    fn an_offline_mark_all_read_expands_to_concrete_ids() {
        // Server bulk endpoint would mark everything published between tap and
        // replay.
        let mut db = db_with_entries(5);
        db.with_tx(|tx| {
            // One entry already read: not re-queued.
            tx.execute("UPDATE entries SET status = 'read' WHERE id = 3", [])?;
            let n = queue_mark_feed_read(tx, 1, 10)?;
            assert_eq!(n, 4);
            Ok(())
        })
        .unwrap();

        assert_eq!(len(db.conn()).unwrap(), 4);
        let unread: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE status = 'unread'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            unread, 0,
            "the local mirror reflects the action immediately"
        );
    }

    #[test]
    fn pending_for_resolves_conflicts_per_field() {
        let mut db = db_with_entries(1);
        db.with_tx(|tx| queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
            .unwrap();

        assert_eq!(
            pending_for(db.conn(), EntryId(1), OutboxField::Starred).unwrap(),
            Some(DesiredValue::Starred(true))
        );
        assert_eq!(
            pending_for(db.conn(), EntryId(1), OutboxField::Status).unwrap(),
            None,
            "a pending star must not make the pull think status is also pending"
        );
    }

    #[test]
    fn an_unrecognised_queued_row_is_skipped_not_fatal() {
        // Newer Vuo could queue field this build predates.
        let db = db_with_entries(1);
        db.conn()
            .execute(
                "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (1,'status','sideways',1)",
                [],
            )
            .unwrap();
        assert_eq!(pending(db.conn()).unwrap(), Vec::new());
    }
}
