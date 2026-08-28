//! Schema migration tests.
//!
//! §8.3: *the SQLite mirror is the source of truth, so an upgrade that drops a
//! pending outbox loses user actions. Test each migration against a fixture
//! database from the previous version.*
//!
//! # How to add a test when you add a migration
//!
//! There is currently one migration, so there is no *older* on-disk format to
//! restore from yet. The harness is here anyway, with the fixture built in
//! code, so that adding migration 2 is a matter of:
//!
//! 1. Committing a real database file produced by the previous release into
//!    `tests/fixtures/schema-v1.sqlite` (a real file, not one built by the
//!    current code — the point is to test against what actually shipped).
//! 2. Adding a case to [`fixtures_upgrade_without_data_loss`].
//!
//! The property that matters most is the last one in this file: a migration
//! must never drop the outbox. Everything else in the mirror can be re-fetched
//! from the server. Pending user actions cannot — they exist *only* on the
//! device, and losing them silently un-does marks and stars the user believes
//! they made.

// Test code: see the note in vuo-core's lib.rs. The unwrap/panic denials
// guard foreign-input paths in production, not assertions in tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
use rusqlite::Connection;
use vuo_core::db::migrations;
use vuo_core::db::outbox::{self, DesiredValue};
use vuo_core::db::Database;
use vuo_core::model::{EntryId, EntryStatus};

/// Build a database at the current schema, populated the way a real one is.
fn populated(path: &std::path::Path) {
    let mut db = Database::open(path).expect("open");
    db.with_tx(|tx| {
        tx.execute("INSERT INTO categories (id, title) VALUES (1, 'News')", [])?;
        tx.execute(
            "INSERT INTO feeds (id, category_id, title) VALUES (10, 1, 'A feed')",
            [],
        )?;
        for i in 1..=5i64 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status, title) VALUES (?1, 10, 'unread', 'e')",
                [i],
            )?;
        }
        // Pending user actions: the thing that must survive at all costs.
        outbox::queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 100)?;
        outbox::queue(tx, EntryId(2), DesiredValue::Starred(true), 101)?;
        Ok(())
    })
    .expect("populate");
}

#[test]
fn a_fresh_database_migrates_to_the_current_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vuo.sqlite");
    let db = Database::open(&path).expect("open");
    assert_eq!(
        migrations::current_version(db.conn()).expect("version"),
        migrations::target_version()
    );
}

#[test]
fn reopening_a_populated_database_preserves_everything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vuo.sqlite");
    populated(&path);

    // Re-open, which re-runs the migration path against an already-migrated
    // file. This is what every app launch after the first one does.
    let db = Database::open(&path).expect("reopen");
    let entries: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .expect("count");
    assert_eq!(entries, 5);
    assert_eq!(outbox::len(db.conn()).expect("outbox"), 2);
}

// Note: the "a later migration preserves the outbox" property is exercised in
// `db::migrations`' own unit tests, which can drive the migration machinery
// with a synthetic second step. It cannot be tested from out here yet, because
// only one schema version has ever shipped and re-running CREATE TABLE against
// an existing schema is not what an upgrade does -- a test that rewound
// `user_version` would be testing a scenario that cannot occur.

#[test]
fn a_database_from_a_newer_vuo_is_refused_rather_than_corrupted() {
    // Downgrading is not supported, and writing to a schema this build does
    // not understand could destroy data the newer build relies on. Refusing
    // with an actionable message is the only safe answer.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("vuo.sqlite");
    populated(&path);
    {
        let conn = Connection::open(&path).expect("raw open");
        conn.pragma_update(None, "user_version", 9999i64)
            .expect("bump");
    }

    let err = Database::open(&path).expect_err("should refuse");
    let message = err.to_string();
    assert!(
        message.contains("9999"),
        "the error should name the version: {message}"
    );
    assert!(
        message.contains("Upgrade Vuo") || message.contains("remove the local mirror"),
        "the error should tell the user what to do: {message}"
    );
}

#[test]
fn fixtures_upgrade_without_data_loss() {
    // Add a case here for each shipped schema version, restoring a REAL
    // database file produced by that release. See the module docs.
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    if !fixtures.exists() {
        // Nothing to test yet: only one schema version has ever shipped.
        return;
    }

    let mut found = 0usize;
    for entry in std::fs::read_dir(&fixtures)
        .expect("fixtures dir")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sqlite") {
            continue;
        }
        found += 1;

        // Copy, because migrating mutates and a fixture must stay pristine.
        let dir = tempfile::tempdir().expect("tempdir");
        let working = dir.path().join("fixture.sqlite");
        std::fs::copy(&path, &working).expect("copy fixture");

        let outbox_before: i64 = {
            let conn = Connection::open(&working).expect("open fixture");
            conn.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
                .unwrap_or(0)
        };

        let db = Database::open(&working)
            .unwrap_or_else(|e| panic!("migrating {} failed: {e}", path.display()));
        assert_eq!(
            migrations::current_version(db.conn()).expect("version"),
            migrations::target_version(),
            "{} did not reach the current schema",
            path.display()
        );
        assert_eq!(
            outbox::len(db.conn()).expect("outbox"),
            outbox_before,
            "migrating {} lost pending user actions",
            path.display()
        );
    }
    let _ = found;
}
