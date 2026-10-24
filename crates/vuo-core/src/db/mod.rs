//! Local SQLite mirror.
//!
//! §5: mirror = UI's single source of truth; UI never waits on network. Sync
//! writes SQLite; models observe SQLite. Offline reading follows from
//! architecture.
//!
//! # §9.4, in full
//!
//! - **No SQL built by string formatting.** No query here uses `format!`, not
//!   even "obviously safe" integer ids; a test enforces it. One
//!   non-parameterised statement: `PRAGMA user_version = N` can't take bind
//!   param in any SQLite client. `rusqlite`'s `pragma_update` doesn't bind it
//!   either: renders value into statement text, calls `execute_batch`.
//!   Acceptable only for the value: an `i64` from this crate's `MIGRATIONS`,
//!   never server, user or feed. Don't use `pragma_update` for config or server
//!   values; not an escaping layer.
//! - **No filesystem path derived from server data.** No file named after feed
//!   title or URL path; icons stored as blobs keyed by server's integer icon
//!   id.
//! - **Migrations preserve outbox.** See [`migrations`].

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
    /// Open (create if absent) and migrate mirror at `path`.
    ///
    /// # Errors
    ///
    /// - [`Error::Db`](crate::error::Error::Db) if file can't open, connection
    ///   can't be configured (incl. WAL mode), or migration transaction fails
    ///   to begin/commit.
    /// - [`Error::Migration`](crate::error::Error::Migration) as for
    ///   [`migrations::migrate`]: schema newer than this build, or migration
    ///   step fails.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        let mut db = Database { conn };
        migrations::migrate(&mut db.conn)?;
        Ok(db)
    }

    /// In-memory mirror, for tests.
    ///
    /// # Errors
    ///
    /// As [`Database::open`], minus file errors.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        let mut db = Database { conn };
        migrations::migrate(&mut db.conn)?;
        Ok(db)
    }

    fn configure(conn: &Connection) -> Result<()> {
        // `busy_timeout` FIRST, before any pragma needing a lock.
        //
        // `journal_mode = WAL` takes brief exclusive lock; UI thread and worker
        // open mirror together at start-up. Without timeout set, loser fails at
        // once with "database is locked" instead of waiting.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL: sync and UI both hold database open. Readers proceed while
        // writer commits: UI scrolls during sync without stutter.
        Self::ensure_wal(conn)?;
        // NORMAL: durable against process death (what outbox needs), not sudden
        // power loss. FULL's fsync per commit not worth it for feed state on
        // flash.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Enclosure rows cascade from entries; without this constraint is
        // decorative.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Per connection, like pragmas: SQL function lives on registering
        // connection, not file.
        crate::search::register(conn)?;
        Ok(())
    }

    /// Put database in WAL mode, tolerating another connection doing same at
    /// same moment.
    ///
    /// Changing `journal_mode` needs brief exclusive lock and **`busy_timeout`
    /// doesn't apply**: SQLite returns `SQLITE_BUSY` at once. Two connections
    /// opening fresh mirror together (UI thread + worker at start-up, normal
    /// case) → one fails "database is locked".
    ///
    /// Journal mode persists in file: loser needn't win, only see WAL. Check
    /// first; on busy, re-read instead of giving up.
    fn ensure_wal(conn: &Connection) -> Result<()> {
        // Bounded retry, not single re-read: loser can see "delete" mode
        // briefly after own attempt fails (winner holds lock, not yet
        // committed). Checking once is itself racy.
        const ATTEMPTS: u32 = 20;
        const PAUSE: std::time::Duration = std::time::Duration::from_millis(25);

        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            last = conn
                .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                .unwrap_or_default();
            if last.eq_ignore_ascii_case("wal") {
                return Ok(());
            }
            if conn.pragma_update(None, "journal_mode", "WAL").is_ok() {
                return Ok(());
            }
            // Someone else holds lock. Journal mode persists in file: we only
            // need to see it set.
            if attempt.saturating_add(1) < ATTEMPTS {
                std::thread::sleep(PAUSE);
            }
        }

        Err(crate::error::Error::Db(format!(
            "could not put the database into WAL mode (it is in {last:?} mode); \
             concurrent access from the UI and the sync timer needs WAL"
        )))
    }

    /// Connection, mutably.
    ///
    /// For `transaction_with_behavior` (`&mut Connection`). Only
    /// `tests/concurrent_writers.rs` uses it (holds IMMEDIATE transaction while
    /// second connection writes); integration test is separate crate, so can't
    /// be `#[cfg(test)]`. Hence in `workspace_guards` EXEMPT list.
    #[must_use]
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Connection, for reads.
    ///
    /// Writes go through [`Database::with_tx`].
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Run `f` in WRITE transaction: commit on `Ok`, roll back on `Err`.
    ///
    /// `BEGIN IMMEDIATE`, not `Connection::transaction` default `BEGIN DEFERRED`.
    ///
    /// Deferred transaction in WAL takes read snapshot at first *read*. Another connection
    /// commits after → later *write* fails `SQLITE_BUSY_SNAPSHOT`; `busy_timeout` can't rescue,
    /// needs rollback and retry.
    ///
    /// Vuo has that shape: UI thread and worker share mirror; write path is read-then-write
    /// ("mark feed read" reads unread ids, then queues). Worker commit between halves → user's
    /// mark lost.
    ///
    /// Write lock at `BEGIN` removes upgrade: contention lands on *other* writer, where
    /// `busy_timeout` works. Right way round: background refresh retries next tick; user action
    /// can't be re-invented.
    ///
    /// Readers unaffected: reads use [`Database::conn`], no transaction.
    ///
    /// # Errors
    ///
    /// [`Error::Db`](crate::error::Error::Db) if transaction can't begin (writer holds lock past
    /// busy timeout) or commit; passes on `f`'s error after rollback.
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
        // `enclosures.entry_id` references `entries(id)`; without FK
        // enforcement orphan insert succeeds.
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
        // Regression: BEGIN DEFERRED → "database is locked", queued user action
        // lost. UI thread and worker share file: normal case.
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

        // Read-then-write goes through `with_tx`, the code under test.
        // Hand-built Immediate transaction would supply the behaviour being
        // checked.
        timer
            .conn()
            .busy_timeout(std::time::Duration::from_millis(50))
            .expect("timeout");

        let outcome = ui.with_tx(|tx| {
            let unread: i64 = tx.query_row(
                "SELECT COUNT(*) FROM entries WHERE status = 'unread'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(unread, 1);

            // Timer commits between UI's read and write: exact window BEGIN
            // DEFERRED leaves (snapshot at read, write fails
            // SQLITE_BUSY_SNAPSHOT, `busy_timeout` can't help).
            let concurrent = timer.with_tx(|t| {
                t.execute("UPDATE entries SET title = 'x' WHERE id = 1", [])?;
                Ok(())
            });
            assert!(
                concurrent.is_err(),
                "the second writer should be the one that waits"
            );

            tx.execute("UPDATE entries SET status = 'read' WHERE id = 1", [])?;
            Ok(())
        });
        outcome.expect("the user's write must not be lost to a background sync");

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
        // §9.4 structural rule, structural test.
        //
        // NOT line-local, NOT hand-kept file list: must catch rustfmt-shaped
        //
        //     let sql = format!(
        //         "SELECT ... WHERE feed_id = {id}"
        //     );
        //
        // where `format!` and keyword sit on different lines. Walks every .rs
        // under src/, comments stripped, whitespace collapsed.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(
            files.len() > 5,
            "found too few sources to be scanning the crate"
        );

        // Guard must be able to fail: these assertions prove
        // `String::from("SELECT ...")` + `push_str` is caught, not just
        // `format!`.
        assert!(
            sql_built_by_string_building(
                r#"let mut s = String::from("SELECT COUNT(*) FROM outbox WHERE entry_id = ");
                   s.push_str(&entry_id.to_string());"#
            )
            .is_some(),
            "the scanner cannot see a push_str violation, so it guarantees nothing"
        );
        assert!(
            sql_built_by_string_building(r#"let msg = format!("could not open {path}: {e}");"#)
                .is_none(),
            "the scanner flags an ordinary format! that has nothing to do with SQL"
        );

        for file in files {
            let source = std::fs::read_to_string(&file).unwrap_or_default();
            // Production code only: §9.4 is about queries Vuo runs; test
            // modules contain the scanned words (this one too).
            let source = source.split("#[cfg(test)]").next().unwrap_or("").to_owned();
            // Strip line comments: prose about SQL isn't a finding.
            let code: String = source
                .lines()
                .map(|l| l.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n");

            if let Some(window) = sql_built_by_string_building(&code) {
                panic!(
                    "{} assembles SQL from strings. §9.4 forbids it, including the \
                     obviously-safe integer cases.\n---\n{window}\n---",
                    file.display(),
                );
            }
        }
    }

    /// Window around first string-building construct near SQL keyword, or
    /// `None`.
    ///
    /// Bidirectional: rustfmt often puts keyword BEFORE builder
    /// (`String::from(...)` then `push_str`).
    fn sql_built_by_string_building(code: &str) -> Option<String> {
        const BUILDERS: [&str; 4] = ["format!", "concat!", "push_str", "to_string() +"];
        const KEYWORDS: [&str; 6] = [
            "SELECT ",
            "INSERT ",
            "UPDATE ",
            "DELETE FROM",
            "CREATE TABLE",
            "PRAGMA ",
        ];

        for builder in BUILDERS {
            for (index, _) in code.match_indices(builder) {
                let start = index.saturating_sub(200);
                let end = (index + 400).min(code.len());
                let Some(window) = code.get(start..end) else {
                    continue;
                };
                let upper = window.to_ascii_uppercase();
                if KEYWORDS.iter().any(|k| upper.contains(k)) {
                    return Some(window.chars().take(300).collect());
                }
            }
        }
        None
    }

    fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
}
