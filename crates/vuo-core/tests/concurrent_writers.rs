//! Two connections, one mirror.
//!
//! UI thread and worker share SQLite file: user action racing background sync is
//! normal case. §5: mirror = UI's single source of truth; lost write = lost *user
//! action* (mark or star user watched vanish).
//!
//! Pins `with_tx` = `BEGIN IMMEDIATE`. Under `BEGIN DEFERRED` in WAL, read-then-write
//! ("mark feed read" reads unread ids, then queues) snapshots at read, fails
//! `SQLITE_BUSY_SNAPSHOT` if timer commits between. `busy_timeout` can't rescue
//! (needs rollback and retry): user's action lost.

// Test code: see note in vuo-core's lib.rs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use vuo_core::db::{outbox, store, Database};

fn seed(db: &mut Database, entries: i64) {
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'Feed')", [])?;
        for i in 1..=entries {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        Ok(())
    })
    .expect("seed");
}

#[test]
fn a_mark_feed_read_is_not_lost_to_a_concurrent_sync() {
    // Interleaving that matters; "commit first, then act" test does NOT
    // reproduce it:
    //
    //     UI: BEGIN ... read the unread ids
    //     timer:                              BEGIN ... write ... COMMIT
    //     UI: ... write the outbox rows ... COMMIT
    //
    // BEGIN DEFERRED: UI snapshot at read, timer commit invalidates it, UI
    // write fails "database is locked", mark lost. BEGIN IMMEDIATE: UI holds
    // write lock from start, timer waits.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mirror.sqlite");

    let mut ui = Database::open(&path).expect("ui connection");
    seed(&mut ui, 5);

    // Both connections opened BEFORE UI takes lock: opening inside would
    // contend for lock itself, muddling measurement.
    let mut timer = Database::open(&path).expect("timer connection");
    // Short busy_timeout: regression fails fast, no hang.
    timer
        .conn()
        .busy_timeout(Duration::from_millis(100))
        .expect("timeout");

    let result = ui.with_tx(|tx| {
        // Read half of "mark feed read".
        let unread: i64 = tx.query_row(
            "SELECT COUNT(*) FROM entries WHERE status = 'unread'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(unread, 5);

        // Background sync tries commit here. IMMEDIATE: UI holds write lock,
        // this side waits and gives up. Correct: background refresh retryable;
        // user action can't be re-invented.
        let _ = timer.with_tx(|t| {
            t.execute("UPDATE entries SET title = 'pulled' WHERE id = 1", [])?;
            Ok(())
        });

        // Write half: fails under DEFERRED.
        outbox::queue_mark_feed_read(tx, 1, 100)
    });

    let queued = result.expect("the user's mark-feed-read must survive a concurrent sync");
    assert_eq!(queued, 5);
    assert_eq!(outbox::len(ui.conn()).expect("outbox"), 5);
    assert_eq!(store::unread_count(ui.conn()).expect("unread"), 0);
}

#[test]
fn writers_interleave_without_losing_either_side() {
    // Alternating writers, each through real API, no coordination beyond SQLite
    // locking.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mirror.sqlite");

    let mut ui = Database::open(&path).expect("ui");
    let mut timer = Database::open(&path).expect("timer");
    seed(&mut ui, 20);

    for round in 0..10i64 {
        timer
            .with_tx(|tx| {
                tx.execute(
                    "UPDATE entries SET title = ?2 WHERE id = ?1",
                    rusqlite::params![round + 1, format!("round {round}")],
                )?;
                Ok(())
            })
            .expect("timer write");

        ui.with_tx(|tx| {
            outbox::queue(
                tx,
                vuo_core::model::EntryId(round + 1),
                outbox::DesiredValue::Status(vuo_core::model::EntryStatus::Read),
                round,
            )
        })
        .expect("ui write");
    }

    assert_eq!(
        outbox::len(ui.conn()).expect("outbox"),
        10,
        "every user action survived"
    );
}

#[test]
fn a_second_writer_waits_rather_than_corrupting() {
    // BEGIN IMMEDIATE: first writer holds lock; second blocks on busy_timeout,
    // no un-upgradable snapshot.

    // Short: regression fails fast, no hang. Long enough: "it waited"
    // unambiguous on loaded machine.
    const BUSY: Duration = Duration::from_millis(300);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mirror.sqlite");

    let mut ui = Database::open(&path).expect("ui");
    let mut timer = Database::open(&path).expect("timer");
    seed(&mut ui, 1);

    timer.conn().busy_timeout(BUSY).expect("timeout");

    let held = ui
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("begin");

    let started = Instant::now();
    let blocked = timer.with_tx(|tx| {
        // Read first. No read = no snapshot to fail on; DEFERRED would block on
        // write lock like IMMEDIATE: write-only probe can't tell apart.
        // Read-then-write = real callers ("mark feed read").
        let _: i64 = tx.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
        tx.execute("UPDATE entries SET title = 'x' WHERE id = 1", [])?;
        Ok(())
    });
    let waited = started.elapsed();

    assert!(
        blocked.is_err(),
        "the second writer must wait, not proceed on a stale snapshot"
    );
    // `is_err()` alone doesn't separate fix from bug: BOTH error. IMMEDIATE:
    // loser blocks on busy_timeout, then gives up. DEFERRED: snapshot, fails on
    // write with SQLITE_BUSY_SNAPSHOT, returns near-instantly (busy_timeout
    // doesn't apply). Elapsed time tells apart.
    assert!(
        waited >= BUSY.checked_sub(Duration::from_millis(50)).unwrap(),
        "the second writer returned after {waited:?} without waiting out the {BUSY:?} \
         busy_timeout, which is the SQLITE_BUSY_SNAPSHOT signature of a DEFERRED \
         transaction rather than a writer politely queueing"
    );

    held.execute("UPDATE entries SET status = 'read' WHERE id = 1", [])
        .expect("the lock holder's write must succeed");
    held.commit().expect("commit");

    // Once lock free, other writer succeeds.
    timer
        .with_tx(|tx| {
            tx.execute("UPDATE entries SET title = 'x' WHERE id = 1", [])?;
            Ok(())
        })
        .expect("the second writer succeeds once the lock is released");
}

#[test]
fn an_opened_database_carries_a_busy_timeout() {
    // `Database::configure` 5-second busy_timeout: difference between working
    // app and one failing to launch when timer fires. Every other test sets own
    // timeout; this pins the default.
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::open(&dir.path().join("mirror.sqlite")).expect("open");
    let timeout: i64 = db
        .conn()
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .expect("read busy_timeout");
    assert!(
        timeout >= 1000,
        "an opened mirror must wait for a concurrent writer, not fail immediately; \
         busy_timeout is {timeout} ms"
    );
}

#[test]
fn two_processes_can_open_a_fresh_mirror_at_once() {
    // `migrate` reads `user_version` INSIDE its transaction. Read before: two
    // connections opening together both see 0, both run migration 1, loser
    // fails "table already exists" on healthy database. UI thread and worker
    // really open it at same moment on start-up.
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fresh.sqlite");

    // Several rounds: race timing-dependent.
    for _ in 0..5 {
        let _ = std::fs::remove_file(&path);
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();

        for _ in 0..4 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                Database::open(&path).map(|_| ())
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("thread")
                .expect("opening a fresh mirror concurrently must succeed");
        }
    }
}
