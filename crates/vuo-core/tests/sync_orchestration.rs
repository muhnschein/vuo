//! `sync::sync()` (orchestrator) driven end to end.
//!
//! # Why
//!
//! Pieces covered elsewhere (`replay::flush`, `pull::entries` have own
//! binaries); composition covered here. Mutations this catches, invisible to
//! other binaries:
//!
//!   - dropping `store::set_sync_state(tx, &next)`, so the cursor never
//!     advances (every sync is a full re-pull, on a phone, forever) and
//!     `sync_generation` never increments -- which is what the deletion sweep
//!     keys off;
//!   - `if false && !options.skip_replay`, so the user's queued marks and stars
//!     never leave the device at all.
//!
//! Both silent: no error, no log, only "starring doesn't work" months later.
//! Asserts PERSISTED STATE and SEQUENCE of pass: only place either shows.

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
use vuo_core::db::outbox::{self, DesiredValue};
use vuo_core::db::{store, Database};
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::sync::{self, SyncOptions};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Server answering every endpoint one pass touches, at `version`.
///
/// Version load-bearing: 2.2.0 takes legacy branches (no id listing, no
/// deletion reconcile), 2.3.3 modern ones. Id listing mounted either way:
/// version bump can't fail by 404.
async fn server_with_version(version: &str) -> MockServer {
    let server = MockServer::start().await;
    mount_version(&server, version).await;
    mount_pass_endpoints(&server).await;
    server
}

/// Every endpoint a pass touches except the version.
async fn mount_pass_endpoints(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/entries/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total": 1,
            "entry_ids": [1]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![category_json(1, "News")]))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![feed_json(1, "Feed", 1)]))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds/counters"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reads": {}, "unreads": {}
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(
            ResponseTemplate::new(200)
                // Fri, 02 Jan 2026 03:04:05 GMT == 1767323045
                .insert_header("Date", "Fri, 02 Jan 2026 03:04:05 GMT")
                .set_body_json(entries_response(vec![entry_json(1, 1, "unread", false)], 1)),
        )
        .mount(server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

/// Legacy fixture: pre-2.3.2 server. Vuo refuses entry-state writes below
/// floor: only empty-outbox passes here.
async fn quiet_server() -> MockServer {
    server_with_version("2.2.0").await
}

/// Server Vuo writes to: 2.3.2+, `PUT /v1/entries` honours absolute sets. Tests
/// queueing user actions use this.
async fn modern_server() -> MockServer {
    server_with_version("2.3.3").await
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
        Some(1_767_323_045 - sync::pull::CURSOR_SKEW_SECS),
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

    // Second pass moves it on again, doesn't start over.
    sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("second sync");
    let third = store::sync_state(db.conn()).expect("state");
    assert_eq!(third.sync_generation, before.sync_generation + 2);
}

#[tokio::test]
async fn a_pass_sends_the_users_queued_actions_before_pulling() {
    // Order matters: replay first, so pull's echo of own write carries value
    // already set; pull can't overwrite unsent intent.
    let server = modern_server().await;
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
    // Pull would fail identically; credential problem buried under network
    // error makes "sync does nothing" unreportable.
    let server = MockServer::start().await;
    mount_version(&server, "2.3.3").await;
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

    // Mirror state untouched: next pass starts where this would have.
    let state = store::sync_state(db.conn()).expect("state");
    assert_eq!(
        state.sync_generation, 0,
        "a stopped pass commits no generation"
    );
}

/// §the retention window is applied by the pass, and only when it is set.
///
/// Only step shrinking mirror (else file grows forever), so also step that can
/// lose reader's articles if run unasked.
#[tokio::test]
async fn a_pass_prunes_only_when_a_retention_window_is_set() {
    // Two local articles server doesn't list, published January, read long ago.
    // One favourite.
    fn seeded() -> Database {
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

    // Off by default: what install that never opened Settings means.
    let mut db = seeded();
    let report = sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("sync");
    assert_eq!(report.entries_pruned, 0);
    assert_eq!(
        store::local_entry_ids(db.conn()).expect("ids").len(),
        3,
        "the two seeded articles and the one the pull brought"
    );

    // On, window closed months ago.
    let mut db = seeded();
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

/// §the server version is asked for once, not once per pass.
///
/// Version gates two request rules and one endpoint, pass needs it; but changes
/// on Miniflux upgrade only, and every request re-arms radio for tail timer.
/// Hourly ask for yearly-changing answer = cheapest request to remove.
#[tokio::test]
async fn a_recorded_server_version_is_not_asked_for_again_next_pass() {
    let server = quiet_server().await;
    let client = client_for(&server);
    let mut db = Database::open_in_memory().expect("mirror");

    sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("first sync");

    let state = store::sync_state(db.conn()).expect("state");
    assert_eq!(state.server_version.as_deref(), Some("2.2.0"));
    assert!(
        state.server_version_checked_at.is_some(),
        "the pass that asked must record WHEN it asked, or the next one cannot \
         tell a fresh answer from an ancient one"
    );

    let after_first = version_requests(&server).await;
    assert_eq!(after_first, 1, "the first pass has nothing cached to use");

    sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("second sync");
    assert_eq!(
        version_requests(&server).await,
        after_first,
        "the second pass must reuse the recorded version rather than asking again"
    );

    // Cache expires: zero TTL = "trust nothing", what upgrade between passes
    // needs.
    sync::sync(
        &mut db,
        &client,
        SyncOptions {
            server_version_ttl_secs: 0,
            ..SyncOptions::default()
        },
    )
    .await
    .expect("third sync");
    assert_eq!(
        version_requests(&server).await,
        after_first + 1,
        "an expired cache must be re-asked, or an upgraded server is never noticed"
    );
}

/// Failed version check keeps last recorded version.
///
/// Oldest-supported fallback would, for one pass, skip due reconcile and record
/// pre-2.3 era for unchanged server, over one failed request.
#[tokio::test]
async fn a_failed_version_check_keeps_the_recorded_version() {
    let server = modern_server().await;
    let client = client_for(&server);
    let mut db = Database::open_in_memory().expect("mirror");

    sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("first sync");
    let before = store::sync_state(db.conn()).expect("state");
    assert_eq!(before.server_version.as_deref(), Some("2.3.3"));

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/v1/version"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    mount_pass_endpoints(&server).await;

    let report = sync::sync(
        &mut db,
        &client,
        SyncOptions {
            server_version_ttl_secs: 0,
            ..SyncOptions::default()
        },
    )
    .await
    .expect("a failed version check does not fail the pass");
    assert_eq!(report.server_version.as_deref(), Some("2.3.3"));

    let after = store::sync_state(db.conn()).expect("state");
    assert_eq!(after.server_version.as_deref(), Some("2.3.3"));
    assert_eq!(after.server_era, before.server_era);
    assert_eq!(
        after.server_version_checked_at, before.server_version_checked_at,
        "a check that failed must not count as a check, or the next pass \
         would not ask again"
    );
}

/// §the counters request is not spent when nothing could read its answer.
///
/// ONLY evidence server-side deletion leaves (cursor blind): survives quiet
/// pass. Needn't survive pass whose reconcile is due anyway.
#[tokio::test]
async fn the_counters_request_is_skipped_when_a_reconcile_is_due_anyway() {
    let server = quiet_server().await;
    let client = client_for(&server);
    let mut db = Database::open_in_memory().expect("mirror");

    // Fresh mirror never reconciled: first pass always due.
    sync::sync(&mut db, &client, SyncOptions::default())
        .await
        .expect("sync");

    let counters = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|r| r.url.path() == "/v1/feeds/counters")
        .count();
    assert_eq!(
        counters, 0,
        "a pass that is going to reconcile regardless has no use for the \
         counters, and asking anyway is a round trip spent on a question \
         whose answer is ignored"
    );
}

/// How many times the mock server was asked for its version.
async fn version_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|r| r.url.path() == "/v1/version")
        .count()
}
