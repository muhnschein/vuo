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

    /// Run `f` inside a transaction, committing on `Ok` and rolling back on
    /// `Err`.
    pub fn with_tx<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let tx = self.conn.transaction()?;
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
