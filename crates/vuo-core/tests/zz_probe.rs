#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod common;
use common::*;
use vuo_core::db::store;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// PROBE 1: reconcile against a corpus larger than the server's 1000 limit cap.
#[tokio::test]
async fn probe_reconcile_pages_past_one_thousand() {
    let server = MockServer::start().await;
    // The server holds ids 1..=1500. It honours limit (clamped to 1000 by
    // EntriesQuery) and offset, id DESC.
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .respond_with(|req: &wiremock::Request| {
            let q: std::collections::HashMap<_, _> = req.url.query_pairs().into_owned().collect();
            let limit: usize = q.get("limit").unwrap().parse().unwrap();
            let offset: usize = q.get("offset").map(|s| s.parse().unwrap()).unwrap_or(0);
            let all: Vec<i64> = (1..=1500i64).rev().collect();
            let page: Vec<i64> = all.into_iter().skip(offset).take(limit).collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 1500, "entry_ids": page
            }))
        })
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        for i in 1..=1600i64 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        Ok(())
    })
    .unwrap();

    let deleted = pull_reconcile(&mut db, &client).await;
    let reqs = server.received_requests().await.unwrap();
    eprintln!("PROBE1 requests={} deleted={}", reqs.len(), deleted);
    for r in &reqs {
        eprintln!("  {}", r.url.query().unwrap_or(""));
    }
    eprintln!(
        "PROBE1 local remaining = {}",
        store::local_entry_ids(db.conn()).unwrap().len()
    );
}

async fn pull_reconcile(
    db: &mut vuo_core::db::Database,
    c: &vuo_core::api::MinifluxClient,
) -> usize {
    vuo_core::sync::pull::reconcile(db, c).await.unwrap()
}

// PROBE 2: a negative `total` bypasses the torn-listing guard.
#[tokio::test]
async fn probe_negative_total_bypasses_the_guard() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total": -1, "entry_ids": []
        })))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        for i in 1..=5i64 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        tx.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (2,'starred','true',1)",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let deleted = pull_reconcile(&mut db, &client).await;
    eprintln!(
        "PROBE2 deleted={} remaining={} outbox={}",
        deleted,
        store::local_entry_ids(db.conn()).unwrap().len(),
        vuo_core::db::outbox::len(db.conn()).unwrap()
    );
}

// PROBE 3: the page cap terminates the pull but the cursor still advances.
#[tokio::test]
async fn probe_page_cap_still_advances_the_cursor() {
    let server = MockServer::start().await;
    // A server whose ids are not ascending despite order=id: every page comes
    // back with the same last id, so after_entry_id never advances.
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Fri, 02 Jan 2026 03:04:05 GMT")
                .set_body_json(entries_response(
                    (1..=250)
                        .map(|i| entry_json(i, 1, "unread", false))
                        .collect(),
                    100_000,
                )),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    let outcome = vuo_core::sync::pull::entries(&mut db, &client, Some(1_000_000), 1)
        .await
        .unwrap();
    eprintln!(
        "PROBE3 pages={} upserted={} next_cursor={:?}",
        outcome.pages, outcome.upserted, outcome.next_cursor
    );
}

// PROBE 4: delete_feed leaves outbox rows behind.
#[test]
fn probe_delete_feed_leaves_the_outbox() {
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        tx.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (7, 1, 'unread')",
            [],
        )?;
        tx.execute(
            "INSERT INTO enclosures (id, entry_id, url) VALUES (1, 7, 'https://x.example/a')",
            [],
        )?;
        tx.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (7,'status','read',1)",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    db.with_tx(|tx| store::delete_feed(tx, vuo_core::model::FeedId(1)))
        .unwrap();
    let enc: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM enclosures", [], |r| r.get(0))
        .unwrap();
    eprintln!(
        "PROBE4 outbox={} enclosures={}",
        vuo_core::db::outbox::len(db.conn()).unwrap(),
        enc
    );
}

// PROBE 5: taxonomy with an empty feed list wipes the mirror.
#[tokio::test]
async fn probe_empty_feed_list_wipes_everything() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        for i in 1..=3i64 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        tx.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (2,'starred','true',1)",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    vuo_core::sync::pull::taxonomy(&mut db, &client, 1)
        .await
        .unwrap();
    eprintln!(
        "PROBE5 entries={} feeds={} outbox={}",
        store::local_entry_ids(db.conn()).unwrap().len(),
        store::feeds(db.conn()).unwrap().len(),
        vuo_core::db::outbox::len(db.conn()).unwrap()
    );
}

// PROBE 6: a pre-2.3 soft delete (status=removed) is dropped, not applied.
#[tokio::test]
async fn probe_removed_status_leaves_a_stale_row() {
    let server = MockServer::start().await;
    let mut e = entry_json(7, 1, "removed", false);
    e["status"] = serde_json::Value::from("removed");
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(entries_response(vec![e], 1)))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        tx.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (7, 1, 'unread')",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let out = vuo_core::sync::pull::entries(&mut db, &client, Some(1000), 1)
        .await
        .unwrap();
    eprintln!(
        "PROBE6 rejected={} entry_still_present={}",
        out.rejected,
        store::entry(db.conn(), vuo_core::model::EntryId(7))
            .unwrap()
            .is_some()
    );
}
