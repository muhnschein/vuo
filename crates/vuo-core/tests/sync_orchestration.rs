//! `sync::sync()` — the orchestrator — driven end to end.
//!
//! # Why this file exists
//!
//! Its pieces were well covered and their composition was not. `replay::flush`
//! and `pull::entries` each have a test binary; `sync()` had only two unit
//! tests, over `era_label` and the default `SyncOptions` values. The one test
//! that called it, `icon_fetching.rs`, asserted only icon side-effects.
//!
//! Two independent mutations of `sync()` left all eleven test binaries green:
//!
//!   - dropping `store::set_sync_state(tx, &next)`, so the cursor never
//!     advances (every sync is a full re-pull, on a phone, forever) and
//!     `sync_generation` never increments -- which is what the deletion sweep
//!     keys off;
//!   - `if false && !options.skip_replay`, so the user's queued marks and stars
//!     never leave the device at all.
//!
//! Both are silent: no error, no log, nothing a user would report except
//! "starring doesn't work" months later. This file asserts the PERSISTED STATE
//! and the SEQUENCE of a pass, which is the only place either shows up.

// Test code: see the note in vuo-core's lib.rs. The unwrap/panic denials guard
// foreign-input paths in production, not assertions in tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod common;

use common::*;
use vuo_core::db::outbox::{self, DesiredValue};
use vuo_core::db::{store, Database};
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::sync::{self, SyncOptions};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A server that answers every endpoint one pass touches.
async fn quiet_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": "2.2.0"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![category_json(1, "News")]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![feed_json(1, "Feed", 1)]))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds/counters"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reads": {}, "unreads": {}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(200)
                // Fri, 02 Jan 2026 03:04:05 GMT == 1767323045
                .insert_header("Date", "Fri, 02 Jan 2026 03:04:05 GMT")
                .set_body_json(entries_response(vec![entry_json(1, 1, "unread", false)], 1)),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_pass_persists_the_cursor_and_bumps_the_generation() {
    let server = quiet_server().await;
    let client = client_for(&server);
    let mut db = Database::open_in_memory().expect("mirror");

    let before = store::sync_state(db.conn()).expect("state");
    assert_eq!(
        before.cursor_changed_after, None,
        "a fresh mirror has no cursor"
    );

    let report = sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("sync");
    assert_eq!(report.pull.upserted, 1);

    let after = store::sync_state(db.conn()).expect("state");
    assert_eq!(
        after.cursor_changed_after,
        Some(1_767_323_045 - vuo_core::sync::pull::CURSOR_SKEW_SECS),
        "the cursor the pass computed must be COMMITTED. Dropping the
         set_sync_state call means every sync re-pulls the whole window, on a
         phone, forever -- and nothing else in the suite noticed."
    );
    assert_eq!(
        after.sync_generation,
        before.sync_generation + 1,
        "the generation must advance; the deletion sweep keys off it"
    );
    assert_eq!(after.server_version.as_deref(), Some("2.2.0"));

    // And a second pass moves it on again rather than starting over.
    sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("second sync");
    let third = store::sync_state(db.conn()).expect("state");
    assert_eq!(third.sync_generation, before.sync_generation + 2);
}

#[tokio::test]
async fn a_pass_sends_the_users_queued_actions_before_pulling() {
    // Order matters: replaying first means the pull's echo of our own write
    // arrives with the value we already set, rather than the pull overwriting
    // an intent we had not sent yet.
    let server = quiet_server().await;
    let client = client_for(&server);
    let mut db = Database::open_in_memory().expect("mirror");

    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'Feed')", [])?;
        tx.execute(
            "INSERT INTO entries (id, feed_id, status, starred, title) \
             VALUES (1, 1, 'unread', 0, 'e')",
            [],
        )?;
        outbox::queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 1)
    })
    .expect("queue");
    assert_eq!(outbox::len(db.conn()).expect("outbox"), 1);

    let report = sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("sync");

    assert_eq!(
        report.replay.confirmed, 1,
        "a pass must send the user's queued actions. `if false && !skip_replay`
         -- so marks and stars never leave the device -- left every other test
         binary green."
    );
    assert_eq!(
        outbox::len(db.conn()).expect("outbox"),
        0,
        "and clear them once the server confirmed"
    );

    let requests = server.received_requests().await.expect("requests");
    let put = requests
        .iter()
        .position(|r| r.method == wiremock::http::Method::PUT)
        .expect("the queued mark must have been sent");
    let entries_get = requests
        .iter()
        .position(|r| r.method == wiremock::http::Method::GET && r.url.path() == "/v1/entries")
        .expect("the pass must pull entries");
    assert!(
        put < entries_get,
        "the replay must go out BEFORE the pull, or the pull's snapshot \
         overwrites an intent that had not been sent yet"
    );
}

#[tokio::test]
async fn an_auth_failure_stops_the_pass_rather_than_pulling_anyway() {
    // Pulling would fail identically, and burying a credential problem under a
    // network error is what makes "sync does nothing" unreportable.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let client = client_for(&server);
    let mut db = Database::open_in_memory().expect("mirror");
    db.with_tx(|tx| {
        tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'Feed')", [])?;
        tx.execute(
            "INSERT INTO entries (id, feed_id, status, starred, title) \
             VALUES (1, 1, 'unread', 0, 'e')",
            [],
        )?;
        outbox::queue(tx, EntryId(1), DesiredValue::Status(EntryStatus::Read), 1)
    })
    .expect("queue");

    let report = sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("a rejected key is a report, not an error");

    assert!(report.replay.auth_failed);
    assert_eq!(report.pull.upserted, 0, "the pass must not have pulled");
    assert_eq!(
        outbox::len(db.conn()).expect("outbox"),
        1,
        "and must not have dropped the user's action"
    );

    // The mirror's state is untouched, so the next pass starts where this one
    // would have.
    let state = store::sync_state(db.conn()).expect("state");
    assert_eq!(
        state.sync_generation, 0,
        "a stopped pass commits no generation"
    );
}

/// §the retention window is applied by the pass, and only when it is set.
///
/// The mirror is a cache of the server, but nothing ever removed anything from
/// it: a phone that had read a year of feeds still held every article body it
/// had ever seen, and the file only grew. This is the step that shrinks it, so
/// it is also the step that can lose the reader's articles if it runs when it
/// was not asked to.
#[tokio::test]
async fn a_pass_prunes_only_when_a_retention_window_is_set() {
    // Two locally-held articles the server does not list, both published in
    // January and read long ago. One is a favourite.
    async fn seeded() -> Database {
        let mut db = Database::open_in_memory().expect("mirror");
        db.with_tx(|tx| {
            tx.execute("INSERT INTO feeds (id, title) VALUES (1, 'Feed')", [])?;
            tx.execute(
                "INSERT INTO entries (id, feed_id, status, starred, published_at)
                 VALUES (98, 1, 'read', 0, 1767323045), (99, 1, 'read', 1, 1767323045)",
                [],
            )?;
            Ok(())
        })
        .expect("seed");
        db
    }

    let server = quiet_server().await;
    let client = client_for(&server);

    // Off by default, which is what every version before this did and what an
    // install that has never opened Settings still means.
    let mut db = seeded().await;
    let report = sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("sync");
    assert_eq!(report.entries_pruned, 0);
    assert_eq!(
        store::local_entry_ids(db.conn()).expect("ids").len(),
        3,
        "the two seeded articles and the one the pull brought"
    );

    // And on, with a window that closed months ago.
    let mut db = seeded().await;
    let options = SyncOptions {
        retention_secs: Some(30 * 24 * 60 * 60),
        ..SyncOptions::default()
    };
    let report = sync::sync(&mut db, &client, options).await.expect("sync");
    assert_eq!(
        report.entries_pruned, 1,
        "the read article goes and the favourite stays"
    );
    let mut left: Vec<i64> = store::local_entry_ids(db.conn())
        .expect("ids")
        .iter()
        .map(|id| id.get())
        .collect();
    left.sort_unstable();
    assert_eq!(
        left,
        vec![1, 99],
        "entry 1 is unread and entry 99 is a favourite; neither is retention's to take"
    );
}
