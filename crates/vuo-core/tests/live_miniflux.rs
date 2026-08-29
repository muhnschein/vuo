//! Opt-in integration tests against a real Miniflux instance.
//!
//! §8: everything in `make check` must run *"from a clean checkout, with no
//! phone, no server account, and no network"*. These tests need a server, so
//! they sit behind an explicit gate rather than being skipped silently:
//!
//! ```sh
//! VUO_LIVE_BASE_URL=http://127.0.0.1:8080 \
//! VUO_LIVE_TOKEN=... \
//!   cargo test -p vuo-core --features live-integration-tests -- --ignored
//! ```
//!
//! `make live-test` does that, and the ephemeral-Miniflux CI job runs it
//! weekly against a pinned container.
//!
//! # Why these exist at all
//!
//! §8.3: *the §11 open questions — cursor semantics and bulk-mutation
//! idempotency — are contract questions about someone else's server.
//! Verifying them once by hand and writing the answer into a comment means
//! finding out by regression when the server updates.*
//!
//! So these do not re-test Vuo's logic, which the mock-server suite already
//! covers. They test the **assumptions Vuo makes about Miniflux**, and each one
//! names the assumption it is protecting.

// Test code: see the note in vuo-core's lib.rs. The unwrap/panic denials
// guard foreign-input paths in production, not assertions in tests.
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

/// `None` only when the opt-in has NOT been taken.
///
/// Once `VUO_LIVE_BASE_URL` is set, a URL that will not parse or a transport
/// that will not build is a configuration error and panics. It used to be a
/// chain of `.ok()?`, so a typo'd URL made every live test return early --
/// which cargo reports as `ok`. `make live-test` then passed having contacted
/// nothing, which is exactly the silent skip the module doc says it is not.
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

/// Fail rather than return when the instance cannot exercise the contract.
///
/// An early `return` is a PASS. Both contract tests used to take one when the
/// instance held too few entries -- and that is the state the weekly CI job
/// produces, since it imports an OPML and runs immediately, before Miniflux's
/// poller has fetched anything. So the job could report green having asserted
/// nothing about the two API behaviours it exists to watch.
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
    // Vuo gates request construction on this. An unparseable version falls
    // back to the oldest supported behaviour, which is safe but wasteful.
    let client = client_or_skip!();
    let version = client.version().await.expect("version");
    eprintln!(
        "server version: {} (parsed {}.{}.{})",
        version.raw, version.major, version.minor, version.patch
    );
    assert!(version.major >= 2, "Vuo targets Miniflux 2.x");
}

#[tokio::test]
#[ignore = "needs a real Miniflux instance"]
async fn keyset_pagination_returns_strictly_increasing_ids() {
    // THE ASSUMPTION: order=id&direction=asc&after_entry_id=N is a true keyset
    // cursor with no ties. Vuo's whole gap-free pull rests on it. If a future
    // Miniflux changed the sort, this fails here rather than by users
    // reporting missing articles.
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
    // THE ASSUMPTION that the entire outbox design rests on: PUT /v1/entries
    // sets an absolute value, so replaying it is a no-op rather than a toggle.
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

    // Apply the same absolute value twice.
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

    // And the same for starred, which is the field the server's other
    // endpoints implement as a toggle.
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

    // Leave the instance as we found it.
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
    // The server answers 400. Vuo refuses first, so the outbox's retry
    // classifier never sees a self-inflicted client error.
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
    // THE ASSUMPTION behind the pull's echo handling: a mutation bumps
    // changed_at, so Vuo sees its own writes come back. The sync applies them
    // as no-ops; this test documents that the echo is real and expected rather
    // than a sign something is wrong.
    // This had NO assertions at all. Both outcomes were accepted -- if
    // changed_at did not move it printed a note saying "the pull tolerates
    // both" and returned Ok -- so the only way it could fail was an HTTP error.
    // A contract test that accepts either answer is a liveness check on the
    // server, not a check of the contract the sync depends on.
    //
    // The reason it hedged is real, though: if the entry is ALREADY read, the
    // write is a no-op and no sane server bumps a timestamp for it. So set up
    // the precondition first, then assert.
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

    // Force the opposite status, so the mutation below is a real change.
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
    // The reconcile aborts on a mismatch rather than deleting; this checks the
    // happy path is actually reachable on a quiet instance.
    let client = client_or_skip!();
    let version = client.version().await.expect("version");
    if !version.has_entry_ids_endpoint() {
        eprintln!("skipping: /v1/entries/ids needs Miniflux 2.3.2+");
        return;
    }
    // `assert!(ids.total >= 0)` was the only assertion, which the server can
    // only fail by sending a negative count -- and the actual claim, that
    // `entry_ids.len()` matches `total`, was an eprintln! note. That claim is
    // the one the reconcile's deletion guard rests on: a server declaring 999
    // and returning one id is exactly the torn listing that makes the guard
    // abort, so a test named for the match has to check the match.
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
        // Page through and check the union, which is what the reconcile does.
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
