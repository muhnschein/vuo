//! The local SQLite mirror.
//!
//! §5: *the local SQLite mirror is the single source of truth for the UI. The
//! UI never waits on the network. Sync writes to SQLite; models observe SQLite.
//! This makes offline reading a consequence of the architecture rather than a
//! feature bolted on later.*
//!
//! # §9.4, in full
//!
//! - **Parameterised SQL only.** No query anywhere in this crate is built with
//!   `format!` — not even the "obviously safe" ones with an integer id. Where
//!   a statement genuinely cannot take a bind parameter (`PRAGMA
//!   user_version`), rusqlite's `pragma_update` is used, which does. Batched
//!   operations issue one parameterised statement per row inside a transaction
//!   rather than assembling an `IN (?,?,?)` list, so there is no dynamic SQL
//!   at all.
//! - **No filesystem path is ever derived from server data.** Nothing here
//!   opens a file named after a feed title or a URL path; icons are stored as
//!   blobs in the database, keyed by the server's integer icon id.
//! - **Migrations preserve the outbox.** See [`migrations`].

pub mod migrations;
pub mod outbox;
pub mod store;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// A handle to the local mirror.
#[derive(Debug)]
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (creating if absent) and migrate the mirror at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        let mut db = Database { conn };
        migrations::migrate(&mut db.conn)?;
        Ok(db)
    }

    /// An in-memory mirror, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        let mut db = Database { conn };
        migrations::migrate(&mut db.conn)?;
        Ok(db)
    }

    fn configure(conn: &Connection) -> Result<()> {
        // WAL: the background sync unit and the UI process both hold the
        // database open. WAL lets a reader proceed while a writer commits,
        // which is the difference between a UI that scrolls during a sync and
        // one that stutters.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // NORMAL is durable against process death (which is what matters for
        // the outbox) though not against sudden power loss. On a phone with
        // flash storage the fsync-per-commit cost of FULL is not worth paying
        // for feed state.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Enclosure rows cascade from entries; without this the constraint is
        // decorative.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Rather than failing immediately when the other process holds a write
        // lock.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(())
    }

    #[must_use]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Run `f` inside a WRITE transaction, committing on `Ok` and rolling back
    /// on `Err`.
    ///
    /// `BEGIN IMMEDIATE`, not the `BEGIN DEFERRED` that `Connection::transaction`
    /// gives by default. The difference is not academic here.
    ///
    /// A deferred transaction in WAL mode takes its read snapshot at the first
    /// *read*. If another connection commits after that point, the later
    /// *write* fails with `SQLITE_BUSY_SNAPSHOT` — and `busy_timeout` does not
    /// rescue it, because the transaction cannot be saved by waiting; it has to
    /// be rolled back and retried.
    ///
    /// Vuo has exactly the shape that hits this. The UI process and the systemd
    /// timer process both hold the mirror open (see [`Database::configure`]),
    /// and the write path is read-then-write: "mark this feed read" reads the
    /// unread ids, then queues them. Measured before this change, with the
    /// timer committing between those two halves, the user's mark failed with
    /// "database is locked" and was lost.
    ///
    /// Taking the write lock at `BEGIN` removes the upgrade, so the contention
    /// lands on the *other* writer, where `busy_timeout` applies normally. That
    /// is the right way round: a background refresh can be retried on the next
    /// tick, a user's action cannot be invented again.
    ///
    /// Readers are unaffected — reads in this crate go through
    /// [`Database::conn`] and never open a transaction.
    pub fn with_tx<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_database_is_migrated_and_usable() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(
            migrations::current_version(db.conn()).unwrap(),
            migrations::target_version()
        );
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let db = Database::open_in_memory().unwrap();
        // enclosures.entry_id references entries(id); without FK enforcement
        // this orphan insert would succeed.
        let orphan = db.conn().execute(
            "INSERT INTO enclosures (id, entry_id, url) VALUES (1, 424242, 'https://x.example/a')",
            [],
        );
        assert!(orphan.is_err(), "foreign keys must be on");
    }

    #[test]
    fn a_rolled_back_transaction_leaves_nothing_behind() {
        let mut db = Database::open_in_memory().unwrap();
        let result: Result<()> = db.with_tx(|tx| {
            tx.execute("INSERT INTO categories (id, title) VALUES (1, 'x')", [])?;
            Err(crate::error::Error::Cancelled)
        });
        assert!(result.is_err());
        let n: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM categories", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_write_transaction_survives_a_concurrent_committer() {
        // Regression: with BEGIN DEFERRED this failed with "database is
        // locked" and the user's queued action was lost. Vuo runs the UI and
        // a systemd timer against the same file, so this race is the normal
        // case, not an exotic one.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mirror.sqlite");
        let mut ui = Database::open(&path).expect("ui");
        let mut timer = Database::open(&path).expect("timer");

        ui.with_tx(|tx| {
            tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (1, 1, 'unread')",
                [],
            )?;
            Ok(())
        })
        .expect("seed");

        // Open the UI's write transaction and read inside it, exactly as
        // "mark this feed read" does.
        let tx = ui
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("begin");
        let unread: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE status = 'unread'",
                [],
                |r| r.get(0),
            )
            .expect("read");
        assert_eq!(unread, 1);

        // The timer would commit here. With IMMEDIATE the UI already holds the
        // write lock, so the timer waits rather than invalidating the UI's
        // snapshot; give it a short timeout so this test cannot hang.
        timer
            .conn()
            .busy_timeout(std::time::Duration::from_millis(50))
            .expect("timeout");
        let concurrent = timer.with_tx(|t| {
            t.execute("UPDATE entries SET title = 'x' WHERE id = 1", [])?;
            Ok(())
        });
        assert!(
            concurrent.is_err(),
            "the second writer should be the one that waits"
        );

        // The user's write still succeeds. That is the property that matters:
        // a queued mark must never be lost to a background sync.
        tx.execute("UPDATE entries SET status = 'read' WHERE id = 1", [])
            .expect("the user's write must not be lost");
        tx.commit().expect("commit");

        let read: i64 = ui
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE status = 'read'",
                [],
                |r| r.get(0),
            )
            .expect("verify");
        assert_eq!(read, 1);
    }

    #[test]
    fn no_sql_in_this_crate_is_built_by_formatting() {
        // §9.4 is a structural rule, so it gets a structural test. This scans
        // the database layer for string-built SQL, which is the shape the rule
        // forbids -- including the "obviously safe" integer-id case.
        for (name, source) in [
            ("db/mod.rs", include_str!("mod.rs")),
            ("db/store.rs", include_str!("store.rs")),
            ("db/outbox.rs", include_str!("outbox.rs")),
            ("db/migrations.rs", include_str!("migrations.rs")),
        ] {
            for (n, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with("///") {
                    continue;
                }
                let formatted_sql = ["format!", "&format!"].iter().any(|m| line.contains(m))
                    && ["SELECT", "INSERT", "UPDATE", "DELETE", "PRAGMA", "CREATE"]
                        .iter()
                        .any(|kw| line.to_ascii_uppercase().contains(kw));
                assert!(
                    !formatted_sql,
                    "{name}:{} builds SQL with format!: {line}",
                    n + 1
                );
            }
        }
    }
}
