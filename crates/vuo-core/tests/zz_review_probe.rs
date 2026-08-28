//! throwaway review probes
use vuo_core::db::Database;

/// Reproduce the real code path: the shim's mark-feed-read (read-then-write
/// deferred tx) racing a second connection's commit.
#[test]
fn probe_mark_feed_read_across_a_concurrent_writer() {
    use vuo_core::db::outbox;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.sqlite");
    let mut ui = Database::open(&path).unwrap();
    let mut timer = Database::open(&path).unwrap();

    ui.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1,'f')", [])?;
        for i in 1..=3 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        Ok(())
    })
    .unwrap();

    // Hand-inline `with_tx` so the concurrent commit can be injected mid-closure.
    let tx = ui.conn_mut().transaction().unwrap();
    let ids: Vec<i64> = {
        let mut s = tx
            .prepare("SELECT id FROM entries WHERE feed_id = ?1 AND status = 'unread'")
            .unwrap();
        let r = s.query_map([1i64], |r| r.get::<_, i64>(0)).unwrap();
        r.collect::<Result<Vec<_>, _>>().unwrap()
    };
    assert_eq!(ids.len(), 3);

    // the timer process commits a pull page here
    timer
        .with_tx(|tx| {
            tx.execute("UPDATE entries SET title = 'x' WHERE id = 1", [])?;
            Ok(())
        })
        .unwrap();

    let out = outbox::queue(
        &tx,
        vuo_core::model::EntryId(ids[0]),
        outbox::DesiredValue::Status(vuo_core::model::EntryStatus::Read),
        1,
    );
    println!("queue result: {out:?}");
    assert!(out.is_ok(), "the user's mark-read was lost: {out:?}");
}

/// The same sequence with BEGIN IMMEDIATE instead of BEGIN DEFERRED.
#[test]
fn probe_immediate_behaviour_would_survive() {
    use rusqlite::TransactionBehavior;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.sqlite");
    let mut ui = Database::open(&path).unwrap();
    let mut timer = Database::open(&path).unwrap();
    ui.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1,'f')", [])?;
        tx.execute("INSERT INTO entries (id, feed_id, status) VALUES (1,1,'unread')", [])?;
        Ok(())
    })
    .unwrap();

    let tx = ui
        .conn_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let _n: i64 = tx
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    // the other connection cannot commit while we hold the write lock, so it
    // is not possible to invalidate our snapshot; simulate by committing after.
    let r = tx.execute("UPDATE entries SET status='read' WHERE id=1", []);
    println!("immediate write: {r:?}");
    assert!(r.is_ok());
    tx.commit().unwrap();
    timer
        .with_tx(|tx| {
            tx.execute("UPDATE entries SET title='x' WHERE id=1", [])?;
            Ok(())
        })
        .unwrap();
}
