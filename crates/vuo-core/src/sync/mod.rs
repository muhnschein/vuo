//! Sync engine.
//!
//! One pass, in order:
//!
//! 1. **Flush outbox.** Before pulling: user's actions reach server first, pull
//!    already reflects them, echo (every mutation bumps `changed_at`) lands as
//!    no-op, not value racing local one.
//! 2. **Pull taxonomy** (categories, feeds): entry's feed exists before entry;
//!    unsubscribed feed's entries dropped.
//! 3. **Pull entries incrementally**, keyset-paginated (see [`pull`]).
//! 4. **Check divergence** with one cheap counters request; reconcile deletions
//!    only if something disagrees or enough time passed.
//! 5. **Fetch bounded batch of missing icons**, never all at once.
//! 6. **Persist cursor**, last, only if all above succeeded.
//!
//! No transaction held across await. Each step: network I/O, then short
//! transaction. Transaction open during request blocks UI readers as long as
//! signal takes: opposite of §5 (UI never waits on network).
//!
//! # Why pass is as short as possible
//!
//! Every step = request; on phone cost is radio, not bytes. Idle modem must
//! reconnect to send, stays up for tail timer after: battery cost tracks PASS
//! DURATION, not transfer size.
//!
//! Two consequences, both done above:
//!
//! - **Ask only for what changes.** Server version gates two request rules and
//!   one endpoint's existence, changes only on Miniflux upgrade. Cached in
//!   mirror, re-asked once a day.
//! - **Ask only when answer is read.** Counters request = only evidence a
//!   server-side deletion leaves: kept on quiet passes (its purpose). Skipped
//!   where nothing could act on it: reconcile already due, or server without id
//!   listing.
//!
//! One NOT done, doesn't pay yet. Categories and feeds independent, icons too;
//! sequential awaits hold radio for sum of latencies, not max. But `reqwest`
//! built without `http2`: concurrent requests can't share connection; each =
//! extra TCP connection and TLS handshake, pool always cold between passes.
//! Saving tens of ms from radio event with multi-second tail, at cost of extra
//! handshake = loss. HTTP/2 first; concurrency after.

pub mod pull;
pub mod replay;

use crate::api::{decode_icon, IconLimits, MinifluxClient};
use crate::db::{store, Database};
use crate::error::Result;
use crate::model::ServerVersion;

/// Tuning for a [`sync`] pass.
#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    /// How often to run full deletion reconcile even when nothing looks wrong.
    /// Deletions invisible to cursor; periodic sweep only backstop.
    pub reconcile_interval_secs: i64,
    /// Icons fetched per pass.
    ///
    /// §11 thundering herd on first sync: 200 feeds fetch icons over several
    /// passes, not 200 connections behind first screen.
    pub icons_per_pass: i64,
    /// Skip the outbox flush (used by a read-only refresh).
    pub skip_replay: bool,
    /// How long read, unfavourited article kept locally, seconds.
    ///
    /// `None` (default) keeps everything; existing install keeps that unless
    /// owner asks. Never pruned whatever this says: [`store::prune_entries`].
    pub retention_secs: Option<i64>,
    /// How long recorded server version trusted before re-asking, seconds.
    ///
    /// Version changes when owner upgrades. Asking every pass = one request per
    /// pass for same answer; daily = one in twenty-four at hourly interval.
    /// Stale worst case: one pass uses previous era's rules against
    /// just-upgraded server; next pass corrects, cursor designed to survive.
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

/// What one [`sync`] pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// The outbox flush. All zero when `skip_replay` was set.
    pub replay: replay::ReplayOutcome,
    /// The incremental entry pull.
    pub pull: pull::PullOutcome,
    /// Local entries deleted by a deletion reconcile.
    pub entries_deleted: usize,
    /// Feed icons fetched, validated and stored.
    pub icons_fetched: usize,
    /// Entries dropped by the retention policy this pass.
    pub entries_pruned: usize,
    /// `true` when a deletion reconcile ran to completion.
    pub reconciled: bool,
    /// Server version pass used: cached, fresh, or `"unknown"` when server
    /// won't say. `None` only when pass stopped after flush on credential
    /// failure.
    pub server_version: Option<String>,
}

/// Run one full sync pass.
///
/// Credential failure during flush not error: pass stops, returns `Ok` with
/// [`ReplayOutcome::auth_failed`] set. Failures fetching server version,
/// counters, icon absorbed.
///
/// # Errors
///
/// - [`Error::Transport`], [`Error::Http`] or [`Error::Protocol`] when pulling
///   categories, feeds, entries fails, or reconcile's id listing fails.
/// - [`Error::Db`] when mirror read/write fails at any step.
///
/// Cursor saved only at end: next pass re-reads window failed one didn't
/// finish.
///
/// [`Error::Transport`]: crate::error::Error::Transport
/// [`Error::Http`]: crate::error::Error::Http
/// [`Error::Protocol`]: crate::error::Error::Protocol
/// [`Error::Db`]: crate::error::Error::Db
/// [`ReplayOutcome::auth_failed`]: replay::ReplayOutcome::auth_failed
pub async fn sync(
    db: &mut Database,
    client: &MinifluxClient,
    options: SyncOptions,
) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let state = store::sync_state(db.conn())?;
    let generation = state.sync_generation.saturating_add(1);
    let now = chrono::Utc::now().timestamp();

    // 1. User's own actions first.
    if !options.skip_replay {
        report.replay = replay::flush(db, client).await?;
        if report.replay.auth_failed {
            // Pull would fail same way. Surface credential problem, don't bury
            // under network error.
            return Ok(report);
        }
    }

    // Version gating: request rules and one endpoint's existence depend on it;
    // guessing high calls endpoints that 404.
    //
    // Cached in mirror: changes only on Miniflux upgrade. See
    // `SyncOptions::server_version_ttl_secs`.
    let cached_version = state
        .server_version_checked_at
        .filter(|checked| now.saturating_sub(*checked) < options.server_version_ttl_secs)
        .and(state.server_version.as_deref())
        .and_then(ServerVersion::parse);
    let (version, version_checked_at) = match cached_version {
        Some(version) => (version, state.server_version_checked_at),
        None => match client.version().await {
            Ok(version) => (version, Some(now)),
            // Failed check ≠ answer. Last recorded version still best guess:
            // oldest-supported fallback would skip due reconcile and record
            // wrong era for unchanged server. Check time untouched: next pass
            // asks again.
            Err(e) => {
                tracing::warn!(error = %e, "could not check the server version");
                let recorded = state
                    .server_version
                    .as_deref()
                    .and_then(ServerVersion::parse);
                (
                    recorded.unwrap_or_default(),
                    state.server_version_checked_at,
                )
            }
        },
    };
    report.server_version = Some(version.raw.clone());

    // 2 & 3.
    pull::taxonomy(db, client, generation).await?;
    report.pull = pull::entries(db, client, state.cursor_changed_after, generation).await?;

    // 4. Deletions invisible to cursor: need own signal.
    let due = state
        .last_full_reconcile_at
        .is_none_or(|last| now.saturating_sub(last) >= options.reconcile_interval_secs);
    // Counters request spent only where answer can change this pass.
    //
    // CANNOT skip on quiet pass (tempting, wrong): server-side deletion
    // invisible to cursor by construction (`pull`'s "What the cursor cannot
    // see"); pass that pulled nothing = exactly where count mismatch is only
    // evidence. Gating on pull activity silently disables deletion backstop.
    //
    // Skippable where answer unread: reconcile already due runs regardless;
    // server without id listing can't act on divergence. Modest: one
    // request/day on current server, one/pass on pre-2.3.
    let worth_asking = !due && version.has_entry_ids_endpoint();
    let diverging = if worth_asking {
        pull::diverging_feeds(db, client).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    if version.has_entry_ids_endpoint() && (due || !diverging.is_empty()) {
        let outcome = pull::reconcile(db, client).await?;
        report.entries_deleted = outcome.deleted;
        // Only completed reconcile counts. Recording aborted one stamps
        // `last_full_reconcile_at`, defers backstop a whole interval on
        // unchecked mirror. Aborts not rare: any concurrent write tears
        // listing.
        report.reconciled = outcome.completed;
    }

    // 5.
    report.icons_fetched = fetch_icons(db, client, options.icons_per_pass).await?;

    // 6. Retention, last mirror-changing step.
    //
    // After pull and reconcile: pruning first deletes rows pass is about to
    // re-write; before reconcile shrinks local side of comparison still
    // running.
    //
    // Off unless user enabled. Mirror holding everything = disk problem, not
    // correctness; don't start deleting reader's articles because of an update.
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

    // 7. Only now, everything above committed.
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
    };
    db.with_tx(|tx| store::set_sync_state(tx, &next))?;

    Ok(report)
}

/// Which deletion regime server implements.
///
/// For diagnosis. Miniflux changed at 2.3.0: before, soft `status=removed`
/// exposed by API; from 2.3.0, hard delete, no observable trace.
fn era_label(version: &ServerVersion) -> &'static str {
    if version.has_entry_ids_endpoint() {
        "hard-delete-with-id-listing"
    } else if version.enforces_entry_limit_cap() {
        "hard-delete"
    } else {
        "legacy-soft-delete"
    }
}

/// Fetch up to `limit` missing feed icons.
async fn fetch_icons(db: &mut Database, client: &MinifluxClient, limit: i64) -> Result<usize> {
    if limit <= 0 {
        return Ok(0);
    }
    let wanted = store::feeds_missing_icons(db.conn(), limit)?;
    let mut fetched = 0usize;

    for (feed_id, _icon_id) in wanted {
        let wire = match client.feed_icon(feed_id.get()).await {
            Ok(w) => w,
            Err(e) => {
                // Missing or broken icon never fails sync, but IS reason to
                // stop asking, else this feed starves every one behind it each
                // sync.
                tracing::debug!(feed = %feed_id, error = %e, "could not fetch a feed icon");
                db.with_tx(|tx| store::record_icon_failure(tx, feed_id))?;
                continue;
            }
        };
        // Validated by content, not by claimed type (§9.3).
        match decode_icon(&wire, IconLimits::default()) {
            Ok(icon) => {
                db.with_tx(|tx| {
                    store::upsert_icon(tx, &icon)?;
                    store::clear_icon_failures(tx, feed_id)
                })?;
                fetched = fetched.saturating_add(1);
            }
            Err(e) => {
                tracing::debug!(feed = %feed_id, error = %e, "rejected a feed icon");
                db.with_tx(|tx| store::record_icon_failure(tx, feed_id))?;
            }
        }
    }
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
