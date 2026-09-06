//! Two connections, one mirror.
//!
//! Vuo runs the UI thread and the worker thread against the same SQLite file,
//! so a user action racing a background sync is the normal case rather than an exotic
//! one. §5 makes the mirror the single source of truth for the UI, which means
//! a lost write here is a lost *user action* — a mark or a star the user made
//! and watched disappear.
//!
//! Regression test for a real defect: `with_tx` used `BEGIN DEFERRED`, so in
//! WAL mode the read-then-write path ("mark this feed read" reads the unread
//! ids, then queues them) took its snapshot at the read and failed with
//! `SQLITE_BUSY_SNAPSHOT` if the timer committed in between. `busy_timeout`
//! does not rescue that — the transaction has to be rolled back and retried —
//! so the user's action was simply lost.

// Test code: see the note in vuo-core's lib.rs.
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
    // The interleaving that matters, and the one a "commit first, then act"
    // test does NOT reproduce:
    //
    //     UI: BEGIN ... read the unread ids
    //     timer:                              BEGIN ... write ... COMMIT
    //     UI: ... write the outbox rows ... COMMIT
    //
    // Under BEGIN DEFERRED the UI's snapshot is taken at its read, the timer's
    // commit invalidates it, and the UI's write fails with "database is
    // locked" — losing the user's mark. Under BEGIN IMMEDIATE the UI holds the
    // write lock from the start, so the timer is the one that waits.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mirror.sqlite");

    let mut ui = Database::open(&path).expect("ui connection");
    seed(&mut ui, 5);

    // Both connections are opened BEFORE the UI takes its lock: opening one
    // inside would itself contend for the lock and confuse what is being
    // measured.
    let mut timer = Database::open(&path).expect("timer connection");
    // Short busy_timeout so a regression fails fast rather than hanging.
    timer
        .conn()
        .busy_timeout(Duration::from_millis(100))
        .expect("timeout");

    let result = ui.with_tx(|tx| {
        // The read half of "mark this feed read".
        let unread: i64 = tx.query_row(
            "SELECT COUNT(*) FROM entries WHERE status = 'unread'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(unread, 5);

        // The background sync tries to commit right here. Under IMMEDIATE the
        // UI already holds the write lock, so this is the side that waits and
        // gives up -- which is the correct outcome: a background refresh can
        // be retried, a user's action cannot be invented again.
        let _ = timer.with_tx(|t| {
            t.execute("UPDATE entries SET title = 'pulled' WHERE id = 1", [])?;
            Ok(())
        });

        // The write half. This is what used to fail.
        outbox::queue_mark_feed_read(tx, 1, 100)
    });

    let queued = result.expect("the user's mark-feed-read must survive a concurrent sync");
    assert_eq!(queued, 5);
    assert_eq!(outbox::len(ui.conn()).expect("outbox"), 5);
    assert_eq!(store::unread_count(ui.conn()).expect("unread"), 0);
}

#[test]
fn writers_interleave_without_losing_either_side() {
    // Alternating writers, each through the real API, with no coordination
    // beyond SQLite's own locking.
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
    // With BEGIN IMMEDIATE the first writer holds the lock, so the second
    // blocks on busy_timeout instead of taking a snapshot it cannot upgrade.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mirror.sqlite");

    let mut ui = Database::open(&path).expect("ui");
    let mut timer = Database::open(&path).expect("timer");
    seed(&mut ui, 1);

    // Short enough that a regression fails fast instead of hanging, long
    // enough that "it waited" is unambiguous on a loaded machine.
    const BUSY: Duration = Duration::from_millis(300);
    timer.conn().busy_timeout(BUSY).expect("timeout");

    let held = ui
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .expect("begin");

    let started = Instant::now();
    let blocked = timer.with_tx(|tx| {
        // Read first. Without a read there is no snapshot to fail on, and a
        // DEFERRED transaction would simply block on the write lock like an
        // IMMEDIATE one -- so a write-only probe cannot tell the two apart at
        // all. Read-then-write is also what the real callers do ("mark this
        // feed read" reads the unread ids, then queues them).
        let _: i64 = tx.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
        tx.execute("UPDATE entries SET title = 'x' WHERE id = 1", [])?;
        Ok(())
    });
    let waited = started.elapsed();

    assert!(
        blocked.is_err(),
        "the second writer must wait, not proceed on a stale snapshot"
    );
    // `is_err()` alone does not separate the fix from the bug: BOTH produce an
    // error. Under BEGIN IMMEDIATE the loser blocks on busy_timeout and only
    // then gives up; under BEGIN DEFERRED it takes a snapshot, fails on the
    // write with SQLITE_BUSY_SNAPSHOT, and returns essentially instantly --
    // busy_timeout does not apply to that. The elapsed time is what tells them
    // apart, and asserting only `is_err()` left this test green under exactly
    // the defect the module header describes.
    assert!(
        waited >= BUSY - Duration::from_millis(50),
        "the second writer returned after {waited:?} without waiting out the {BUSY:?} \
         busy_timeout, which is the SQLITE_BUSY_SNAPSHOT signature of a DEFERRED \
         transaction rather than a writer politely queueing"
    );

    held.execute("UPDATE entries SET status = 'read' WHERE id = 1", [])
        .expect("the lock holder's write must succeed");
    held.commit().expect("commit");

    // And once the lock is free the other writer succeeds.
    timer
        .with_tx(|tx| {
            tx.execute("UPDATE entries SET title = 'x' WHERE id = 1", [])?;
            Ok(())
        })
        .expect("the second writer succeeds once the lock is released");
}

#[test]
fn an_opened_database_carries_a_busy_timeout() {
    // `Database::configure` calls the 5-second busy_timeout "the difference
    // between a working app and one that fails to launch when the timer
    // fires", and nothing tested it: setting it to 0 ms left the entire crate
    // suite green, because every test that needs a timeout sets its own.
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
    // Regression: `migrate` read `user_version` BEFORE opening its
    // transaction, so two connections opening together both saw version 0, both
    // ran migration 1, and the loser failed on "table already exists" against
    // a database that was in perfect shape. At start-up the UI thread and
    // the worker thread really do open it at the same moment.
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fresh.sqlite");

    // Several rounds, because the race is timing-dependent.
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
