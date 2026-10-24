//! Opt-in integration tests against real Miniflux.
//!
//! §8: everything in `make check` runs *"from a clean checkout, with no phone,
//! no server account, and no network"*. These need server: explicit gate, not
//! silent skip:
//!
//! ```sh
//! VUO_LIVE_BASE_URL=http://127.0.0.1:8080 \
//! VUO_LIVE_TOKEN=... \
//!   cargo test -p vuo-core --features live-integration-tests -- --ignored
//! ```
//!
//! `make live-test` runs that; ephemeral-Miniflux CI job runs it weekly against
//! pinned container.
//!
//! # Why
//!
//! §8.3: *the §11 open questions — cursor semantics and bulk-mutation
//! idempotency — are contract questions about someone else's server. Verifying
//! them once by hand and writing the answer into a comment means finding out by
//! regression when the server updates.*
//!
//! Not re-testing Vuo logic (mock suite covers it). Testing **assumptions Vuo
//! makes about Miniflux**; each names assumption it protects.

// Test code: see note in vuo-core's lib.rs. unwrap/panic denials guard
// production foreign-input paths, not test assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
#![cfg(feature = "live-integration-tests")]

use vuo_core::api::{EntriesQuery, EntryMutation, MinifluxClient, Transport, TransportConfig};
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::redact::ApiToken;

/// `None` only when opt-in NOT taken.
///
/// `VUO_LIVE_BASE_URL` set → unparseable URL or unbuildable transport = config
/// error, panics. Early return would be reported `ok` by cargo: `make
/// live-test` passing having contacted nothing = silent skip module doc
/// forbids.
fn live_client() -> Option<MinifluxClient> {
    let base = std::env::var("VUO_LIVE_BASE_URL").ok()?;
    let token = std::env::var("VUO_LIVE_TOKEN")
        .expect("VUO_LIVE_BASE_URL is set, so VUO_LIVE_TOKEN must be too");
    let origin = url::Url::parse(&base).expect("VUO_LIVE_BASE_URL must be a valid URL");
    let transport = Transport::new(origin, ApiToken::new(token), &TransportConfig::default())
        .expect("the live transport must build");
    Some(MinifluxClient::new(transport))
}

macro_rules! client_or_skip {
    () => {
        match live_client() {
            Some(c) => c,
            None => {
                eprintln!("skipping: set VUO_LIVE_BASE_URL and VUO_LIVE_TOKEN");
                return;
            }
        }
    };
}

/// Fail, don't return, when instance can't exercise contract.
///
/// Early `return` = PASS. Weekly CI job imports OPML and runs before Miniflux's
/// poller fetched anything: too few entries. Returning would report green with
/// nothing asserted.
macro_rules! require_entries {
    ($have:expr, $need:expr, $what:literal) => {
        assert!(
            $have >= $need,
            "the live instance has {} entries; {} needs at least {}. Seed it before \
             running: a green run against an empty instance proves nothing.",
            $have,
            $what,
            $need
        );
    };
}

#[tokio::test]
#[ignore = "needs a real Miniflux instance"]
async fn the_server_reports_a_version_we_can_parse() {
    // Vuo gates request construction on this. Unparseable version → oldest
    // supported behaviour: safe, wasteful.
    let client = client_or_skip!();
    let version = client.version().await.expect("version");
    eprintln!(
        "server version: {} (parsed {}.{}.{})",
        version.raw, version.major, version.minor, version.patch
    );
    // Write floor, not just "2.x": below 2.3.2 Vuo refuses entry-state writes
    // (`ServerVersion::supports_entry_state_writes`); older instance mirrors
    // but never syncs user actions.
    assert!(
        version.supports_entry_state_writes(),
        "Vuo needs Miniflux 2.3.2+; this instance says {}",
        version.raw
    );
}

#[tokio::test]
#[ignore = "needs a real Miniflux instance"]
async fn keyset_pagination_returns_strictly_increasing_ids() {
    // ASSUMPTION: order=id&direction=asc&after_entry_id=N = true keyset cursor,
    // no ties. Gap-free pull rests on it. Future sort change fails here, not
    // via users reporting missing articles.
    let client = client_or_skip!();

    let (first, _) = client
        .entries(&EntriesQuery::keyset(5))
        .await
        .expect("first page");
    require_entries!(
        first.entries.len(),
        2,
        "keyset_pagination_returns_strictly_increasing_ids"
    );

    let ids: Vec<i64> = first.entries.iter().map(|e| e.id).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "ids were not strictly increasing: {ids:?}"
    );

    let last = *ids.last().expect("non-empty");
    let (second, _) = client
        .entries(&EntriesQuery::keyset(5).after_entry_id(Some(EntryId(last))))
        .await
        .expect("second page");
    assert!(
        second.entries.iter().all(|e| e.id > last),
        "after_entry_id did not exclude everything up to and including {last}"
    );
}

#[tokio::test]
#[ignore = "needs a real Miniflux instance"]
async fn bulk_status_updates_are_absolute_and_idempotent() {
    // ASSUMPTION whole outbox design rests on: PUT /v1/entries sets absolute
    // value; replay = no-op, not toggle.
    let client = client_or_skip!();

    let (page, _) = client
        .entries(&EntriesQuery::keyset(1))
        .await
        .expect("an entry");
    let Some(entry) = page.entries.first() else {
        eprintln!("skipping: the instance has no entries");
        return;
    };
    let id = EntryId(entry.id);
    let original = if entry.status == "read" {
        EntryStatus::Read
    } else {
        EntryStatus::Unread
    };

    // Same absolute value twice.
    client
        .update_entries(&[id], EntryMutation::Status(EntryStatus::Read))
        .await
        .expect("first");
    let after_first = client.entry(id).await.expect("read back").status.clone();
    client
        .update_entries(&[id], EntryMutation::Status(EntryStatus::Read))
        .await
        .expect("second");
    let after_second = client.entry(id).await.expect("read back").status.clone();

    assert_eq!(after_first, "read");
    assert_eq!(
        after_first, after_second,
        "PUT /v1/entries is not idempotent -- the outbox design is invalid"
    );

    // Same for starred (server's other endpoints toggle it). `starred` field of
    // `PUT /v1/entries` only from 2.3.2; below, body 400s ("invalid entry
    // status") and outbox would drop intent as malformed. Client refuses below
    // floor before sending (`outbox_reconciliation` holds that); this holds
    // absolute-set semantics refusal preserves.
    client
        .update_entries(&[id], EntryMutation::Starred(true))
        .await
        .expect("star");
    let starred_once = client.entry(id).await.expect("read back").starred;
    client
        .update_entries(&[id], EntryMutation::Starred(true))
        .await
        .expect("star again");
    let starred_twice = client.entry(id).await.expect("read back").starred;
    assert!(starred_once, "starred was not set");
    assert_eq!(
        starred_once, starred_twice,
        "the starred field toggled on replay -- absolute-set semantics are gone"
    );

    // Leave instance as found.
    client
        .update_entries(&[id], EntryMutation::Starred(false))
        .await
        .ok();
    client
        .update_entries(&[id], EntryMutation::Status(original))
        .await
        .ok();
}

#[tokio::test]
#[ignore = "needs a real Miniflux instance"]
async fn an_empty_id_list_is_refused_locally_not_sent() {
    // Server answers 400. Vuo refuses first: retry classifier never sees
    // self-inflicted client error.
    let client = client_or_skip!();
    let err = client
        .update_entries(&[], EntryMutation::Status(EntryStatus::Read))
        .await
        .expect_err("an empty batch must be refused");
    assert!(
        !err.is_transient(),
        "an empty batch must not look retryable"
    );
}

#[tokio::test]
#[ignore = "needs a real Miniflux instance"]
async fn changed_after_is_bumped_by_our_own_writes() {
    // ASSUMPTION behind pull's echo handling: mutation bumps changed_at, Vuo
    // sees own writes return. Sync applies them as no-ops; this pins echo as
    // real and expected.
    //
    // Already-read entry → write is no-op, no sane server bumps timestamp. So
    // set precondition first, then assert; test accepting either answer =
    // server liveness check, not contract.
    let client = client_or_skip!();
    let (page, _) = client
        .entries(&EntriesQuery::keyset(1))
        .await
        .expect("an entry");
    require_entries!(
        page.entries.len(),
        1,
        "changed_after_is_bumped_by_our_own_writes"
    );
    let id = EntryId(page.entries[0].id);

    // Force opposite status: mutation below is real change.
    client
        .update_entries(&[id], EntryMutation::Status(EntryStatus::Unread))
        .await
        .expect("set up the precondition");
    let before = client
        .entry(id)
        .await
        .expect("before")
        .changed_at
        .expect("the server must report changed_at");

    client
        .update_entries(&[id], EntryMutation::Status(EntryStatus::Read))
        .await
        .expect("mutate");
    let after = client
        .entry(id)
        .await
        .expect("after")
        .changed_at
        .expect("the server must report changed_at");

    assert!(
        after > before,
        "a real status change must bump changed_at ({before:?} -> {after:?}). \
         The pull's cursor is `changed_after`: a server that does not bump it \
         means Vuo never sees its own writes come back, and worse, never sees \
         anyone else's either."
    );
}

#[tokio::test]
#[ignore = "needs a real Miniflux instance"]
async fn the_entry_ids_endpoint_matches_its_declared_total() {
    // Reconcile aborts on mismatch, doesn't delete; checks happy path reachable
    // on quiet instance.
    let client = client_or_skip!();
    let version = client.version().await.expect("version");
    if !version.has_entry_ids_endpoint() {
        eprintln!("skipping: /v1/entries/ids needs Miniflux 2.3.2+");
        return;
    }
    // Claim reconcile's deletion guard rests on: `entry_ids.len()` matches
    // `total`. Server declaring 999 and returning one id = torn listing guard
    // aborts on. Test named for match checks match.
    const LIMIT: u32 = 1000;
    let ids = client
        .entry_ids(&EntriesQuery::default().with_limit(LIMIT), 0)
        .await
        .expect("ids");
    assert!(ids.total >= 0, "a negative total is not a count");

    if ids.total <= i64::from(LIMIT) {
        assert_eq!(
            ids.entry_ids.len() as i64,
            ids.total,
            "one page holds the whole listing, so the ids must match the declared total"
        );
    } else {
        // Page through, check union: what reconcile does.
        let mut collected: Vec<i64> = ids.entry_ids.clone();
        let mut offset: u32 = LIMIT;
        while (collected.len() as i64) < ids.total {
            let page = client
                .entry_ids(&EntriesQuery::default().with_limit(LIMIT), offset)
                .await
                .expect("a later page");
            if page.entry_ids.is_empty() {
                break;
            }
            offset = offset.saturating_add(u32::try_from(page.entry_ids.len()).unwrap_or(LIMIT));
            collected.extend(page.entry_ids);
        }
        assert_eq!(
            collected.len() as i64,
            ids.total,
            "paging the listing must yield exactly the declared total"
        );
        let mut sorted = collected.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            collected.len(),
            "a paged listing must not repeat an id; the reconcile would then \
             under-count and delete live entries"
        );
    }
}
