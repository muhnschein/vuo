//! The incremental pull: pagination, the cursor, and deletion detection.

// Test code: see the note in vuo-core's lib.rs. The unwrap/panic denials
// guard foreign-input paths in production, not assertions in tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod common;

use common::*;
use vuo_core::db::store;
use vuo_core::model::EntryId;
use vuo_core::sync::pull;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn pagination_uses_a_keyset_and_never_an_offset() {
    // The server's ORDER BY has no id tiebreaker, so offset paging silently
    // skips and duplicates rows. This asserts the shape of what we send.
    let server = MockServer::start().await;

    // Page 1: a full page of ids 1..=250.
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .and(query_param("order", "id"))
        .and(query_param("direction", "asc"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(entries_response(
                (1..=250)
                    .map(|i| entry_json(i, 1, "unread", false))
                    .collect(),
                600,
            )),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Page 2 must arrive with after_entry_id=250; a short page ends the pass.
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .and(query_param("after_entry_id", "250"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(entries_response(
                (251..=300)
                    .map(|i| entry_json(i, 1, "unread", false))
                    .collect(),
                350,
            )),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    let outcome = pull::entries(&mut db, &client, None, 1).await.unwrap();

    assert_eq!(outcome.pages, 2);
    assert_eq!(outcome.upserted, 300);

    for request in server.received_requests().await.unwrap() {
        let query = request.url.query().unwrap_or_default();
        assert!(
            !query.contains("offset="),
            "offset paging is unsafe here: {query}"
        );
    }
}

#[tokio::test]
async fn a_page_of_unusable_entries_does_not_end_the_pass() {
    // Paging must continue on the raw returned count, not on how many entries
    // survived validation -- otherwise one poisoned page truncates the sync.
    let server = MockServer::start().await;

    let mut poisoned: Vec<_> = (1..=250)
        .map(|i| entry_json(i, 1, "unread", false))
        .collect();
    for e in &mut poisoned {
        e["status"] = serde_json::Value::from("nonsense");
    }

    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(entries_response(poisoned, 300)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .and(query_param("after_entry_id", "250"))
        .respond_with(ResponseTemplate::new(200).set_body_json(entries_response(
            vec![entry_json(251, 1, "unread", false)],
            1,
        )))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    let outcome = pull::entries(&mut db, &client, None, 1).await.unwrap();

    assert_eq!(outcome.rejected, 250, "every entry on page 1 was unusable");
    assert_eq!(outcome.pages, 2, "but the pass continued past it");
    assert_eq!(outcome.upserted, 1);
}

#[tokio::test]
async fn the_cursor_comes_from_the_servers_clock_minus_a_skew() {
    // A phone's clock can be hours out; the comparison happens server-side.
    let server = MockServer::start().await;
    // Fri, 02 Jan 2026 03:04:05 GMT == 1767323045
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Fri, 02 Jan 2026 03:04:05 GMT")
                .set_body_json(entries_response(vec![], 0)),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    let outcome = pull::entries(&mut db, &client, None, 1).await.unwrap();

    assert_eq!(
        outcome.next_cursor,
        Some(1_767_323_045 - pull::CURSOR_SKEW_SECS),
        "the cursor must trail the server's clock by the skew"
    );
}

#[tokio::test]
async fn an_unset_cursor_is_omitted_rather_than_sent_as_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(entries_response(vec![], 0)))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    pull::entries(&mut db, &client, None, 1).await.unwrap();

    for request in server.received_requests().await.unwrap() {
        let query = request.url.query().unwrap_or_default();
        assert!(
            !query.contains("changed_after"),
            "changed_after=0 would mean 'everything since 1970': {query}"
        );
    }
}

#[tokio::test]
async fn unsubscribing_a_feed_removes_its_entries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![category_json(1, "News")]))
        .mount(&server)
        .await;
    // The server now lists only feed 1; feed 2 was unsubscribed elsewhere.
    Mock::given(method("GET"))
        .and(path("/v1/feeds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![feed_json(1, "Kept", 1)]))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (2, 'Gone')", [])?;
        tx.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (99, 2, 'unread')",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    pull::taxonomy(&mut db, &client, 1).await.unwrap();

    assert!(
        store::entry(db.conn(), EntryId(99)).unwrap().is_none(),
        "an unsubscribed feed's entries are unreachable and must not accumulate"
    );
    assert_eq!(store::feeds(db.conn()).unwrap().len(), 1);
}

#[tokio::test]
async fn a_torn_id_listing_aborts_the_reconcile_instead_of_deleting() {
    // /v1/entries/ids pages by offset over an id DESC ordering, which is not
    // stable under concurrent writes. Acting on a torn listing would delete
    // live entries, so the total is checked and a mismatch aborts.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            // Claims 5 ids exist but returns 3: the window shifted mid-listing.
            "total": 5,
            "entry_ids": [3, 2, 1]
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        for i in 1..=5 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        Ok(())
    })
    .unwrap();

    let deleted = pull::reconcile(&mut db, &client).await.unwrap();
    assert_eq!(deleted, 0, "a torn listing must never drive deletions");
    assert_eq!(store::local_entry_ids(db.conn()).unwrap().len(), 5);
}

#[tokio::test]
async fn a_consistent_id_listing_deletes_what_the_server_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total": 3,
            "entry_ids": [5, 3, 1]
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        for i in 1..=5 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        // Entry 2 has a pending intent; deleting the entry must take it too,
        // since Miniflux silently ignores unknown ids and the row would
        // otherwise never clear.
        tx.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (2,'status','read',1)",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let deleted = pull::reconcile(&mut db, &client).await.unwrap();
    assert_eq!(deleted, 2, "entries 2 and 4 are gone server-side");

    let remaining = store::local_entry_ids(db.conn()).unwrap();
    let mut ids: Vec<i64> = remaining.iter().map(|i| i.get()).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 3, 5]);
    assert_eq!(
        vuo_core::db::outbox::len(db.conn()).unwrap(),
        0,
        "an intent for a deleted entry can never be confirmed"
    );
}

#[tokio::test]
async fn the_counters_check_finds_diverging_feeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds/counters"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reads":   { "1": 2 },
            "unreads": { "1": 1, "2": 4 }
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute(
            "INSERT INTO feeds (id, title) VALUES (1, 'a'), (2, 'b')",
            [],
        )?;
        // Feed 1 agrees (3 local, 2+1 server). Feed 2 does not (1 local, 4 server).
        for (id, feed) in [(1i64, 1i64), (2, 1), (3, 1), (4, 2)] {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, ?2, 'unread')",
                [id, feed],
            )?;
        }
        Ok(())
    })
    .unwrap();

    let diverging = pull::diverging_feeds(&db, &client).await.unwrap();
    assert_eq!(
        diverging,
        vec![2],
        "only the feed whose counts disagree needs work"
    );
}
