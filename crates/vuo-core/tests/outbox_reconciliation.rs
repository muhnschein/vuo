//! Outbox reconciliation, tested deterministically rather than incidentally.
//!
//! §8.3 names four properties, and each has a test here:
//!
//! - replay is idempotent;
//! - a process killed mid-flight resumes without losing or double-applying;
//! - an offline burst of marks reconciles correctly on reconnect;
//! - a server-side change to an entry mutated locally resolves by a stated rule.
//!
//! The stated rule, for the last one, is **local intent wins per field**. See
//! `db::store` for why it is per field and not per entry.

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
use serde_json::Value;
use vuo_core::db::outbox::{self, DesiredValue};
use vuo_core::db::{store, Database};
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::sync::replay;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Seed a mirror with `n` unread, unstarred entries on feed 1.
fn seeded_db(n: i64) -> Database {
    let mut db = memory_db();
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'Feed')", [])?;
        for i in 1..=n {
            tx.execute(
                "INSERT INTO entries (id, feed_id, status, starred, title) VALUES (?1, 1, 'unread', 0, 'e')",
                [i],
            )?;
        }
        Ok(())
    })
    .unwrap();
    db
}

/// Every `PUT /v1/entries` body the mock server received.
fn update_bodies(requests: &[Request]) -> Vec<Value> {
    requests
        .iter()
        .filter(|r| r.method == wiremock::http::Method::PUT && r.url.path() == "/v1/entries")
        .filter_map(|r| serde_json::from_slice(&r.body).ok())
        .collect()
}

#[tokio::test]
async fn replay_is_idempotent() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(3);

    db.with_tx(|tx| {
        for id in 1..=3 {
            outbox::queue(
                tx,
                EntryId(id),
                DesiredValue::Status(EntryStatus::Read),
                100,
            )?;
        }
        Ok(())
    })
    .unwrap();

    let first = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(first.confirmed, 3);
    assert_eq!(outbox::len(db.conn()).unwrap(), 0);

    // A second flush has nothing to send, and sends nothing.
    let second = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(second.confirmed, 0);

    let bodies = update_bodies(&server.received_requests().await.unwrap());
    assert_eq!(bodies.len(), 1, "the second flush must not re-send");
    // What went on the wire is an absolute value, which is what makes
    // resending it safe in the first place.
    assert_eq!(
        bodies.first().and_then(|b| b.get("status")),
        Some(&Value::from("read"))
    );
}

#[tokio::test]
async fn a_process_killed_mid_flight_resumes_without_losing_or_double_applying() {
    // The server applies the change, then the process dies before it can
    // record the confirmation. The intent must still be queued, and resending
    // it must be harmless.
    //
    // The crash is simulated OUTSIDE `flush` -- the batch is sent by hand --
    // so what this test actually pins is the idempotency of a resend: the same
    // absolute value, twice, is a no-op. It deliberately does not carry the two
    // neighbouring properties, which have their own tests because this shape
    // cannot see them:
    //
    //   - that `flush` sends BEFORE it clears the row, so a failure mid-request
    //     does not lose the intent -- `a_transient_failure_keeps_the_intent_queued` and
    //     `a_misconfigured_server_url_never_discards_queued_work`;
    //   - that a re-toggle during the in-flight window survives, which is why
    //     `confirm` compares before deleting -- `a_retoggle_during_the_request_
    //     is_not_lost`.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1);

    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
        .unwrap();

    // Simulate the crash: send the batch by hand, and never confirm it.
    let batch = outbox::batches(&outbox::pending(db.conn()).unwrap());
    let sent = batch.first().cloned().expect("one batch");
    client
        .update_entries(&sent.entry_ids, vuo_core::api::EntryMutation::Starred(true))
        .await
        .unwrap();

    assert_eq!(
        outbox::len(db.conn()).unwrap(),
        1,
        "an unconfirmed intent must survive the crash"
    );

    // Restart: the next flush resends and then confirms.
    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(outcome.confirmed, 1);
    assert_eq!(outbox::len(db.conn()).unwrap(), 0);

    let bodies = update_bodies(&server.received_requests().await.unwrap());
    assert_eq!(bodies.len(), 2, "the same absolute value was sent twice");
    assert_eq!(
        bodies.first(),
        bodies.get(1),
        "and both times it was identical"
    );
    // Double-applying an absolute set is a no-op, which is the whole point:
    // had this been the server's toggle endpoint, the star would now be off.
    assert_eq!(
        bodies.first().and_then(|b| b.get("starred")),
        Some(&Value::from(true))
    );
}

#[tokio::test]
async fn an_offline_burst_reconciles_on_reconnect() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1200);

    // Offline: the user reads everything and stars a few, toggling some of
    // them repeatedly the way a real person does.
    db.with_tx(|tx| {
        for id in 1..=1200 {
            outbox::queue(tx, EntryId(id), DesiredValue::Status(EntryStatus::Read), 10)?;
        }
        for id in [7i64, 42, 99] {
            outbox::queue(tx, EntryId(id), DesiredValue::Starred(true), 11)?;
            outbox::queue(tx, EntryId(id), DesiredValue::Starred(false), 12)?;
            outbox::queue(tx, EntryId(id), DesiredValue::Starred(true), 13)?;
        }
        Ok(())
    })
    .unwrap();

    assert_eq!(
        outbox::len(db.conn()).unwrap(),
        1203,
        "toggles collapse: 1200 statuses + 3 stars, not 1209"
    );

    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(outcome.confirmed, 1203);
    assert_eq!(outbox::len(db.conn()).unwrap(), 0);

    let bodies = update_bodies(&server.received_requests().await.unwrap());
    assert_eq!(
        bodies.len(),
        4,
        "1200 marks chunk into 3 requests, plus 1 for the stars"
    );

    // Every id appears exactly once across the whole flush.
    let mut seen: Vec<i64> = Vec::new();
    for body in &bodies {
        if let Some(ids) = body.get("entry_ids").and_then(|v| v.as_array()) {
            seen.extend(ids.iter().filter_map(serde_json::Value::as_i64));
        }
    }
    assert_eq!(seen.len(), 1203);

    // And the stars settled on the final value the user chose, not an
    // intermediate one.
    let star_body = bodies
        .iter()
        .find(|b| b.get("starred").is_some())
        .expect("a star request");
    assert_eq!(star_body.get("starred"), Some(&Value::from(true)));
}

#[tokio::test]
async fn a_server_change_to_a_locally_mutated_entry_resolves_by_the_stated_rule() {
    // The rule: local intent wins, PER FIELD.
    //
    // The user stars entry 1 offline. Meanwhile the server (another device)
    // marks it read. The pull must accept the remote read status while
    // preserving the local pending star -- resolving per entry would drop one
    // or the other.
    let mut db = seeded_db(1);
    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
        .unwrap();

    let remote = vuo_core::api::convert::entry(
        serde_json::from_value(entry_json(1, 1, "read", false)).unwrap(),
    )
    .unwrap();
    db.with_tx(|tx| store::upsert_entry(tx, &remote, 1))
        .unwrap();

    let stored = store::entry(db.conn(), EntryId(1)).unwrap().expect("entry");
    assert_eq!(
        stored.status,
        EntryStatus::Read,
        "the remote status change is accepted"
    );
    assert!(
        stored.starred,
        "but the local pending star is not clobbered"
    );
    assert_eq!(
        outbox::len(db.conn()).unwrap(),
        1,
        "and it is still queued to send"
    );
}

#[tokio::test]
async fn the_servers_toggle_endpoints_are_never_called() {
    // PUT /v1/entries/{id}/star is `SET starred = NOT starred`. Replaying one
    // flips the value back, so it must never appear in a replay path. This is
    // the single most important thing to get wrong, so it gets its own test.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(2);

    db.with_tx(|tx| {
        outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1)?;
        outbox::queue(tx, EntryId(2), DesiredValue::Starred(false), 1)
    })
    .unwrap();
    replay::flush(&mut db, &client).await.unwrap();

    for request in server.received_requests().await.unwrap() {
        let path = request.url.path();
        assert!(
            !path.ends_with("/star"),
            "the toggle endpoint was called: {path}"
        );
        assert!(
            !path.ends_with("/bookmark"),
            "the toggle endpoint was called: {path}"
        );
    }
}

#[tokio::test]
async fn a_transient_failure_keeps_the_intent_queued() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1);

    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 1))
        .unwrap();

    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(outcome.confirmed, 0);
    assert_eq!(outcome.deferred, 1);
    assert_eq!(
        outbox::len(db.conn()).unwrap(),
        1,
        "a 503 must never lose the user's action"
    );
    assert_eq!(
        outbox::pending(db.conn())
            .unwrap()
            .first()
            .map(|m| m.attempts),
        Some(1)
    );
}

#[tokio::test]
async fn a_misconfigured_server_url_never_discards_queued_work() {
    // THE regression, driven end to end through `flush`.
    //
    // `is_transient()` is false for a policy-refused redirect, and the flush
    // used to drop anything that was not transient -- so pointing Vuo at a URL
    // that redirects off-origin (a typo, a moved instance, a captive portal)
    // silently destroyed every queued mark and star.
    //
    // The unit tests in sync::replay cannot protect this. They assert on
    // `would_discard`, a pure predicate, and a predicate cannot see its caller
    // change: restoring the exact historical bug in `flush` leaves all nine of
    // them green. Only a real failure through `flush` distinguishes them.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "https://elsewhere.invalid/v1/entries"),
        )
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(3);

    db.with_tx(|tx| {
        for id in 1..=3 {
            outbox::queue(tx, EntryId(id), DesiredValue::Status(EntryStatus::Read), 1)?;
        }
        Ok(())
    })
    .unwrap();

    let outcome = replay::flush(&mut db, &client).await.unwrap();

    assert_eq!(
        outcome.dropped, 0,
        "a cross-origin redirect means the SERVER is misconfigured, not that the \
         user's marks are invalid. Dropping them is unrecoverable data loss."
    );
    assert_eq!(
        outbox::len(db.conn()).unwrap(),
        3,
        "every queued action must still be there, waiting for a human to fix the URL"
    );
    assert_eq!(
        outbox::pending(db.conn())
            .unwrap()
            .first()
            .map(|m| m.attempts),
        Some(1),
        "and the attempt must be recorded, so the UI can say why nothing is syncing"
    );
}

#[tokio::test]
async fn revoked_credentials_stop_the_flush_without_dropping_work() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error_message": "access unauthorized"
        })))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1000);

    db.with_tx(|tx| {
        for id in 1..=1000 {
            outbox::queue(tx, EntryId(id), DesiredValue::Status(EntryStatus::Read), 1)?;
        }
        Ok(())
    })
    .unwrap();

    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert!(outcome.auth_failed);
    assert_eq!(
        outbox::len(db.conn()).unwrap(),
        1000,
        "nothing may be dropped"
    );

    // It stopped after the first batch rather than hammering the server with
    // every remaining chunk.
    let sent = update_bodies(&server.received_requests().await.unwrap());
    assert_eq!(sent.len(), 1, "every later batch would fail identically");
}

#[tokio::test]
async fn a_permanent_rejection_is_dropped_rather_than_blocking_the_queue() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error_message": "invalid entry status"
        })))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1);

    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 1))
        .unwrap();

    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(outcome.dropped, 1);
    assert_eq!(
        outbox::len(db.conn()).unwrap(),
        0,
        "retrying a 400 forever would block every later intent behind it"
    );
}

#[tokio::test]
async fn a_retoggle_during_the_request_is_not_lost() {
    // The compare-and-delete case, end to end.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1);

    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
        .unwrap();
    let batch = outbox::batches(&outbox::pending(db.conn()).unwrap());
    let sent = batch.first().cloned().unwrap();

    // The request goes out...
    client
        .update_entries(&sent.entry_ids, vuo_core::api::EntryMutation::Starred(true))
        .await
        .unwrap();
    // ...and while it is in flight the user changes their mind.
    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(false), 2))
        .unwrap();
    // ...and only then does the confirmation land.
    let cleared = db.with_tx(|tx| outbox::confirm(tx, &sent)).unwrap();

    assert_eq!(
        cleared, 0,
        "the queued value is no longer the one that was sent"
    );
    assert_eq!(
        outbox::pending(db.conn()).unwrap().first().map(|m| m.value),
        Some(DesiredValue::Starred(false)),
        "the newer intent must survive to be sent"
    );
}
