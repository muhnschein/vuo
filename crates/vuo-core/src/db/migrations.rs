//! Schema migrations.
//!
//! §9.4: *migrations must preserve the outbox. Untrusted input can crash a
//! sync; the pending user actions it was carrying must still be there on
//! restart.* And §8.3 asks for migration tests against a fixture database from
//! the previous version, which live in `tests/migrations.rs`.
//!
//! # Rules for adding a migration
//!
//! 1. **Append only.** Never edit an existing entry in [`MIGRATIONS`]; a
//!    database in the wild has already run it. Add a new one.
//! 2. **Never drop or recreate `outbox`.** Losing it loses user actions that
//!    were never sent to the server -- marks and stars the user believes they
//!    made. Everything else in the mirror can be re-fetched; the outbox cannot.
//! 3. **The whole set runs in one transaction.** A migration that fails leaves
//!    the database exactly as it was, rather than half-upgraded with no way
//!    back.

use rusqlite::Connection;

use crate::error::{Error, Result};

/// One schema step. `version` is the value `user_version` takes afterwards.
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

/// The ordered schema history. Append; never edit.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial schema",
        sql: r#"
CREATE TABLE categories (
    id             INTEGER PRIMARY KEY,
    title          TEXT    NOT NULL DEFAULT '',
    hide_globally  INTEGER NOT NULL DEFAULT 0,
    last_seen_sync INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE feeds (
    id                    INTEGER PRIMARY KEY,
    category_id           INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    title                 TEXT    NOT NULL DEFAULT '',
    site_url              TEXT,
    feed_url              TEXT,
    icon_id               INTEGER,
    checked_at            INTEGER,
    parsing_error_message TEXT    NOT NULL DEFAULT '',
    parsing_error_count   INTEGER NOT NULL DEFAULT 0,
    disabled              INTEGER NOT NULL DEFAULT 0,
    hide_globally         INTEGER NOT NULL DEFAULT 0,
    last_seen_sync        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX feeds_category_idx ON feeds(category_id);

-- 'removed' is deliberately not permitted. Pre-2.3 servers expose it as a
-- third status, but it means "this entry should not exist", which is a
-- deletion rather than a state. Translating it to a local DELETE at the
-- boundary keeps this schema independent of which deletion regime the server
-- happens to implement.
CREATE TABLE entries (
    id             INTEGER PRIMARY KEY,
    feed_id        INTEGER NOT NULL,
    status         TEXT    NOT NULL CHECK(status IN ('unread','read')),
    starred        INTEGER NOT NULL DEFAULT 0,
    title          TEXT    NOT NULL DEFAULT '',
    url            TEXT,
    comments_url   TEXT,
    author         TEXT    NOT NULL DEFAULT '',
    content        TEXT    NOT NULL DEFAULT '',
    published_at   INTEGER,
    created_at     INTEGER,
    changed_at     INTEGER,
    reading_time   INTEGER NOT NULL DEFAULT 0,
    tags           TEXT    NOT NULL DEFAULT '[]',
    last_seen_sync INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX entries_feed_idx      ON entries(feed_id);
CREATE INDEX entries_status_idx    ON entries(status, published_at DESC);
CREATE INDEX entries_published_idx ON entries(published_at DESC);
CREATE INDEX entries_starred_idx   ON entries(starred, published_at DESC) WHERE starred = 1;

CREATE TABLE enclosures (
    id        INTEGER PRIMARY KEY,
    entry_id  INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    url       TEXT,
    mime_type TEXT    NOT NULL DEFAULT '',
    size      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX enclosures_entry_idx ON enclosures(entry_id);

CREATE TABLE icons (
    id     INTEGER PRIMARY KEY,
    format TEXT NOT NULL,
    bytes  BLOB NOT NULL,
    width  INTEGER,
    height INTEGER
);

-- The outbox is a KEYED DESIRED-STATE MAP, not an append-only operation log.
--
-- The primary key is (entry_id, field), so queueing an intent overwrites any
-- previous intent for the same field. Star, unstar, star again while offline
-- collapses to one row holding the final value. That is what makes replay
-- idempotent: what gets transmitted is a desired absolute state, never a
-- delta, so resending after an ambiguous timeout cannot double-apply.
--
-- It is also the answer to the server's toggle problem. PUT /v1/entries/{id}/
-- star is `SET starred = NOT starred` -- not idempotent, unusable from a
-- queue. Because the outbox stores an absolute value, every flush can go
-- through PUT /v1/entries, which is an absolute set.
CREATE TABLE outbox (
    entry_id   INTEGER NOT NULL,
    field      TEXT    NOT NULL CHECK(field IN ('status','starred')),
    value      TEXT    NOT NULL,
    queued_at  INTEGER NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    PRIMARY KEY (entry_id, field)
) WITHOUT ROWID;

CREATE TABLE sync_state (
    id                     INTEGER PRIMARY KEY CHECK (id = 1),
    cursor_changed_after   INTEGER,
    sync_generation        INTEGER NOT NULL DEFAULT 0,
    last_full_reconcile_at INTEGER,
    server_era             TEXT,
    server_version         TEXT
);
INSERT INTO sync_state (id) VALUES (1);

-- Origins the user has agreed to load un-proxied media from (§9.3). Consent is
-- per origin, so agreeing to one host does not agree to every host.
CREATE TABLE media_consent (
    origin     TEXT PRIMARY KEY,
    granted_at INTEGER NOT NULL
);
"#,
    },
    Migration {
        version: 2,
        name: "record failed icon fetches",
        sql: r#"
-- Without this, a feed whose icon cannot be decoded is retried on every sync
-- forever, and because icons are fetched in a small batch ordered by feed id,
-- a handful of permanently-bad icons at the front starve every feed behind
-- them: measured, feeds 9 and 10 never got their icons across five passes
-- while 40 requests were spent re-fetching the same broken ones.
ALTER TABLE feeds ADD COLUMN icon_failures INTEGER NOT NULL DEFAULT 0;
"#,
    },
];

/// The schema version this build expects.
#[must_use]
pub fn target_version() -> i64 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

/// Read the database's current schema version.
pub fn current_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Bring a database up to [`target_version`].
///
/// Runs every pending migration inside one transaction, so a failure leaves
/// the file exactly as it was rather than half-upgraded.
pub fn migrate(conn: &mut Connection) -> Result<()> {
    apply(conn, MIGRATIONS)
}

/// The migration machinery, parameterised over the step list.
///
/// Split out so tests can drive it with a synthetic set and prove the
/// properties that matter -- ordering, atomicity, and above all that data
/// survives -- without waiting for a second real migration to exist.
fn apply(conn: &mut Connection, migrations: &[Migration]) -> Result<()> {
    let target = migrations.last().map_or(0, |m| m.version);

    // The version is read INSIDE the write transaction, not before it.
    //
    // Vuo opens the mirror from two processes -- the UI and the systemd timer
    // -- and on a device they can start at the same moment. Reading the
    // version first and then opening a transaction is a check-then-act race:
    // both processes see version 0, both try to run migration 1, and the loser
    // fails on `CREATE TABLE ... already exists` with the database in a
    // perfectly good state. `BEGIN IMMEDIATE` serialises them, and re-reading
    // inside means the loser sees the winner's committed version and exits
    // cleanly.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let from = current_version(&tx)?;

    if from > target {
        drop(tx);
        // A newer Vuo has already upgraded this database. Refusing is the only
        // safe answer: this build does not know what the newer schema means,
        // and writing to it could destroy data the newer build understands.
        return Err(Error::Migration {
            version: from,
            reason: format!(
                "database schema is version {from}, but this build of Vuo understands only {target}. \
                 Upgrade Vuo, or remove the local mirror to re-sync from scratch."
            ),
        });
    }
    if from == target {
        return Ok(());
    }

    for migration in migrations.iter().filter(|m| m.version > from) {
        tx.execute_batch(migration.sql)
            .map_err(|e| Error::Migration {
                version: migration.version,
                reason: format!("{} failed: {e}", migration.name),
            })?;
    }
    // `PRAGMA user_version = ?` is not bindable in ANY SQLite client, and
    // rusqlite's `pragma_update` does not bind it either: it renders the value
    // into the statement text and calls `execute_batch`. That is fine here
    // for a reason that is about the value, not the API — `target` is an i64
    // from this file's own MIGRATIONS table, never from the server. It is not
    // an escaping helper and must not be used as one.
    tx.pragma_update(None, "user_version", target)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn
    }

    #[test]
    fn migrating_a_fresh_database_reaches_the_target_version() {
        let conn = fresh();
        assert_eq!(current_version(&conn).unwrap(), target_version());
    }

    #[test]
    fn migration_is_idempotent() {
        let mut conn = fresh();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), target_version());
    }

    #[test]
    fn a_future_schema_is_refused_rather_than_written_to() {
        let mut conn = fresh();
        conn.execute_batch("PRAGMA user_version = 9999").unwrap();
        let err = migrate(&mut conn).unwrap_err();
        assert!(matches!(err, Error::Migration { .. }));
    }

    #[test]
    fn versions_are_unique_and_ascending() {
        // Guards against an append that reuses or reorders a version number.
        let mut previous = 0;
        for m in MIGRATIONS {
            assert!(
                m.version > previous,
                "migration versions must ascend: {}",
                m.version
            );
            previous = m.version;
        }
    }

    /// Queue two rows and read them back, whatever the schema version.
    fn seed_outbox(conn: &Connection) {
        conn.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![7_i64, "status", "read", 1_700_000_000_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![9_i64, "starred", "1", 1_700_000_001_i64],
        )
        .unwrap();
    }

    fn read_outbox(conn: &Connection) -> Vec<(i64, String, String)> {
        let mut stmt = conn
            .prepare("SELECT entry_id, field, value FROM outbox ORDER BY entry_id, field")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    #[test]
    fn the_real_migrations_run_forward_over_a_populated_outbox() {
        // §9.4: "Migrations must preserve the outbox." The mirror can be
        // re-fetched; queued marks and stars cannot.
        //
        // This runs the REAL `MIGRATIONS` -- not the synthetic list the
        // machinery tests use -- forward from every shipped version over a
        // populated queue. `no_migration_drops_the_outbox` below is only a
        // text scan, and a text scan cannot see `DELETE FROM main.outbox`:
        // adding exactly that to migration 2 left the whole suite green.
        for k in 1..MIGRATIONS.len() {
            let stopped_at = MIGRATIONS[k - 1].version;
            let mut conn = Connection::open_in_memory().unwrap();

            // Build the database as the release that shipped `stopped_at` left it.
            apply(&mut conn, &MIGRATIONS[..k]).unwrap();
            assert_eq!(current_version(&conn).unwrap(), stopped_at);
            seed_outbox(&conn);
            let before = read_outbox(&conn);
            assert_eq!(before.len(), 2, "the fixture must actually queue something");

            // Then upgrade it the way a user's phone would.
            migrate(&mut conn).unwrap();

            assert_eq!(
                current_version(&conn).unwrap(),
                target_version(),
                "upgrading from version {stopped_at} did not reach the target"
            );
            assert_eq!(
                read_outbox(&conn),
                before,
                "upgrading from version {stopped_at} changed the outbox. Every queued \
                 mark and star on the device would be gone."
            );
        }
    }

    #[test]
    fn no_migration_drops_the_outbox() {
        // A cheap text scan on top of the behavioural test above. It catches
        // the unqualified forms at review time, before anyone runs anything;
        // it is NOT the guarantee, because a schema-qualified name walks
        // straight past it.
        for m in MIGRATIONS {
            // Normalise whitespace so a statement split across lines, or
            // written with a quoted identifier, cannot slip past.
            let sql = m
                .sql
                .to_ascii_lowercase()
                .replace(['"', '`', '[', ']'], "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            for forbidden in [
                "drop table outbox",
                "drop table if exists outbox",
                "delete from outbox",
                "truncate outbox",
                // The standard SQLite recipe for changing a table's shape is
                // rename-create-copy-drop. Forgetting the copy silently loses
                // the queue, so the rename itself has to be deliberate: if a
                // migration genuinely needs it, it must also be reviewed here.
                "alter table outbox rename",
            ] {
                assert!(
                    !sql.contains(forbidden),
                    "migration {} contains {forbidden:?} and would risk destroying pending \
                     user actions. The mirror can be re-fetched; the outbox cannot.",
                    m.version
                );
            }
        }
    }

    #[test]
    fn the_entries_status_check_rejects_removed() {
        let conn = fresh();
        conn.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])
            .unwrap();
        let bad = conn.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (1, 1, 'removed')",
            [],
        );
        assert!(
            bad.is_err(),
            "'removed' is a deletion, not a status to mirror"
        );

        conn.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (2, 1, 'unread')",
            [],
        )
        .unwrap();
    }

    /// A synthetic second migration, so the machinery can be tested before a
    /// real one exists.
    const SYNTHETIC: &[Migration] = &[
        Migration {
            version: 1,
            name: "initial schema",
            sql: MIGRATIONS[0].sql,
        },
        Migration {
            version: 2,
            name: "synthetic: add a column",
            sql: "ALTER TABLE entries ADD COLUMN synthetic TEXT NOT NULL DEFAULT '';",
        },
    ];

    #[test]
    fn a_later_migration_preserves_existing_data_and_the_outbox() {
        // §9.4's rule, exercised against the real machinery rather than
        // asserted about it.
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn, &SYNTHETIC[..1]).unwrap();
        conn.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (7, 1, 'unread')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (7,'status','read',1)",
            [],
        )
        .unwrap();

        apply(&mut conn, SYNTHETIC).unwrap();

        assert_eq!(current_version(&conn).unwrap(), 2);
        let entries: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap();
        let pending: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(entries, 1, "a migration must not lose mirrored rows");
        assert_eq!(
            pending, 1,
            "a migration must never lose pending user actions"
        );
    }

    #[test]
    fn a_failing_migration_leaves_the_database_untouched() {
        // The whole set runs in one transaction, so a half-upgraded database
        // with no way back is not a state that can occur.
        const BROKEN: &[Migration] = &[
            Migration {
                version: 1,
                name: "initial schema",
                sql: MIGRATIONS[0].sql,
            },
            Migration {
                version: 2,
                name: "broken",
                sql: "THIS IS NOT SQL;",
            },
        ];

        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn, &BROKEN[..1]).unwrap();
        conn.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (1,'status','read',1)",
            [],
        )
        .unwrap();

        let err = apply(&mut conn, BROKEN).unwrap_err();
        assert!(matches!(err, Error::Migration { version: 2, .. }));
        assert_eq!(
            current_version(&conn).unwrap(),
            1,
            "the version must not have advanced"
        );
        let pending: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            pending, 1,
            "a failed migration must not take the outbox with it"
        );
    }

    #[test]
    fn the_outbox_key_collapses_repeated_intents() {
        let conn = fresh();
        // Two intents for the same (entry, field) must leave exactly one row:
        // this is what makes star/unstar/star while offline converge.
        conn.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (1,'starred','true',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (1,'starred','false',1)
             ON CONFLICT(entry_id, field) DO UPDATE SET value = excluded.value",
            [],
        )
        .unwrap();
        let (n, value): (i64, String) = conn
            .query_row("SELECT COUNT(*), MAX(value) FROM outbox", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(value, "false");
    }

    #[test]
    fn the_outbox_field_check_rejects_unknown_fields() {
        let conn = fresh();
        let bad = conn.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (1,'colour','red',0)",
            [],
        );
        assert!(bad.is_err());
    }
}
