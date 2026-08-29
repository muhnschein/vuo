//! The local SQLite mirror.
//!
//! §5: *the local SQLite mirror is the single source of truth for the UI. The
//! UI never waits on the network. Sync writes to SQLite; models observe SQLite.
//! This makes offline reading a consequence of the architecture rather than a
//! feature bolted on later.*
//!
//! # §9.4, in full
//!
//! - **No SQL is built by string formatting.** No query anywhere in this crate
//!   is assembled with `format!` — not even the "obviously safe" ones with an
//!   integer id — and a test enforces it. There is exactly one statement that
//!   is not parameterised, and it is worth stating precisely rather than
//!   glossing: `PRAGMA user_version = N` cannot take a bind parameter at all,
//!   in any SQLite client. `rusqlite`'s `pragma_update` does *not* bind it
//!   either — it renders the value into the statement text and calls
//!   `execute_batch`. What makes that acceptable here is not the API but the
//!   value: an `i64` read from this crate's own `MIGRATIONS` table, never from
//!   the server, and never from anything a user or a feed can influence.
//!   Do not reach for `pragma_update` to set a pragma from configuration or
//!   from a server response; it is not an escaping layer.
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
        // busy_timeout FIRST, before any pragma that needs a lock.
        //
        // Setting `journal_mode = WAL` takes a brief exclusive lock, so two
        // processes opening the mirror at the same moment contend on it -- and
        // with the timeout still unset, the loser failed immediately with
        // "database is locked" rather than waiting a few milliseconds. On a
        // device the UI and the systemd timer really do start together, so
        // this ordering is the difference between a working app and one that
        // fails to launch when the timer fires.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL: the background sync unit and the UI process both hold the
        // database open. WAL lets a reader proceed while a writer commits,
        // which is the difference between a UI that scrolls during a sync and
        // one that stutters.
        Self::ensure_wal(conn)?;
        // NORMAL is durable against process death (which is what matters for
        // the outbox) though not against sudden power loss. On a phone with
        // flash storage the fsync-per-commit cost of FULL is not worth paying
        // for feed state.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Enclosure rows cascade from entries; without this the constraint is
        // decorative.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// Put the database into WAL mode, tolerating another process doing the
    /// same thing at the same moment.
    ///
    /// Changing `journal_mode` needs a brief exclusive lock, and — unlike
    /// almost every other statement — **`busy_timeout` does not apply to it**:
    /// SQLite returns `SQLITE_BUSY` immediately rather than waiting. So two
    /// processes opening a fresh mirror together (the UI and the systemd timer
    /// starting at once, which happens on a device) had one of them fail
    /// outright with "database is locked".
    ///
    /// Journal mode is a persistent property of the file, so the loser does
    /// not need to win: it only needs the file to end up in WAL. Check first,
    /// and on a busy error re-read rather than giving up.
    fn ensure_wal(conn: &Connection) -> Result<()> {
        // A bounded retry, not a single re-read. The loser of the race can
        // observe the file still in "delete" mode for a moment after its own
        // attempt fails — the winner has taken the lock but not yet committed
        // the change — so checking once and giving up is itself racy, and
        // fails intermittently in exactly the situation this exists to handle.
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
            // Someone else holds the lock. Journal mode is a persistent
            // property of the file, so we do not need to be the one who sets
            // it — only to see it set.
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(PAUSE);
            }
        }

        Err(crate::error::Error::Db(format!(
            "could not put the database into WAL mode (it is in {last:?} mode); \
             concurrent access from the UI and the sync timer needs WAL"
        )))
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
    /// timer process both hold the mirror open, and the write path is
    /// read-then-write: "mark this feed read" reads the unread ids, then queues
    /// them. Measured before this change, with the timer committing between
    /// those two halves, the user's mark failed with "database is locked" and
    /// was lost.
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

        // The read-then-write goes through `with_tx`, which is the code this
        // test is named for.
        //
        // It used to build the UI's transaction by hand with
        // `TransactionBehavior::Immediate` -- supplying from the fixture the
        // exact behaviour it claimed to be checking. Reverting `with_tx` to
        // `Deferred`, the defect the comment above describes, left it green.
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

            // The timer commits between the UI's read and its write. That is
            // precisely the window BEGIN DEFERRED leaves open: the snapshot is
            // taken at the read, and the write then fails with
            // SQLITE_BUSY_SNAPSHOT, which busy_timeout cannot rescue.
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
        // §9.4 is a structural rule, so it gets a structural test.
        //
        // Deliberately NOT line-local and NOT a hand-maintained file list: the
        // first version of this test was both, and would have missed the
        // ordinary rustfmt-shaped form
        //
        //     let sql = format!(
        //         "SELECT ... WHERE feed_id = {id}"
        //     );
        //
        // because the `format!` and the keyword land on different lines. It
        // now walks every .rs file under src/ and scans with comments stripped
        // and whitespace collapsed.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(
            files.len() > 5,
            "found too few sources to be scanning the crate"
        );

        // The guard must be able to fail. `BUILDERS` used to be declared here
        // and then discarded with `let _ = BUILDERS;` while the loop scanned
        // only for `format!` -- so the most natural way to write the violation,
        // `String::from("SELECT ...")` plus `push_str`, was invisible. These
        // two assertions are what stop that happening again.
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
            // Only production code. §9.4 is about the queries Vuo runs, and a
            // test module inevitably contains the words it is scanning for --
            // including this one.
            let source = source.split("#[cfg(test)]").next().unwrap_or("").to_owned();
            // Strip line comments so prose about SQL is not a finding.
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

    /// The window around the first string-building construct that sits near an
    /// SQL keyword, or `None`.
    ///
    /// The window is bidirectional: rustfmt readily puts the keyword BEFORE the
    /// builder, as it does in the `String::from(...)` then `push_str` shape.
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

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
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
