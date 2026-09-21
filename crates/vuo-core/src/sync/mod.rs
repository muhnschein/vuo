//! The sync engine.
//!
//! One pass, in this order:
//!
//! 1. **Flush the outbox.** Before pulling, not after. The user's actions
//!    reach the server first, so the pull that follows already reflects them
//!    and the echo (every mutation bumps `changed_at`) lands as a no-op rather
//!    than as a value racing the local one.
//! 2. **Pull the taxonomy** — categories and feeds — so an entry's feed exists
//!    before the entry does, and so an unsubscribed feed's entries are dropped.
//! 3. **Pull entries incrementally**, keyset-paginated (see [`pull`]).
//! 4. **Check for divergence** with one cheap counters request, and reconcile
//!    deletions only if something actually disagrees or enough time has passed.
//! 5. **Fetch a bounded batch of missing icons**, never all of them at once.
//! 6. **Persist the cursor**, last, only if everything above succeeded.
//!
//! No database transaction is ever held across an await. Each step does its
//! network I/O, then opens a short transaction. A transaction held open for the
//! duration of a request would block the UI's readers for however long the
//! phone's signal takes, which is the opposite of §5's promise that the UI
//! never waits on the network.
//!
//! # Why the pass is as short as it can be made
//!
//! Every step above is a request, and on a phone the cost of a request is not
//! its bytes: it is the radio. A cellular modem that has gone idle must
//! re-establish a connected state to send anything, and stays there for a tail
//! timer afterwards -- so what a sync pass costs the battery tracks HOW LONG
//! THE PASS IS, near enough, and not how much it transfers.
//!
//! Two things follow, and both are done above rather than described:
//!
//! - **Ask only for what changes.** The server's version gates two request
//!   rules and one endpoint's existence, but it changes when someone upgrades
//!   their Miniflux. It is cached in the mirror and re-asked once a day.
//! - **Ask only when the answer is read.** The counters request is the only
//!   evidence a server-side deletion leaves, so it survives on the quiet
//!   passes -- that is the case it exists for. It is skipped where nothing
//!   could act on it: a reconcile already due, or a server with no id
//!   listing.
//!
//! - **Ask for independent things at once, where that is free.** Categories
//!   and feeds do not depend on each other, and neither do icons, so awaiting
//!   them in turn holds the radio up for the sum of their latencies rather
//!   than the longest of them.
//!
//! That last one is conditional, and the condition is the whole of it. It pays
//! only over HTTP/2, where the requests share one connection. Over HTTP/1.1
//! each request in flight is another TCP connection and another TLS handshake,
//! and passes are far enough apart that the pool is always cold -- so the
//! saved round trip, tens of milliseconds off a radio event whose tail is
//! seconds, is bought with packets and elliptic-curve work that were not there
//! before. A loss. The pass therefore asks before it overlaps anything.
//!
//! # How the pass knows whether it can overlap
//!
//! ALPN settles it on the first response, and [`crate::api::Transport`]
//! records what it saw. But reading that from the live transport is reading it
//! one pass too late: a phone's reader is opened, synced once and closed
//! again, so a client built at launch has not yet seen a response by the time
//! the taxonomy step has to decide, and on that usage pattern the answer would
//! never arrive in time to be used.
//!
//! So the reading is carried in the mirror, in `sync_state.server_multiplexes`.
//! A pass acts on what the previous pass observed and stores what this one
//! observed, alongside the cursor and for the same reason. An instance that
//! gains or loses `h2` -- someone puts a proxy in front of it, or takes one
//! away -- corrects the mirror on the next pass, and the cost of being wrong
//! for exactly one pass is one round trip either way.
//!
//! And one thing still deliberately NOT done: [`pull::reconcile`] pages an id
//! listing by offset and could fire every page after the first together, which
//! would both shorten it and narrow the window in which a concurrent write can
//! tear the listing. It is left sequential because it runs once a day, is a
//! single page for any corpus under a thousand entries, and its abort guard is
//! the one place in the engine where being clever risks the user's data.

pub mod pull;
pub mod replay;

use futures_util::StreamExt as _;

use crate::api::{decode_icon, IconLimits, MinifluxClient};
use crate::db::{store, Database};
use crate::error::Result;
use crate::model::{FeedId, Icon, ServerVersion};

#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    /// How often to run a full deletion reconcile even when nothing looks
    /// wrong. Deletions are invisible to the cursor, so a periodic sweep is
    /// the only backstop.
    pub reconcile_interval_secs: i64,
    /// Icons fetched per pass.
    ///
    /// §11 asks how to avoid a thundering herd on first sync. This is the
    /// answer: a first sync of 200 feeds fetches icons over several passes
    /// instead of opening 200 connections behind the first screen the user
    /// sees.
    pub icons_per_pass: i64,
    /// Skip the outbox flush (used by a read-only refresh).
    pub skip_replay: bool,
    /// How long a read, unfavourited article is kept locally, in seconds.
    ///
    /// `None` -- the default -- keeps everything, which is what every version
    /// before this one did and what an existing install must keep doing unless
    /// its owner asks otherwise. See [`store::prune_entries`] for what is
    /// never pruned whatever this says.
    pub retention_secs: Option<i64>,
    /// How long a recorded server version is trusted before it is asked for
    /// again, in seconds.
    ///
    /// A version changes when the instance's owner upgrades it. Asking every
    /// pass cost one request per pass to learn the same answer; asking once a
    /// day costs one in twenty-four at an hourly interval, and the worst case
    /// of a stale answer is one pass that uses the previous era's request
    /// rules against a server that has just been upgraded -- which the next
    /// pass corrects, and which the cursor is already designed to survive.
    pub server_version_ttl_secs: i64,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            reconcile_interval_secs: 24 * 60 * 60,
            icons_per_pass: 8,
            skip_replay: false,
            retention_secs: None,
            server_version_ttl_secs: 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub replay: replay::ReplayOutcome,
    pub pull: pull::PullOutcome,
    pub entries_deleted: usize,
    pub icons_fetched: usize,
    /// Entries dropped by the retention policy this pass.
    pub entries_pruned: usize,
    pub reconciled: bool,
    pub server_version: Option<String>,
}

/// Run one full sync pass.
pub async fn sync(
    db: &mut Database,
    client: &MinifluxClient,
    options: SyncOptions,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let state = store::sync_state(db.conn())?;
    let generation = state.sync_generation.saturating_add(1);
    let now = chrono::Utc::now().timestamp();

    // 1. The user's own actions go first.
    if !options.skip_replay {
        report.replay = replay::flush(db, client).await?;
        if report.replay.auth_failed {
            // Pulling would fail the same way. Surface the credential problem
            // rather than burying it under a network error.
            return Ok(report);
        }
    }

    // Version gating: several request rules and one endpoint's existence
    // depend on it, and guessing high means calling endpoints that 404.
    //
    // Cached in the mirror, because the answer changes when the instance's
    // owner upgrades their Miniflux and not otherwise. See
    // `SyncOptions::server_version_ttl_secs`.
    let cached_version = state
        .server_version_checked_at
        .filter(|checked| now - checked < options.server_version_ttl_secs)
        .and(state.server_version.as_deref())
        .and_then(ServerVersion::parse);
    let (version, version_checked_at) = match cached_version {
        Some(version) => (version, state.server_version_checked_at),
        None => (client.version().await.unwrap_or_default(), Some(now)),
    };
    report.server_version = Some(version.raw.clone());

    // Whether this pass may put independent requests in flight together. From
    // the mirror, not from the client: see "How the pass knows whether it can
    // overlap" in the module docs.
    let multiplexes = state.server_multiplexes.unwrap_or(false);

    // 2 & 3.
    pull::taxonomy_with(db, client, generation, multiplexes).await?;
    report.pull = pull::entries(db, client, state.cursor_changed_after, generation).await?;

    // 4. Deletions are invisible to the cursor, so they need their own signal.
    let due = state
        .last_full_reconcile_at
        .is_none_or(|last| now - last >= options.reconcile_interval_secs);
    // The counters request is spent only where its answer can change what
    // this pass does.
    //
    // It CANNOT be skipped on a quiet pass, and it is worth saying why, since
    // that is the tempting version of this optimisation and it is wrong: a
    // server-side deletion is invisible to the cursor by construction (see
    // `pull`'s "What the cursor cannot see"), so a pass that pulled nothing is
    // exactly the pass where a count mismatch is the only evidence there is.
    // Gating on pull activity would quietly disable the deletion backstop.
    //
    // What it can be skipped on is the two cases where the answer is not read:
    // a reconcile already due runs whatever the counters say, and a server
    // with no id listing has nothing that could act on divergence -- there the
    // request bought a log line and a round trip. Modest: one request a day on
    // a current server, one per pass on a pre-2.3 one.
    let worth_asking = !due && version.has_entry_ids_endpoint();
    let diverging = if worth_asking {
        pull::diverging_feeds(db, client).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    if version.has_entry_ids_endpoint() && (due || !diverging.is_empty()) {
        let outcome = pull::reconcile(db, client).await?;
        report.entries_deleted = outcome.deleted;
        // Only a reconcile that actually completed counts. Recording an
        // aborted one would stamp `last_full_reconcile_at` and defer the
        // deletion backstop for a whole interval on a mirror that was never
        // checked -- and an abort is not rare: any concurrent write tears the
        // listing.
        report.reconciled = outcome.completed;
    }
    // There used to be an `else if !diverging.is_empty()` here, logging that an
    // older server's feeds diverged with no id listing to reconcile against.
    // It is gone with the request that fed it: `worth_asking` leaves
    // `diverging` empty on exactly the servers that branch was about, so
    // keeping it would be a branch that can no longer be taken. The behaviour
    // it described -- let the periodic refresh handle it, rather than
    // re-pulling the corpus -- is unchanged, because it never did anything
    // else.

    // 5.
    report.icons_fetched = fetch_icons(
        db,
        client,
        options.icons_per_pass,
        icon_concurrency(multiplexes),
    )
    .await?;

    // 6. Retention, last of the mirror-changing steps.
    //
    // After the pull and after the reconcile, not before: pruning first would
    // delete rows this pass is about to re-write, and pruning before the
    // reconcile would shrink the local side of a comparison the reconcile is
    // still making.
    //
    // Off unless the user turned it on. The mirror holding every article the
    // phone has ever seen is a disk problem, not a correctness one, and
    // deleting the reader's articles is not a thing to start doing on their
    // behalf because they installed an update.
    if let Some(keep_for) = options.retention_secs.filter(|s| *s > 0) {
        let cutoff = now.saturating_sub(keep_for);
        report.entries_pruned = db.with_tx(|tx| store::prune_entries(tx, cutoff))?;
        if report.entries_pruned > 0 {
            tracing::info!(
                pruned = report.entries_pruned,
                "dropped read articles past the retention window"
            );
        }
    }

    // 7. Only now, with everything above committed.
    let next = store::SyncState {
        cursor_changed_after: report.pull.next_cursor.or(state.cursor_changed_after),
        sync_generation: generation,
        last_full_reconcile_at: if report.reconciled {
            Some(now)
        } else {
            state.last_full_reconcile_at
        },
        server_era: Some(era_label(&version).to_owned()),
        server_version: report.server_version.clone(),
        server_version_checked_at: version_checked_at,
        // What this pass actually got, for the next one to act on. Taken here
        // rather than earlier so it reflects the last response of the pass:
        // reaching this line means requests succeeded, so it is a real
        // reading and not the pessimistic value a fresh transport starts with.
        server_multiplexes: Some(client.multiplexes()),
    };
    db.with_tx(|tx| store::set_sync_state(tx, &next))?;

    Ok(report)
}

/// Which deletion regime a server implements.
///
/// Recorded for diagnosis. Miniflux changed regimes at 2.3.0: before it,
/// deletion was a soft `status=removed` that the API exposed; from 2.3.0 an
/// entry is hard-deleted with no observable trace at all.
fn era_label(version: &ServerVersion) -> &'static str {
    if version.has_entry_ids_endpoint() {
        "hard-delete-with-id-listing"
    } else if version.enforces_entry_limit_cap() {
        "hard-delete"
    } else {
        "legacy-soft-delete"
    }
}

/// How many icon requests to keep in flight.
///
/// One unless the connection multiplexes, and the asymmetry is the point: over
/// HTTP/2 the extra requests are streams on a connection that is already open,
/// so they cost nothing but the server's time; over HTTP/1.1 each one is
/// another connection and another handshake, which is what this whole
/// arrangement exists to avoid.
///
/// Four rather than "all of them". `icons_per_pass` is eight by default, and
/// a first sync of two hundred feeds would otherwise open the batch as a
/// thundering herd against someone's self-hosted instance -- the very thing
/// §11 asks about and that batching the icons was the answer to. Four keeps
/// the batch to two round trips instead of eight while still being a request
/// rate a small server would not notice.
#[must_use]
pub fn icon_concurrency(multiplexes: bool) -> usize {
    if multiplexes {
        4
    } else {
        1
    }
}

/// Fetch up to `limit` missing feed icons, `in_flight` at a time.
///
/// The network half runs first and the mirror is written once at the end,
/// rather than a transaction per icon. That is not only about the concurrency:
/// a batch of eight icons used to be up to sixteen short transactions, each
/// one a commit the UI's readers had to wait behind, for what is ultimately
/// one screen's worth of small images.
///
/// A failure to fetch or decode is recorded, never propagated. A missing or
/// broken icon is not a reason to fail a sync -- but it IS a reason to stop
/// asking, or that feed starves every one behind it in the batch on every
/// future pass. See [`store::feeds_missing_icons`].
async fn fetch_icons(
    db: &mut Database,
    client: &MinifluxClient,
    limit: i64,
    in_flight: usize,
) -> Result<usize> {
    if limit <= 0 {
        return Ok(0);
    }
    let wanted = store::feeds_missing_icons(db.conn(), limit)?;
    if wanted.is_empty() {
        return Ok(0);
    }

    let outcomes: Vec<(FeedId, Option<Icon>)> =
        futures_util::stream::iter(wanted.into_iter().map(|(feed_id, _icon_id)| async move {
            let icon = match client.feed_icon(feed_id.get()).await {
                // Validated by content, not by claimed type (§9.3).
                Ok(wire) => match decode_icon(&wire, IconLimits::default()) {
                    Ok(icon) => Some(icon),
                    Err(e) => {
                        tracing::debug!(feed = %feed_id, error = %e, "rejected a feed icon");
                        None
                    }
                },
                Err(e) => {
                    tracing::debug!(feed = %feed_id, error = %e, "could not fetch a feed icon");
                    None
                }
            };
            (feed_id, icon)
        }))
        // `buffered`, not `buffer_unordered`: results come back in the order
        // they were asked for, so the batch writes the same rows in the same
        // order whatever the network did. Head-of-line blocking inside a
        // window of four small images is not worth the nondeterminism.
        .buffered(in_flight.max(1))
        .collect()
        .await;

    let fetched = outcomes.iter().filter(|(_, icon)| icon.is_some()).count();
    db.with_tx(|tx| {
        for (feed_id, icon) in &outcomes {
            match icon {
                Some(icon) => {
                    store::upsert_icon(tx, icon)?;
                    store::clear_icon_failures(tx, *feed_id)?;
                }
                None => store::record_icon_failure(tx, *feed_id)?,
            }
        }
        Ok(())
    })?;
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn era_labels_track_the_servers_deletion_regime() {
        let parse = |s: &str| ServerVersion::parse(s).unwrap();
        assert_eq!(era_label(&parse("2.3.2")), "hard-delete-with-id-listing");
        assert_eq!(era_label(&parse("2.3.0")), "hard-delete");
        assert_eq!(era_label(&parse("2.2.7")), "legacy-soft-delete");
    }

    #[test]
    fn nothing_is_overlapped_on_a_connection_that_cannot_multiplex() {
        // The asymmetry IS the feature. Over HTTP/1.1 a second request in
        // flight is a second TCP connection and a second TLS handshake, which
        // costs more than the round trip it saves -- so the fallback is not
        // "a bit less concurrency", it is none at all.
        assert_eq!(
            icon_concurrency(false),
            1,
            "without multiplexing, requests must take turns on the one connection"
        );
    }

    #[test]
    fn multiplexing_overlaps_icons_without_opening_the_batch_all_at_once() {
        let n = icon_concurrency(true);
        assert!(
            n > 1,
            "an HTTP/2 connection carries streams; taking turns on it wastes the pass"
        );
        // The upper bound matters as much as the lower one. `icons_per_pass`
        // is 8 and a first sync has hundreds of feeds waiting: a window as
        // wide as the batch is the thundering herd §11 asks about, aimed at
        // someone's self-hosted instance.
        assert!(
            n <= SyncOptions::default().icons_per_pass as usize / 2,
            "the window must stay well inside the batch, or the batching that \
             avoids a herd on first sync is undone by the concurrency"
        );
    }

    #[test]
    fn default_options_are_conservative() {
        let o = SyncOptions::default();
        assert!(
            o.icons_per_pass > 0 && o.icons_per_pass <= 32,
            "avoid a thundering herd"
        );
        assert!(
            o.reconcile_interval_secs >= 3600,
            "a full reconcile is not cheap"
        );
        assert!(
            !o.skip_replay,
            "the user's own actions must be sent by default"
        );
    }
}
