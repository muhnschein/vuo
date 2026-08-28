//! The offline write path.
//!
//! §5: *local mutations go through an outbox. Marking read, starring, and
//! marking a whole feed read are written locally and enqueued, then replayed
//! against the server in batches. Replay must be idempotent and survive being
//! killed mid-flight. This is the part most worth writing tests for first.*
//!
//! # Why this is a map and not a log
//!
//! The obvious design is an append-only operation log: "user starred 7", "user
//! unstarred 7", replayed in order. It is the wrong design here, for a reason
//! that comes from the server rather than from taste.
//!
//! Miniflux's `PUT /v1/entries/{id}/star` is implemented as
//! `SET starred = NOT starred`. It is a **toggle**, so it is not idempotent:
//! replaying one after an ambiguous timeout flips the value back. An operation
//! log made of toggles cannot be safely replayed at all.
//!
//! The escape is `PUT /v1/entries`, which takes `{entry_ids, status?, starred?}`
//! and whose SQL is a plain `SET status = $1` / `SET starred = $1` — an
//! absolute set. To use it, the queue has to hold *desired states* rather than
//! *operations*. So the outbox is keyed `(entry_id, field)` and queueing
//! upserts: star, unstar, star again while offline collapses to a single row
//! holding `true`.
//!
//! Three properties fall out, and they are exactly the ones §5 asks for:
//!
//! - **Idempotent replay.** What goes on the wire is a final value, so
//!   resending after a timeout cannot double-apply. No request ids, no dedup
//!   tokens, no server-side idempotency keys.
//! - **Survives being killed mid-flight.** A row is deleted only after the
//!   server confirms, so a process killed between send and commit simply
//!   replays — harmlessly, by the previous point.
//! - **Bounded.** The queue cannot grow past one row per entry per field
//!   however long the device stays offline.
//!
//! # Why mark-all-as-read is expanded locally
//!
//! `PUT /v1/feeds/{id}/mark-all-as-read` looks like the right call for
//! "mark this feed read", and it must not be queued. It applies a server-side
//! `published_at < now()` cut-off captured *at request time*. Queued offline at
//! noon and replayed at six, it also marks everything that arrived in between —
//! entries the user never saw and never asked to mark. So an offline mark-all
//! is expanded into the concrete set of entry ids that are unread *now*, which
//! preserves the user's actual intent and inherits the idempotency above.

use rusqlite::{Connection, OptionalExtension as _, Transaction};

use crate::error::Result;
use crate::model::{EntryId, EntryStatus};

/// The largest number of ids Vuo puts in one `PUT /v1/entries` call.
///
/// The server enforces no cap: the ids travel as a single array bind parameter
/// so Postgres's parameter limit does not apply, and there is no request-size
/// guard in the API layer. The chunk size is therefore ours to choose, and it
/// is chosen for *retry granularity* — a failed chunk is re-sent whole, so it
/// should be no larger than we are willing to resend.
pub const MAX_IDS_PER_REQUEST: usize = 500;

/// Which field of an entry an intent concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OutboxField {
    Status,
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
    Status(EntryStatus),
    Starred(bool),
}

impl DesiredValue {
    fn field(self) -> OutboxField {
        match self {
            DesiredValue::Status(_) => OutboxField::Status,
            DesiredValue::Starred(_) => OutboxField::Starred,
        }
    }

    /// The stored representation. Kept textual so the table is legible when
    /// someone is debugging a user's database.
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
    pub entry_id: EntryId,
    pub value: DesiredValue,
    pub attempts: i64,
}

/// A group of intents that share a field and a value, ready to be one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub value: DesiredValue,
    pub entry_ids: Vec<EntryId>,
}

/// Queue an intent **and apply it locally**.
///
/// Both halves matter. The local write is what makes the UI respond instantly
/// and keeps §5's "the mirror is the source of truth for the UI" true while
/// offline; the queue row is what eventually tells the server.
pub fn queue(tx: &Transaction<'_>, entry_id: EntryId, value: DesiredValue, now: i64) -> Result<()> {
    // Upsert, not insert: this is what collapses a burst of toggles into one
    // row and keeps the queue bounded. `attempts` resets because a new value
    // is a new intent and deserves a fresh retry budget.
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

/// Mark every currently-unread entry in a feed as read, offline-safely.
///
/// Expands to concrete ids rather than queueing the server's bulk endpoint;
/// see the module docs for why that endpoint is unsafe to replay.
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

/// As [`queue_mark_feed_read`], for a whole category.
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
        // A row whose field/value pair is unrecognised can only come from a
        // newer Vuo that queued something this build does not understand. Skip
        // it rather than failing the flush; the newer build will send it.
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

/// The pending intent for one entry and field, if any.
///
/// Used by the pull to resolve conflicts **per field**: a remote read-status
/// change must not clobber a local pending star.
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
/// At most four distinct groups exist — read, unread, starred, unstarred —
/// however many entries are queued.
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
            // Never emit an empty batch: `PUT /v1/entries` answers 400 for an
            // empty id list, which the retry classifier would then read as a
            // permanent client error and drop the work.
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

/// Clear a batch after the server confirmed it.
///
/// **Compare-and-delete, never blind delete.** If the user re-toggled while the
/// request was in flight, the row now holds a different value and represents an
/// intent the server has not seen. Deleting it would silently discard a user
/// action. The `value = ?` predicate is what makes that impossible.
///
/// Returns how many rows were actually cleared, which is less than the batch
/// size exactly when something was re-toggled mid-flight.
pub fn confirm(tx: &Transaction<'_>, batch: &Batch) -> Result<usize> {
    let field = batch.value.field().as_str();
    let value = batch.value.as_str();
    let mut cleared = 0usize;
    // One parameterised statement per id rather than a built `IN (...)` list:
    // §9.4 forbids string-built SQL, and a transaction makes the per-row cost
    // irrelevant.
    let mut stmt =
        tx.prepare("DELETE FROM outbox WHERE entry_id = ?1 AND field = ?2 AND value = ?3")?;
    for id in &batch.entry_ids {
        cleared += stmt.execute(rusqlite::params![id.get(), field, value])?;
    }
    Ok(cleared)
}

/// Record a failed attempt without losing the intent.
pub fn record_failure(tx: &Transaction<'_>, batch: &Batch, error: &str) -> Result<()> {
    let field = batch.value.field().as_str();
    let mut stmt = tx.prepare(
        "UPDATE outbox SET attempts = attempts + 1, last_error = ?3
         WHERE entry_id = ?1 AND field = ?2",
    )?;
    for id in &batch.entry_ids {
        stmt.execute(rusqlite::params![id.get(), field, error])?;
    }
    Ok(())
}

/// Drop a batch whose failure is permanent.
///
/// Only for genuinely unrecoverable rejections (a 400 means the payload will
/// be rejected identically forever). Called deliberately, never on a timeout.
pub fn discard(tx: &Transaction<'_>, batch: &Batch) -> Result<usize> {
    confirm(tx, batch)
}

/// How many intents are waiting.
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
        // The UI reads the mirror, so an offline action has to be visible
        // there immediately or it looks like nothing happened.
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
        // The queue must stay bounded however long the device is offline.
        let mut db = db_with_entries(1);
        db.with_tx(|tx| {
            for (i, starred) in [true, false, true, false, true].into_iter().enumerate() {
                queue(tx, EntryId(1), DesiredValue::Starred(starred), i as i64)?;
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
        // The mid-flight re-toggle case. This is the reason confirm compares
        // the value instead of deleting by key.
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

        // While the request is in flight the user unstars entry 2.
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
        // Model the crash: the request went out and the server applied it, but
        // the process died before confirming. The row is still queued, so the
        // next flush resends -- which is harmless precisely because the
        // payload is an absolute value rather than a toggle.
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

        // Resending is a no-op server-side; confirming afterwards clears it.
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
    fn an_offline_mark_all_read_expands_to_concrete_ids() {
        // Queueing the server's bulk endpoint instead would mark everything
        // published between the tap and the replay.
        let mut db = db_with_entries(5);
        db.with_tx(|tx| {
            // One entry is already read and must not be re-queued.
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
        // A newer Vuo could queue a field this build predates.
        let db = db_with_entries(1);
        db.conn()
            .execute(
                "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (1,'status','sideways',1)",
                [],
            )
            .unwrap();
        assert!(pending(db.conn()).unwrap().is_empty());
    }
}
