//! Outbox reconciliation, tested deterministically, not incidentally.
//!
//! §8.3 names four properties, each tested here:
//!
//! - replay idempotent;
//! - process killed mid-flight resumes without losing or double-applying;
//! - offline burst of marks reconciles correctly on reconnect;
//! - server-side change to locally mutated entry resolves by stated rule.
//!
//! Rule: **local intent wins per field**. Why per field: `db::store`.

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
    mount_version(&server, "2.3.3").await;
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

    // Second flush has nothing to send, sends nothing.
    let second = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(second.confirmed, 0);

    let bodies = update_bodies(&server.received_requests().await.unwrap());
    assert_eq!(bodies.len(), 1, "the second flush must not re-send");
    // Wire carries absolute value: what makes resend safe.
    assert_eq!(
        bodies.first().and_then(|b| b.get("status")),
        Some(&Value::from("read"))
    );
}

#[tokio::test]
async fn a_process_killed_mid_flight_resumes_without_losing_or_double_applying() {
    // Server applies change, process dies before recording confirmation. Intent must
    // stay queued; resend harmless.
    //
    // Crash simulated OUTSIDE `flush` (batch sent by hand): pins resend idempotency
    // (same absolute value twice = no-op). Neighbouring properties have own tests,
    // invisible to this shape:
    //
    //   - that `flush` sends BEFORE it clears the row, so a failure mid-request does
    //     not lose the intent -- `a_transient_failure_keeps_the_intent_queued` and
    //     `a_misconfigured_server_url_never_discards_queued_work`;
    //   - that a re-toggle during the in-flight window survives, which is why `confirm`
    //     compares before deleting -- `a_retoggle_during_the_request_ is_not_lost`.
    let server = MockServer::start().await;
    mount_version(&server, "2.3.3").await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1);

    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
        .unwrap();

    // Simulate crash: send batch by hand, never confirm.
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

    // Restart: next flush resends, then confirms.
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
    // Double-applying absolute set = no-op, the point: server's toggle endpoint
    // would have turned star off.
    assert_eq!(
        bodies.first().and_then(|b| b.get("starred")),
        Some(&Value::from(true))
    );
}

#[tokio::test]
async fn an_offline_burst_reconciles_on_reconnect() {
    let server = MockServer::start().await;
    mount_version(&server, "2.3.3").await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1200);

    // Offline: user reads everything, stars a few, toggling some repeatedly
    // like real person.
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

    // Every id exactly once across whole flush.
    let mut seen: Vec<i64> = Vec::new();
    for body in &bodies {
        if let Some(ids) = body.get("entry_ids").and_then(|v| v.as_array()) {
            seen.extend(ids.iter().filter_map(Value::as_i64));
        }
    }
    assert_eq!(seen.len(), 1203);

    // Stars settled on user's final value, not intermediate.
    let star_body = bodies
        .iter()
        .find(|b| b.get("starred").is_some())
        .expect("a star request");
    assert_eq!(star_body.get("starred"), Some(&Value::from(true)));
}

#[tokio::test]
async fn a_server_change_to_a_locally_mutated_entry_resolves_by_the_stated_rule() {
    // Rule: local intent wins, PER FIELD.
    //
    // User stars entry 1 offline; server (other device) marks it read. Pull
    // accepts remote read status, keeps local pending star; per entry drops one
    // or other.
    let mut db = seeded_db(1);
    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
        .unwrap();

    let remote = vuo_core::api::convert::entry(
        serde_json::from_value(entry_json(1, 1, "read", false)).unwrap(),
    )
    .unwrap();
    db.with_tx(|tx| store::upsert_entry(tx, &remote, 1, store::Arrival::Quiet))
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
    // PUT /v1/entries/{id}/star = `SET starred = NOT starred`. Replay flips
    // back, so never in replay path. Single most important thing to get wrong:
    // own test.
    let server = MockServer::start().await;
    mount_version(&server, "2.3.3").await;
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
    mount_version(&server, "2.3.3").await;
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
    // End to end through `flush`.
    //
    // `is_transient()` false for policy-refused redirect; dropping
    // non-transient would make off-origin redirect (typo, moved instance,
    // captive portal) silently destroy every queued mark and star.
    //
    // Unit tests in sync::replay can't protect this: they assert on
    // `would_discard`, pure predicate blind to caller change. Only real failure
    // through `flush` distinguishes.
    let server = MockServer::start().await;
    mount_version(&server, "2.3.3").await;
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
    mount_version(&server, "2.3.3").await;
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

    // Stopped after first batch, didn't hammer server with every remaining
    // chunk.
    let sent = update_bodies(&server.received_requests().await.unwrap());
    assert_eq!(sent.len(), 1, "every later batch would fail identically");
}

#[tokio::test]
async fn a_permanent_rejection_is_dropped_rather_than_blocking_the_queue() {
    let server = MockServer::start().await;
    mount_version(&server, "2.3.3").await;
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
async fn a_pre_2_3_2_server_keeps_intents_queued_instead_of_dropping_them() {
    // Why 2.3.2 write floor exists. On 2.2.13 starred half of `PUT /v1/entries`
    // 400s ("invalid entry status"); 400 = what `would_discard` reads as
    // "malformed forever", so star would be DELETED from outbox. Verified
    // against real 2.2.13 (live contract tests' job).
    //
    // Client refuses before sending; refusal neither transient nor permanent
    // payload rejection: intent queued until instance upgraded, nothing on
    // wire.
    let server = MockServer::start().await;
    mount_version(&server, "2.2.13").await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error_message": "invalid entry status, valid status values are: \"read\", \"unread\" and \"removed\""
        })))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = seeded_db(1);

    db.with_tx(|tx| {
        outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1)?;
        outbox::queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 2)
    })
    .unwrap();

    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(outcome.dropped, 0, "an old server must not cost user data");
    assert_eq!(outcome.confirmed, 0);
    assert_eq!(
        outcome.deferred, 2,
        "both intents stay queued: a server upgrade later replays them"
    );
    assert_eq!(outbox::len(db.conn()).unwrap(), 2);

    let sent = update_bodies(&server.received_requests().await.unwrap());
    assert!(
        sent.is_empty(),
        "the refusal happens before anything is sent; the 400 must never \
         reach the outbox's discard rule"
    );
}

#[tokio::test]
async fn a_server_upgraded_while_running_gets_the_queued_intents() {
    // Other half of floor: intents a 2.2.13 kept queued go out once upgraded,
    // no Vuo restart. Client re-reads version each daily check
    // (`server_version_ttl_secs`), doesn't pin first answer.
    let server = MockServer::start().await;
    mount_version(&server, "2.2.13").await;
    let client = client_for(&server);
    let mut db = seeded_db(1);
    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(true), 1))
        .unwrap();

    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(outcome.deferred, 1, "still refused on the old server");

    // Owner upgrades; next sync pass asks again.
    server.reset().await;
    mount_version(&server, "2.3.3").await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    assert_eq!(client.version().await.unwrap().raw, "2.3.3");

    let outcome = replay::flush(&mut db, &client).await.unwrap();
    assert_eq!(
        outcome.confirmed, 1,
        "the upgrade must reach the write floor"
    );
    assert_eq!(outbox::len(db.conn()).unwrap(), 0);
}

#[tokio::test]
async fn a_retoggle_during_the_request_is_not_lost() {
    // Compare-and-delete, end to end.
    let server = MockServer::start().await;
    mount_version(&server, "2.3.3").await;
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

    // Request goes out...
    client
        .update_entries(&sent.entry_ids, vuo_core::api::EntryMutation::Starred(true))
        .await
        .unwrap();
    // ...user changes mind while in flight...
    db.with_tx(|tx| outbox::queue(tx, EntryId(1), DesiredValue::Starred(false), 2))
        .unwrap();
    // ...only then confirmation lands.
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
