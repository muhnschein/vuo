//! Pulling server state into mirror.
//!
//! # Cursor
//!
//! §11 first question: *which query parameters give a reliable "changed since"
//! pull without gaps or unbounded re-fetching, and how are server-side
//! deletions detected.* Answer, from server source:
//!
//! **`changed_after` for window, keyset on `id` for paging.** Not `offset`.
//! Server's `ORDER BY` has one expression, no id tiebreaker: every order but
//! `id` is *unstable*. One mark-all-as-read stamps thousands of rows with same
//! `changed_at`; offset paging silently skips and duplicates.
//! `order=id&direction=asc&after_entry_id=N` compiles to `e.id > N` over
//! primary key: true keyset cursor, no ties.
//!
//! **Why no gaps.** Entry mutated *during* pass: `id > last_id` (this pass sees
//! it) or `id <= last_id` (doesn't). Second case: `changed_at` ≥ pass start,
//! after persisted cursor, so next pass catches it. Overlap bounded by skew
//! constant, not corpus size: re-fetch stays bounded.
//!
//! **Why cursor from server's clock.** Comparison server-side; phone clock
//! untrustworthy (hours out, jumps when travelling). Pass reads first
//! response's `Date` header, persists it minus [`CURSOR_SKEW_SECS`]: covers
//! skew and transactions whose `now()` predates commit visibility.
//!
//! **Cursor persisted only after whole pass commits.** Crash mid-pass replays
//! from old cursor: safe (idempotent upserts), only choice that can't lose
//! entry.
//!
//! # What cursor can't see
//!
//! Two blind spots, both real, both handled elsewhere:
//!
//! 1. **Deletions.** From 2.3.0 hard-deleted, no API trace; before, became
//!    `status=removed`. Neither reachable via `changed_after`. See
//!    [`reconcile`].
//! 2. **Content edits.** Feed refresh rewriting title or body does *not* bump
//!    `changed_at`: pull never re-fetches edited body for held entry. Vuo
//!    refreshes content on user open; cursor doesn't pretend to cover it.

use crate::api::{convert, EntriesQuery, MinifluxClient};
use crate::db::{store, Database};
use crate::error::Result;
use crate::model::{Entry, EntryId};

/// Seconds subtracted from server's clock when persisting cursor.
///
/// Covers skew between responses and transactions whose `now()` predates
/// visibility. Re-seeing entry free (idempotent upsert); missing one isn't:
/// errs generous.
pub const CURSOR_SKEW_SECS: i64 = 60;

/// Entries per page. Well under server's 1000 cap; failed page cheap to retry
/// over phone connection.
pub const PAGE_SIZE: u32 = 250;

/// Safety valve on pages per pass.
///
/// Pass failing to advance must terminate, not loop against user's data
/// allowance.
pub const MAX_PAGES_PER_PASS: usize = 400;

/// What one [`entries`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullOutcome {
    /// Entries that validated and were written to the mirror.
    pub upserted: usize,
    /// Entries dropped because they failed validation.
    pub rejected: usize,
    /// Entries pre-2.3 server reported `removed`, deleted locally.
    pub removed: usize,
    /// Pages requested, including the final short one.
    pub pages: usize,
    /// The cursor to persist if the whole pass succeeds.
    pub next_cursor: Option<i64>,
}

/// Pull categories and feeds. Cheap; needed before entries so entry's feed
/// exists when UI joins against it.
///
/// Per-item conversion failures drop item, not errors.
///
/// # Errors
///
/// - [`Error::Transport`], [`Error::Http`] or [`Error::Protocol`] when either
///   listing request fails.
/// - [`Error::Db`] when mirror write/read fails.
///
/// [`Error::Transport`]: crate::error::Error::Transport
/// [`Error::Http`]: crate::error::Error::Http
/// [`Error::Protocol`]: crate::error::Error::Protocol
/// [`Error::Db`]: crate::error::Error::Db
pub async fn taxonomy(db: &mut Database, client: &MinifluxClient, generation: i64) -> Result<()> {
    // SEQUENTIAL on purpose, though listings independent.
    //
    // Concurrent saves one round trip, but costs second connection: `reqwest`
    // built without `http2`, no multiplexing; two in flight = two TCP
    // connections, two TLS handshakes. Passes hour apart, pool idle timeout
    // ninety seconds: every pass cold.
    //
    // Saved round trip = tens of ms off radio event with seconds-long tail;
    // extra handshake = packets and EC work. Positive only with HTTP/2; do that
    // first (module docs).
    let categories = client.categories().await?;
    let feeds = client.feeds().await?;

    // Convert outside transaction: per-item rejection mustn't roll back good
    // items.
    let categories: Vec<_> = categories
        .into_iter()
        .filter_map(|c| convert::category(c).ok())
        .collect();
    let feeds: Vec<_> = feeds
        .into_iter()
        .filter_map(|f| convert::feed(f).ok())
        .collect();

    db.with_tx(|tx| {
        for c in &categories {
            store::upsert_category(tx, c, generation)?;
        }
        for f in &feeds {
            store::upsert_feed(tx, f, generation)?;
        }
        Ok(())
    })?;

    // Feed gone from listing = unsubscribed (here or elsewhere). Entries go
    // too: unreachable in UI, else accumulate forever. Cheapest, most common
    // deletion case, no extra request.
    let live: std::collections::HashSet<i64> = feeds.iter().map(|f| f.id.get()).collect();
    let local = store::feeds(db.conn())?;

    // EMPTY feed listing never read as "unsubscribed from everything". Proxy
    // serving stale cached `[]`, or partial response, would delete whole
    // mirror; unsubscribing everything at once isn't real workflow, cached
    // empty body is real failure. No feeds genuinely → nothing local to delete
    // anyway.
    if feeds.is_empty() && !local.is_empty() {
        tracing::warn!(
            local = local.len(),
            "the server listed no feeds at all; refusing to treat that as a mass unsubscribe"
        );
        return Ok(());
    }

    let gone: Vec<_> = local
        .iter()
        .map(|f| f.id)
        .filter(|id| !live.contains(&id.get()))
        .collect();
    if !gone.is_empty() {
        db.with_tx(|tx| {
            for id in &gone {
                store::delete_feed(tx, *id)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Whether entry this pass writes would be NEWS if new to mirror.
///
/// Upsert decides "new to mirror" (only INSERT half marks, see
/// [`store::upsert_entry`]); this rules out two ways a row inserts without
/// being fresh article:
///
/// - **Mirror's first pass.** `cursor` `None`, every entry = insert. Announcing
///   whole corpus as "new" on setup day tells reader nothing.
/// - **Old entry returning.** Cursor = `changed_after`; entry changes when
///   anything touches it (marked unread on web, starred elsewhere). One
///   retention pruned (see [`store::prune_entries`]), or torn-listing reconcile
///   never had, re-enters as insert. Its `created_at` (server first stored it,
///   server clock = cursor's clock) predates window: gives it away.
///
/// No `created_at` at all: benefit of doubt. Every API-capable Miniflux sends
/// one; absence = unknown server, "insert happened" still best evidence.
#[must_use]
pub fn arrival_for(entry: &Entry, cursor: Option<i64>) -> store::Arrival {
    let Some(window_start) = cursor else {
        return store::Arrival::Quiet;
    };
    match entry.created_at {
        Some(created) if created.timestamp() < window_start => store::Arrival::Quiet,
        _ => store::Arrival::Announce,
    }
}

/// One incremental entry pass.
///
/// Returns cursor to persist; caller stores it only after rest of sync commits.
///
/// # Errors
///
/// - [`Error::Transport`], [`Error::Http`] or [`Error::Protocol`] when any page
///   request fails.
/// - [`Error::Db`] when writing page to mirror fails.
///
/// Pages written before failure stay; no cursor returned, next pass re-reads
/// window.
///
/// [`Error::Transport`]: crate::error::Error::Transport
/// [`Error::Http`]: crate::error::Error::Http
/// [`Error::Protocol`]: crate::error::Error::Protocol
/// [`Error::Db`]: crate::error::Error::Db
pub async fn entries(
    db: &mut Database,
    client: &MinifluxClient,
    cursor: Option<i64>,
    generation: i64,
) -> Result<PullOutcome> {
    entries_with_page_cap(db, client, cursor, generation, MAX_PAGES_PER_PASS).await
}

/// [`entries`] with explicit page cap.
///
/// Cap stops endless pass; holding cursor back when it fires stops next pass
/// skipping everything beyond stop point. Real cap = 400 pages × 250 = 100,000
/// rows through mock server, too slow to test; injecting cap makes branch
/// reachable in ms.
///
/// # Errors
///
/// As for [`entries`].
pub async fn entries_with_page_cap(
    db: &mut Database,
    client: &MinifluxClient,
    cursor: Option<i64>,
    generation: i64,
    max_pages: usize,
) -> Result<PullOutcome> {
    let mut outcome = PullOutcome::default();
    let mut after: Option<EntryId> = None;
    let mut server_now: Option<i64> = None;
    // Set when pass stops early. Cursor must NOT advance then: would mark
    // unfinished window "seen", skipping everything past stop point forever.
    let mut incomplete = false;

    loop {
        if outcome.pages >= max_pages {
            tracing::warn!(
                pages = outcome.pages,
                "stopping pull at the page cap; the cursor will not advance"
            );
            incomplete = true;
            break;
        }

        let query = EntriesQuery::keyset(PAGE_SIZE)
            .changed_after(cursor)
            .after_entry_id(after);
        let (page, date) = client.entries(&query).await?;
        outcome.pages = outcome.pages.saturating_add(1);

        // Anchor to FIRST response's Date: later pages follow mutations this
        // pass won't see; later clock would skip them.
        if server_now.is_none() {
            server_now = date.map(|d| d.timestamp());
        }

        let count = page.entries.len();
        // `total` not loop bound: with after_entry_id it counts only rows ahead
        // of cursor, shrinks every page.
        let last_id = page.entries.last().map(|e| EntryId(e.id));

        let decoded = convert::entries(page.entries);
        for e in &decoded.rejected {
            tracing::debug!(error = %e, "dropped an unusable entry");
        }
        outcome.rejected = outcome.rejected.saturating_add(decoded.rejected.len());
        outcome.upserted = outcome.upserted.saturating_add(decoded.valid.len());
        outcome.removed = outcome.removed.saturating_add(decoded.removed.len());

        db.with_tx(|tx| {
            for e in &decoded.valid {
                store::upsert_entry(tx, e, generation, arrival_for(e, cursor))?;
            }
            // Pre-2.3 soft delete. Only deletion signal such server gives, via
            // ordinary cursor.
            for id in &decoded.removed {
                store::delete_entry(tx, *id)?;
            }
            Ok(())
        })?;

        // Short page = end of window. Raw returned count, not survivors: page
        // of all rejected entries still full, paging continues.
        if count < PAGE_SIZE as usize {
            break;
        }
        match last_id {
            Some(id) if Some(id) != after => after = Some(id),
            // Full page without last id, or `after_entry_id` didn't advance:
            // next request repeats this one. Stop; don't pretend window
            // covered.
            _ => {
                incomplete = true;
                break;
            }
        }
    }

    outcome.next_cursor = if incomplete {
        // Cursor stays. Next pass re-reads window: costs bandwidth; advancing
        // costs entries.
        None
    } else {
        server_now
            .map(|now| now.saturating_sub(CURSOR_SKEW_SECS))
            // Local clock fallback = compromise: no Date header, nothing
            // better; keeping old cursor forever re-pulls same window every
            // sync.
            .or_else(|| {
                Some(
                    chrono::Utc::now()
                        .timestamp()
                        .saturating_sub(CURSOR_SKEW_SECS),
                )
            })
    };

    Ok(outcome)
}

/// Cheap per-feed divergence check.
///
/// `GET /v1/feeds/counters`: one small request, "does any feed hold different
/// count than I think". Agreeing feeds skipped: common case near-zero
/// bandwidth. Full id reconcile only on divergence.
///
/// Returns disagreeing feed ids.
///
/// # Errors
///
/// - [`Error::Transport`], [`Error::Http`] or [`Error::Protocol`] when counters
///   request fails.
/// - [`Error::Db`] when reading local counts fails.
///
/// [`Error::Transport`]: crate::error::Error::Transport
/// [`Error::Http`]: crate::error::Error::Http
/// [`Error::Protocol`]: crate::error::Error::Protocol
/// [`Error::Db`]: crate::error::Error::Db
pub async fn diverging_feeds(db: &Database, client: &MinifluxClient) -> Result<Vec<i64>> {
    let counters = client.counters().await?;
    let local = store::entry_counts_by_feed(db.conn())?;

    let mut server: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for (key, value) in counters.reads.iter().chain(counters.unreads.iter()) {
        if let Ok(id) = key.parse::<i64>() {
            // Saturating, not `+=`: numbers chosen by someone else (§9.2); two
            // i64::MAX counts for one feed overflow-panic in debug. Reachable
            // panic on foreign input, §9.5 forbids (unwinding into Qt C++ =
            // UB).
            let slot = server.entry(id).or_insert(0);
            *slot = slot.saturating_add(*value);
        }
    }

    let mut diverging = Vec::new();
    for (feed_id, local_count) in &local {
        // Feed absent from counters = zero entries there.
        let server_count = server.get(feed_id).copied().unwrap_or(0);
        // ONE DIRECTION on purpose. Notices entries server dropped; can only
        // trigger reconcile, which only deletes. Holding FEWER = opposite
        // problem, cursor pull fills it; reconcile would page whole corpus for
        // nothing.
        //
        // `!=` would make mirror deliberately holding less (any with retention,
        // after `store::prune_entries` runs) report every feed diverging every
        // pass.
        if *local_count > server_count {
            diverging.push(*feed_id);
        }
    }
    diverging.sort_unstable();
    Ok(diverging)
}

/// Full deletion reconcile via `GET /v1/entries/ids`.
///
/// Needs server 2.3.2+; check
/// [`crate::model::ServerVersion::has_entry_ids_endpoint`] first.
///
/// # Correctness guard
///
/// Endpoint pages by `offset` over `id DESC`: *not* stable under concurrent
/// inserts/deletes; window shifts, id falls through crack. Acting on that
/// deletes live entry.
///
/// So accumulated id count checked against first page's `total`; mismatch
/// **aborts reconcile**, deletes nothing. Skipped reconcile = slightly stale
/// cache; reconcile on torn listing destroys user data.
///
/// # Errors
///
/// - [`Error::Transport`], [`Error::Http`] or [`Error::Protocol`] when any id
///   listing page fails, incl. [`Error::Http`] from server without endpoint.
/// - [`Error::Db`] when reading local ids or deleting stale entries fails.
///
/// Listing failing guard not error: `Ok`, `completed` false, nothing deleted.
///
/// [`Error::Transport`]: crate::error::Error::Transport
/// [`Error::Http`]: crate::error::Error::Http
/// [`Error::Protocol`]: crate::error::Error::Protocol
/// [`Error::Db`]: crate::error::Error::Db
pub async fn reconcile(db: &mut Database, client: &MinifluxClient) -> Result<Reconcile> {
    // Server caps `limit` at 1000, `EntriesQuery` clamps to it. Larger ask
    // silently yields 1000, then "short page = done" fires on first page:
    // reconcile never pages, total check rejects every corpus over 1000. Use
    // real cap so compared page size = applied page size.
    const PAGE: u32 = 1000;

    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut offset = 0u32;
    let mut declared_total: Option<i64> = None;

    loop {
        let query = EntriesQuery::default().with_limit(PAGE);
        let page = client.entry_ids(&query, offset).await?;
        if declared_total.is_none() {
            declared_total = Some(page.total);
        }
        let count = page.entry_ids.len();
        seen.extend(page.entry_ids);

        if count < PAGE as usize {
            break;
        }
        offset = offset.saturating_add(PAGE);
        if u64::from(offset) > 5_000_000 {
            // Don't page forever against server that never shortens.
            tracing::warn!("aborting deletion reconcile: the id listing never ended");
            return Ok(Reconcile::aborted());
        }
    }

    // Guard. `/v1/entries/ids` pages by offset over `id DESC`, unstable under
    // concurrent writes: insert shifts window, id falls through crack. Torn
    // listing deletes live entries: collected count must match first page's
    // declared total.
    //
    // Missing or NEGATIVE total disables guard, doesn't pass it. Else server
    // (hostile, buggy, cached proxy body) answering `{"total": -1, "entry_ids":
    // []}` deletes whole mirror plus every pending outbox row. Uncheckable
    // value untrusted.
    let Some(total) = declared_total.filter(|t| *t >= 0) else {
        tracing::warn!("aborting deletion reconcile: the server declared no usable total");
        return Ok(Reconcile::aborted());
    };
    if i64::try_from(seen.len()).ok() != Some(total) {
        tracing::warn!(
            declared = total,
            collected = seen.len(),
            "aborting deletion reconcile: the id listing was torn by concurrent writes"
        );
        return Ok(Reconcile::aborted());
    }

    let local = store::local_entry_ids(db.conn())?;
    let stale: Vec<EntryId> = local
        .into_iter()
        .filter(|id| !seen.contains(&id.get()))
        .collect();
    if stale.is_empty() {
        return Ok(Reconcile {
            completed: true,
            deleted: 0,
        });
    }

    let removed = stale.len();
    db.with_tx(|tx| {
        for id in &stale {
            store::delete_entry(tx, *id)?;
        }
        Ok(())
    })?;
    Ok(Reconcile {
        completed: true,
        deleted: removed,
    })
}

/// Deletion reconcile outcome.
///
/// `completed` ≠ "deleted something": aborted reconcile deletes nothing, must
/// NOT be recorded as run, else periodic backstop deferred a full interval on
/// unchecked mirror.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconcile {
    /// `true` when the listing passed the guard and stale entries were removed.
    pub completed: bool,
    /// Local entries deleted because the server no longer lists them.
    pub deleted: usize,
}

impl Reconcile {
    #[must_use]
    fn aborted() -> Self {
        Reconcile {
            completed: false,
            deleted: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skew_is_large_enough_to_be_worth_having() {
        // Direction checked against real pass:
        // tests/sync_pull.rs::the_cursor_comes_from_the_servers_clock_minus_a_skew.
        // Here: only whether constant is sensible size. Too small misses mutation
        // during pass; too large re-reads hours every sync.
        assert!(
            (30..=300).contains(&CURSOR_SKEW_SECS),
            "a cursor skew of {CURSOR_SKEW_SECS}s is outside the range that trades \
             re-reading against missing a concurrent mutation"
        );
    }

    /// Compile-time: both sides constants; runtime assert over consts checked
    /// too late (clippy agrees).
    ///
    /// 2.3.x answers 400 for `limit > 1000`; `limit=0` = UNLIMITED on 2.2.x:
    /// full-corpus dump into phone.
    const _: () = assert!(PAGE_SIZE <= 1000 && PAGE_SIZE > 0);
}
