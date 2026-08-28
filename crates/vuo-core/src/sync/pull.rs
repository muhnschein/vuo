//! Pulling server state into the mirror.
//!
//! # The cursor
//!
//! §11's first open question is *which query parameters give a reliable
//! "changed since" pull without gaps or unbounded re-fetching, and how are
//! server-side deletions detected.* The answer, from the server's own source:
//!
//! **`changed_after` for the window, and a keyset on `id` for the paging.**
//! Not `offset`. The server's generated `ORDER BY` carries exactly one
//! expression and no id tiebreaker, so every order except `id` is *unstable*:
//! a single mark-all-as-read stamps thousands of rows with an identical
//! `changed_at`, and paging through them by offset silently skips and
//! duplicates. `order=id&direction=asc&after_entry_id=N` compiles to
//! `e.id > N` over a primary key — a true keyset cursor with no ties.
//!
//! **Why there are no gaps.** An entry mutated *during* a pass either has
//! `id > last_id` (so this pass sees it) or `id <= last_id` (so it does not).
//! In the second case its `changed_at` is at least the pass start, which is
//! after the cursor the pass will persist, so the next pass catches it. The
//! overlap is bounded by the skew constant rather than by the corpus size,
//! which is what keeps the re-fetch from being unbounded.
//!
//! **Why the cursor comes from the server's clock.** The comparison happens
//! server-side, and a phone's clock is not trustworthy — it can be hours out,
//! or jump when the user travels. The pass reads the `Date` header of its first
//! response and persists that minus [`CURSOR_SKEW_SECS`], covering clock skew
//! and transactions whose `now()` predates their commit visibility.
//!
//! **The cursor is persisted only after the whole pass commits.** A crash
//! mid-pass replays from the old cursor, which is safe because upserts are
//! idempotent, and is the only choice that cannot lose an entry.
//!
//! # What the cursor cannot see
//!
//! Two blind spots, both real, both handled elsewhere rather than papered over:
//!
//! 1. **Deletions.** On servers from 2.3.0 an entry is hard-deleted with no
//!    API-observable trace; before that it became `status=removed`. Neither is
//!    reachable through `changed_after`. See [`reconcile`].
//! 2. **Content edits.** A feed refresh that rewrites an entry's title or body
//!    does *not* bump `changed_at`. So a `changed_after` pull never re-fetches
//!    an edited body for an entry already held. Vuo refreshes content on user
//!    open rather than pretending the cursor covers it.

use crate::api::{convert, EntriesQuery, MinifluxClient};
use crate::db::{store, Database};
use crate::error::Result;
use crate::model::EntryId;

/// Seconds subtracted from the server's clock when persisting a cursor.
///
/// Covers clock skew between responses and transactions whose `now()` was
/// taken before they became visible. Re-seeing an entry is free (upserts are
/// idempotent); missing one is not, so this errs generously.
pub const CURSOR_SKEW_SECS: i64 = 60;

/// Entries requested per page. Well under the server's 1000 cap, and sized so
/// that a failed page is cheap to retry over a phone connection.
pub const PAGE_SIZE: u32 = 250;

/// Safety valve on the number of pages one pass will fetch.
///
/// A pass that somehow fails to advance must terminate rather than loop
/// forever against the user's data allowance.
const MAX_PAGES_PER_PASS: usize = 400;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullOutcome {
    pub upserted: usize,
    pub rejected: usize,
    /// Entries a pre-2.3 server reported as `removed` and that were deleted
    /// locally as a result.
    pub removed: usize,
    pub pages: usize,
    /// The cursor to persist if the whole pass succeeds.
    pub next_cursor: Option<i64>,
}

/// Pull categories and feeds. Cheap, and needed before entries so that an
/// entry's feed exists when the UI joins against it.
pub async fn taxonomy(db: &mut Database, client: &MinifluxClient, generation: i64) -> Result<()> {
    let categories = client.categories().await?;
    let feeds = client.feeds().await?;

    // Convert outside the transaction: a per-item rejection should not roll
    // back the items that were fine.
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

    // A feed that vanished from the listing was unsubscribed, on this device or
    // another. Its entries go with it: they are unreachable in the UI and would
    // otherwise accumulate forever. This is the cheapest and most common
    // deletion case, and it costs no extra request.
    let live: std::collections::HashSet<i64> = feeds.iter().map(|f| f.id.get()).collect();
    let local = store::feeds(db.conn())?;

    // An EMPTY feed listing is never taken as "the user unsubscribed from
    // everything". A reverse proxy serving a stale cached `[]`, or a partial
    // response, would otherwise delete the entire mirror -- and unsubscribing
    // from every feed at once is not a real workflow, whereas a cached empty
    // body is a real failure mode. If the user genuinely has no feeds, there
    // is nothing local to delete anyway.
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

/// One incremental entry pass.
///
/// Returns the cursor to persist; the caller stores it only after everything
/// else in the sync has committed.
pub async fn entries(
    db: &mut Database,
    client: &MinifluxClient,
    cursor: Option<i64>,
    generation: i64,
) -> Result<PullOutcome> {
    let mut outcome = PullOutcome::default();
    let mut after: Option<EntryId> = None;
    let mut server_now: Option<i64> = None;
    // Set when the pass stops early. The cursor must NOT advance in that case:
    // it would mark as "seen" a window the pass never finished reading, and
    // every entry beyond the stopping point would be skipped forever.
    let mut incomplete = false;

    loop {
        if outcome.pages >= MAX_PAGES_PER_PASS {
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
        outcome.pages += 1;

        // Anchor to the FIRST response's Date: later pages happen after
        // mutations that this pass will not see, and using a later clock
        // reading would skip them.
        if server_now.is_none() {
            server_now = date.map(|d| d.timestamp());
        }

        let count = page.entries.len();
        // `total` is deliberately not used as a loop bound: with after_entry_id
        // set it counts only rows ahead of the cursor and shrinks every page.
        let last_id = page.entries.last().map(|e| EntryId(e.id));

        let decoded = convert::entries(page.entries);
        for e in &decoded.rejected {
            tracing::debug!(error = %e, "dropped an unusable entry");
        }
        outcome.rejected += decoded.rejected.len();
        outcome.upserted += decoded.valid.len();
        outcome.removed += decoded.removed.len();

        db.with_tx(|tx| {
            for e in &decoded.valid {
                store::upsert_entry(tx, e, generation)?;
            }
            // A pre-2.3 server's soft delete. This is the only deletion signal
            // such a server gives, and it arrives through the ordinary cursor.
            for id in &decoded.removed {
                store::delete_entry(tx, *id)?;
            }
            Ok(())
        })?;

        // A short page means the end of the window. Note this uses the raw
        // returned count, not the count that survived validation: a page made
        // entirely of rejected entries is still a full page and paging must
        // continue past it.
        if count < PAGE_SIZE as usize {
            break;
        }
        match last_id {
            Some(id) if Some(id) != after => after = Some(id),
            // Either a full page with no last id, or a server whose
            // `after_entry_id` did not advance the window -- both mean the
            // next request would repeat this one. Stop, and do not pretend the
            // window was covered.
            _ => {
                incomplete = true;
                break;
            }
        }
    }

    outcome.next_cursor = if incomplete {
        // Leave the cursor where it was. The next pass re-reads this window,
        // which costs bandwidth; advancing it would cost entries.
        None
    } else {
        server_now
            .map(|now| now - CURSOR_SKEW_SECS)
            // Falling back to the local clock is a compromise, not a
            // preference: without a Date header there is nothing better, and
            // keeping the old cursor forever would re-pull the same window
            // every sync.
            .or_else(|| Some(chrono::Utc::now().timestamp() - CURSOR_SKEW_SECS))
    };

    Ok(outcome)
}

/// Cheap per-feed divergence check.
///
/// `GET /v1/feeds/counters` is one small request and answers "does any feed
/// hold a different number of entries than I think it does". Feeds whose
/// counts agree are skipped entirely, which is what keeps the common case at
/// near-zero bandwidth. Only when something diverges is a full id reconcile
/// worth its cost.
///
/// Returns the feed ids that disagree.
pub async fn diverging_feeds(db: &Database, client: &MinifluxClient) -> Result<Vec<i64>> {
    let counters = client.counters().await?;
    let local = store::entry_counts_by_feed(db.conn())?;

    let mut server: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for (key, value) in counters.reads.iter().chain(counters.unreads.iter()) {
        if let Ok(id) = key.parse::<i64>() {
            // saturating, not `+=`. These are numbers chosen by someone else
            // (§9.2), and two i64::MAX counts for the same feed panicked on
            // overflow in a debug build -- a reachable panic on foreign input,
            // which §9.5 forbids outright because unwinding into Qt's C++
            // frames is undefined behaviour.
            let slot = server.entry(id).or_insert(0);
            *slot = slot.saturating_add(*value);
        }
    }

    let mut diverging = Vec::new();
    for (feed_id, local_count) in &local {
        // A feed absent from the counters response holds zero entries there.
        let server_count = server.get(feed_id).copied().unwrap_or(0);
        if server_count != *local_count {
            diverging.push(*feed_id);
        }
    }
    diverging.sort_unstable();
    Ok(diverging)
}

/// Full deletion reconciliation via `GET /v1/entries/ids`.
///
/// Requires a server from 2.3.2; check
/// [`crate::model::ServerVersion::has_entry_ids_endpoint`] first.
///
/// # The correctness guard
///
/// This endpoint pages by `offset` over an `id DESC` ordering, which is *not*
/// stable under concurrent inserts and deletes — a concurrent write shifts the
/// window and an id can fall through the crack. Acting on that directly would
/// delete a live entry.
///
/// So the accumulated id count is checked against the `total` the first page
/// reported, and a mismatch **aborts the reconcile** rather than deleting
/// anything. A reconcile that does not run is a cache that stays slightly
/// stale; a reconcile that runs on a torn listing destroys the user's data.
pub async fn reconcile(db: &mut Database, client: &MinifluxClient) -> Result<Reconcile> {
    // The server caps `limit` at 1000 and `EntriesQuery` clamps to it, so
    // asking for 10_000 silently yielded 1000 -- and the loop's
    // "short page means done" test then fired on the very first page. The
    // reconcile therefore never paged at all, and on any corpus over 1000
    // entries the total check below rejected it every single time. Use the
    // real cap so the page-size the loop compares against is the page-size the
    // server actually applies.
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
            // Refuse to page forever against a server that never shortens.
            tracing::warn!("aborting deletion reconcile: the id listing never ended");
            return Ok(Reconcile::aborted());
        }
    }

    // The guard. `/v1/entries/ids` pages by offset over an `id DESC` ordering,
    // which is not stable under concurrent writes: an insert shifts the window
    // and an id can fall through the crack. Acting on a torn listing deletes
    // live entries, so the collected count must match the total the first page
    // declared.
    //
    // A missing or NEGATIVE total disables the guard rather than passing it.
    // The previous form (`total >= 0 && collected != total`) let a negative
    // value skip the check entirely, so a server -- hostile, buggy, or a proxy
    // serving a cached body -- answering `{"total": -1, "entry_ids": []}` would
    // have deleted the user's entire mirror along with every pending outbox
    // row. A value that cannot be checked is not a value that can be trusted.
    let Some(total) = declared_total.filter(|t| *t >= 0) else {
        tracing::warn!("aborting deletion reconcile: the server declared no usable total");
        return Ok(Reconcile::aborted());
    };
    if seen.len() as i64 != total {
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

/// The outcome of a deletion reconcile.
///
/// `completed` is not the same as "deleted something": an aborted reconcile
/// deletes nothing and must NOT be recorded as having run, or the periodic
/// backstop is deferred for a full interval on a mirror that was never
/// actually checked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconcile {
    pub completed: bool,
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
    fn the_skew_is_subtracted_not_added() {
        // Adding it would move the cursor into the future and skip the window
        // it was supposed to overlap.
        let server_now = 1_000_000i64;
        let cursor = server_now - CURSOR_SKEW_SECS;
        assert!(cursor < server_now);
        assert_eq!(cursor, 999_940);
    }

    /// Compile-time, because both sides are constants: a runtime assertion
    /// over two consts is checked long after the mistake could have been
    /// caught, and clippy rightly points that out.
    ///
    /// 2.3.x answers 400 for `limit > 1000`, and `limit=0` means UNLIMITED on
    /// 2.2.x -- a full-corpus dump into a phone.
    const _: () = assert!(PAGE_SIZE <= 1000 && PAGE_SIZE > 0);
}
