//! The incremental pull: pagination, the cursor, and deletion detection.

// Test code: see note in vuo-core's lib.rs. unwrap/panic denials guard
// production foreign-input paths, not test assertions.
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
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn pagination_uses_a_keyset_and_never_an_offset() {
    // Server's ORDER BY lacks id tiebreaker: offset paging silently skips and
    // duplicates. Asserts shape of what we send.
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

    // Page 2 must carry after_entry_id=250; short page ends pass.
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
    // Paging continues on raw returned count, not validated survivors: else one
    // poisoned page truncates sync.
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
    // Phone clock can be hours out; comparison server-side.
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
async fn the_cursor_anchors_to_the_first_pages_clock_not_the_last() {
    // Multi-page pull takes minutes. Anchoring to LAST page's Date marks "seen"
    // a window ending after mutations pass couldn't read: every entry changed
    // during pass with id ≤ last read skipped permanently.
    //
    // One page can't show this (first = last), so single-page test above passes
    // with rule inverted. Two pages, clocks five minutes apart.
    let server = MockServer::start().await;
    // Fri, 02 Jan 2026 03:04:05 GMT == 1767323045
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .and(query_param("after_entry_id", "250"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Fri, 02 Jan 2026 03:09:05 GMT")
                .set_body_json(entries_response(vec![], 0)),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Fri, 02 Jan 2026 03:04:05 GMT")
                .set_body_json(entries_response(
                    (1..=250)
                        .map(|i| entry_json(i, 1, "unread", false))
                        .collect(),
                    250,
                )),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        Ok(())
    })
    .unwrap();

    let outcome = pull::entries(&mut db, &client, None, 1).await.unwrap();
    assert_eq!(outcome.pages, 2, "the fixture must actually take two pages");
    assert_eq!(
        outcome.next_cursor,
        Some(1_767_323_045 - pull::CURSOR_SKEW_SECS),
        "the FIRST page's clock (03:04:05), not the second's (03:09:05)"
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
    // Server lists only feed 1; feed 2 unsubscribed elsewhere.
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
    // /v1/entries/ids pages by offset over id DESC, unstable under concurrent
    // writes. Torn listing would delete live entries: total checked, mismatch
    // aborts.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            // Claims 5 ids, returns 3: window shifted mid-listing.
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

    let outcome = pull::reconcile(&mut db, &client).await.unwrap();
    assert_eq!(
        outcome.deleted, 0,
        "a torn listing must never drive deletions"
    );
    assert!(
        !outcome.completed,
        "an aborted reconcile must not count as having run"
    );
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
        // Entry 2 has pending intent; deleting entry takes it too: Miniflux
        // silently ignores unknown ids, row would never clear.
        tx.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (2,'status','read',1)",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let outcome = pull::reconcile(&mut db, &client).await.unwrap();
    assert!(outcome.completed);
    assert_eq!(outcome.deleted, 2, "entries 2 and 4 are gone server-side");

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
            // Feed 3 has local entries, absent here = zero on server. Feed 4
            // agrees at zero both sides.
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute(
            "INSERT INTO feeds (id, title) VALUES (1, 'a'), (2, 'b'), (3, 'c')",
            [],
        )?;
        // Feed 1 agrees (3 local, 2+1 server).
        // Feed 2 has FEWER locally than the server (1 local, 4 server).
        // Feed 3 has MORE locally than the server (2 local, absent = 0).
        for (id, feed) in [(1i64, 1i64), (2, 1), (3, 1), (4, 2), (5, 3), (6, 3)] {
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
        vec![3],
        "divergence is the mirror holding MORE than the server, and only that. \
         The check's one consumer is the reconcile, and the reconcile only \
         ever deletes -- so feed 3, where an entry has gone from the server, \
         is the case it is for. Feed 2, where the server has entries the \
         mirror has not pulled yet, is the cursor's job; raising it here \
         bought a full id listing of the whole corpus that could not have \
         fixed it. That mattered the moment a mirror could deliberately hold \
         less than the server: with a retention policy set, EVERY feed is \
         permanently short, so `!=` meant paging the entire corpus on every \
         single pass forever."
    );
}

#[tokio::test]
async fn a_negative_total_aborts_rather_than_disabling_the_guard() {
    // Missing or negative total must abort. Guard written `total >= 0 &&
    // collected != total` lets NEGATIVE total skip check: server (hostile,
    // buggy, cached proxy body) answering {"total": -1, "entry_ids": []}
    // deletes whole mirror and every pending outbox row.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total": -1,
            "entry_ids": []
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
        tx.execute(
            "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (2,'starred','true',1)",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let outcome = pull::reconcile(&mut db, &client).await.unwrap();
    assert!(!outcome.completed);
    assert_eq!(outcome.deleted, 0);
    assert_eq!(
        store::local_entry_ids(db.conn()).unwrap().len(),
        5,
        "the mirror survived"
    );
    assert_eq!(
        vuo_core::db::outbox::len(db.conn()).unwrap(),
        1,
        "pending intent survived"
    );
}

#[tokio::test]
async fn the_reconcile_pages_beyond_the_servers_limit_cap() {
    // PAGE must equal server's 1000 cap (EntriesQuery clamps). Larger PAGE
    // makes "short page = done" fire on first page: reconcile never pages,
    // corpus over 1000 fails total check every run.
    let server = MockServer::start().await;
    let first: Vec<i64> = (1..=1000).rev().collect();
    let second: Vec<i64> = (1001..=1500).rev().collect();

    // `limit` asserted, not just `offset`. Else stub answers 1000 ids whatever
    // asked, hiding a limit SMALLER than loop's PAGE ("short page = done" on
    // page one). §8.3: mock is contract, request is half.
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .and(query_param("offset", "0"))
        .and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total": 1500, "entry_ids": first
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .and(query_param("offset", "1000"))
        .and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total": 1500, "entry_ids": second
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        // 1500 server still has, plus one it doesn't.
        for i in 1..=1501i64 {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status) VALUES (?1, 1, 'unread')",
                [i],
            )?;
        }
        Ok(())
    })
    .unwrap();

    let outcome = pull::reconcile(&mut db, &client).await.unwrap();
    assert!(
        outcome.completed,
        "the reconcile must page rather than abort on a large corpus"
    );
    assert_eq!(outcome.deleted, 1, "only entry 1501 is gone server-side");
    assert_eq!(store::local_entry_ids(db.conn()).unwrap().len(), 1500);
}

#[tokio::test]
async fn stopping_at_the_page_cap_does_not_advance_the_cursor() {
    // PAGE CAP branch, reached for real.
    //
    // Pages never stall, so pass exits through cap, not after_entry_id stall
    // check (covered separately below). Pins cap holding cursor back.
    let server = MockServer::start().await;
    // Every page full, last id keeps advancing: only cap stops pass.
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(|req: &Request| {
            let after: i64 = req
                .url
                .query_pairs()
                .find(
                    |(k, _): &(std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)| {
                        k == "after_entry_id"
                    },
                )
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            ResponseTemplate::new(200)
                .insert_header("Date", "Fri, 02 Jan 2026 03:04:05 GMT")
                .set_body_json(entries_response(
                    (after + 1..=after + 250)
                        .map(|i| entry_json(i, 1, "unread", false))
                        .collect(),
                    1_000_000,
                ))
        })
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'f')", [])?;
        Ok(())
    })
    .unwrap();

    let outcome = pull::entries_with_page_cap(&mut db, &client, Some(1000), 1, 3)
        .await
        .unwrap();

    assert_eq!(outcome.pages, 3, "the pass must stop AT the cap");
    assert_eq!(
        outcome.next_cursor, None,
        "a pass that stopped at the page cap must leave the cursor alone; \
         advancing it marks as seen a window it never finished reading"
    );
}

#[tokio::test]
async fn a_server_whose_keyset_never_advances_does_not_advance_the_cursor() {
    // Pass bailing early returns no cursor; advanced cursor marks unfinished
    // window "seen", skipping everything beyond stop point forever.
    let server = MockServer::start().await;
    // Server whose after_entry_id never advances: every page full, ends on same
    // id.
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Date", "Fri, 02 Jan 2026 03:04:05 GMT")
                .set_body_json(entries_response(
                    (1..=250)
                        .map(|i| entry_json(i, 1, "unread", false))
                        .collect(),
                    10_000,
                )),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    let outcome = pull::entries(&mut db, &client, Some(1000), 1)
        .await
        .unwrap();
    assert_eq!(
        outcome.next_cursor, None,
        "an incomplete pass must leave the cursor alone; advancing it loses entries"
    );
}

#[tokio::test]
async fn hostile_feed_counters_do_not_panic() {
    // §9.2: server numbers chosen by someone else. Two i64::MAX counts for one
    // feed must not overflow divergence accumulator: reachable panic on foreign
    // input, §9.5 forbids (unwinding into Qt C++ = UB).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds/counters"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reads":   { "1": i64::MAX },
            "unreads": { "1": i64::MAX }
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        // Feed 1 takes overflowing counts. Feed 2 absent from response (zero
        // there, one here): call must still produce real answer after surviving
        // accumulator, not pass by returning nothing.
        tx.execute(
            "INSERT INTO feeds (id, title) VALUES (1, 'f'), (2, 'g')",
            [],
        )?;
        tx.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (1, 1, 'unread'), (2, 2, 'unread')",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let diverging = pull::diverging_feeds(&db, &client).await.unwrap();
    assert_eq!(
        diverging,
        vec![2],
        "the absurd count saturates instead of panicking, and the feed that \
         really is ahead of the server is still found"
    );
}

#[tokio::test]
async fn an_empty_feed_listing_is_not_treated_as_a_mass_unsubscribe() {
    // Proxy serving stale cached `[]` would delete whole mirror. Unsubscribing
    // everything at once not real workflow; cached empty body is real failure.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![category_json(1, "News")]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute(
            "INSERT INTO feeds (id, title) VALUES (1, 'kept'), (2, 'kept too')",
            [],
        )?;
        tx.execute(
            "INSERT INTO entries (id, feed_id, status) VALUES (1, 1, 'unread')",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    pull::taxonomy(&mut db, &client, 1).await.unwrap();

    assert_eq!(
        store::feeds(db.conn()).unwrap().len(),
        2,
        "the mirror survived an empty listing"
    );
    assert!(store::entry(db.conn(), EntryId(1)).unwrap().is_some());
}

#[tokio::test]
async fn unsubscribing_a_feed_takes_its_queued_intents_with_it() {
    // Intent for entry server lacks never confirms: Miniflux ignores unknown
    // ids, flush gets 204, `confirm` matches nothing. Row sits forever in
    // "changes waiting to be sent".
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![category_json(1, "News")]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![feed_json(1, "Kept", 1)]))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'kept'), (7, 'going')", [])?;
        for i in 1..=3i64 {
            tx.execute("INSERT INTO entries (id, feed_id, status) VALUES (?1, 7, 'unread')", [i])?;
            tx.execute(
                "INSERT INTO outbox (entry_id, field, value, queued_at) VALUES (?1,'status','read',1)",
                [i],
            )?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(vuo_core::db::outbox::len(db.conn()).unwrap(), 3);

    pull::taxonomy(&mut db, &client, 1).await.unwrap();

    assert_eq!(
        vuo_core::db::outbox::len(db.conn()).unwrap(),
        0,
        "intents for a removed feed's entries can never be confirmed"
    );
}

/// When `entry_json` says the server created every entry: 2026-01-02T03:04:05Z.
const FIXTURE_CREATED_AT: i64 = 1_767_323_045;

/// Serve exactly one page for next pull, replacing earlier mocks.
async fn serve_entries(server: &MockServer, entries: Vec<serde_json::Value>) {
    server.reset().await;
    let total = i64::try_from(entries.len()).unwrap();
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(entries_response(entries, total)))
        .mount(server)
        .await;
}

fn arrived(db: &vuo_core::db::Database) -> Vec<i64> {
    let mut ids: Vec<i64> = store::arrivals(db.conn())
        .unwrap()
        .iter()
        .map(|id| id.get())
        .collect();
    ids.sort_unstable();
    ids
}

/// §what the new-articles notification counts, decided where the rows land.
///
/// Four ways pulled entry isn't news, one way it is:
///
/// - mirror's FIRST pass (whole account);
/// - entry mirror already holds (cursor overlap re-reads every pass);
/// - entry arriving already read;
/// - OLD entry re-entering (pruned by retention, then marked unread on web):
///   insert, not arrival.
#[tokio::test]
async fn only_an_unread_entry_new_to_the_mirror_and_to_the_window_is_an_arrival() {
    let server = MockServer::start().await;
    let client = client_for(&server);
    let mut db = memory_db();

    // First pass: all inserts, none news.
    serve_entries(
        &server,
        vec![
            entry_json(1, 1, "unread", false),
            entry_json(2, 1, "read", false),
        ],
    )
    .await;
    pull::entries(&mut db, &client, None, 1).await.unwrap();
    assert_eq!(
        arrived(&db),
        Vec::<i64>::new(),
        "a first sync announces the whole account as new"
    );

    // Later pass whose window opened hour before fixture entries created.
    let mut returning = entry_json(5, 1, "unread", false);
    returning["created_at"] = serde_json::Value::from("2025-06-01T00:00:00Z");
    serve_entries(
        &server,
        vec![
            entry_json(1, 1, "unread", false), // already held
            entry_json(3, 1, "unread", false), // new, unread: news
            entry_json(4, 1, "read", false),   // new, but read elsewhere
            returning,                         // new to the mirror, not to the server
        ],
    )
    .await;
    pull::entries(&mut db, &client, Some(FIXTURE_CREATED_AT - 3600), 2)
        .await
        .unwrap();

    assert_eq!(arrived(&db), vec![3]);
    assert_eq!(
        store::local_entry_ids(db.conn()).unwrap().len(),
        5,
        "every entry was still written; only the flag is selective"
    );
}

/// §a re-read row keeps its flag, in both directions.
///
/// Cursor re-reads minute of overlap every pass. Arrival not yet told must
/// survive being seen again; one already told mustn't return as news when
/// overlap brings it round.
#[tokio::test]
async fn re_reading_an_entry_neither_raises_nor_clears_its_arrival() {
    let server = MockServer::start().await;
    let client = client_for(&server);
    let mut db = memory_db();
    let window = Some(FIXTURE_CREATED_AT - 3600);

    serve_entries(
        &server,
        vec![
            entry_json(7, 1, "unread", false),
            entry_json(8, 1, "unread", false),
        ],
    )
    .await;
    pull::entries(&mut db, &client, window, 1).await.unwrap();
    assert_eq!(arrived(&db), vec![7, 8]);

    // Reader dismisses notification covering 8.
    db.with_tx(|tx| store::acknowledge_arrivals(tx, &[EntryId(8)]))
        .unwrap();

    // Same two again, in next pass's overlap.
    pull::entries(&mut db, &client, window, 2).await.unwrap();
    assert_eq!(
        arrived(&db),
        vec![7],
        "7 is still unannounced news, and 8 is not news any more"
    );
}

/// §an arrival is recorded with its row, not with the pass's report.
///
/// Pass commits page by page, can fail after writing: count in pass result lost
/// with result, counted rows already held (not new) by next pass. Hence flag =
/// column.
#[tokio::test]
async fn a_pass_that_fails_after_its_first_page_keeps_that_pages_arrivals() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(entries_response(
                (1..=250)
                    .map(|i| entry_json(i, 1, "unread", false))
                    .collect(),
                300,
            )),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .and(query_param("after_entry_id", "250"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut db = memory_db();
    let result = pull::entries(&mut db, &client, Some(FIXTURE_CREATED_AT - 3600), 1).await;

    assert!(result.is_err(), "the second page failed");
    assert_eq!(
        store::arrivals(db.conn()).unwrap().len(),
        250,
        "the first page's arrivals were committed with its rows"
    );
}
