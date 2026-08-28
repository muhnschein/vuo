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
const MIGRATIONS: &[Migration] = &[Migration {
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
}];

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
    let from = current_version(conn)?;
    let target = target_version();

    if from > target {
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

    let tx = conn.transaction()?;
    for migration in MIGRATIONS.iter().filter(|m| m.version > from) {
        tx.execute_batch(migration.sql).map_err(|e| Error::Migration {
            version: migration.version,
            reason: format!("{} failed: {e}", migration.name),
        })?;
    }
    // `PRAGMA user_version = ?` is not bindable in raw SQL, but rusqlite's
    // pragma_update takes the value as a parameter, so even this stays out of
    // string-built SQL. §9.4 is absolute: no query in Vuo is built with
    // `format!`, including the obviously-safe ones with an integer.
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
            assert!(m.version > previous, "migration versions must ascend: {}", m.version);
            previous = m.version;
        }
    }

    #[test]
    fn no_migration_drops_the_outbox() {
        // §9.4: the mirror can be re-fetched, the outbox cannot. This is a
        // structural guard against a future migration doing the easy thing.
        for m in MIGRATIONS {
            let sql = m.sql.to_ascii_lowercase();
            assert!(
                !sql.contains("drop table outbox") && !sql.contains("delete from outbox"),
                "migration {} would destroy pending user actions",
                m.version
            );
        }
    }

    #[test]
    fn the_entries_status_check_rejects_removed() {
        let conn = fresh();
        conn.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", []).unwrap();
        let bad = conn.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (1, 1, 'removed')",
            [],
        );
        assert!(bad.is_err(), "'removed' is a deletion, not a status to mirror");

        conn.execute("INSERT INTO entries (id, feed_id, status) VALUES (2, 1, 'unread')", [])
            .unwrap();
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
