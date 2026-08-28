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

pub mod pull;
pub mod replay;

use crate::api::{decode_icon, IconLimits, MinifluxClient};
use crate::db::{store, Database};
use crate::error::Result;
use crate::model::ServerVersion;

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
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            reconcile_interval_secs: 24 * 60 * 60,
            icons_per_pass: 8,
            skip_replay: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub replay: replay::ReplayOutcome,
    pub pull: pull::PullOutcome,
    pub entries_deleted: usize,
    pub icons_fetched: usize,
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
    let version = client.version().await.unwrap_or_default();
    report.server_version = Some(version.raw.clone());

    // 2 & 3.
    pull::taxonomy(db, client, generation).await?;
    report.pull = pull::entries(db, client, state.cursor_changed_after, generation).await?;

    // 4. Deletions are invisible to the cursor, so they need their own signal.
    let due = state
        .last_full_reconcile_at
        .map_or(true, |last| now - last >= options.reconcile_interval_secs);
    let diverging = pull::diverging_feeds(db, client).await.unwrap_or_default();
    if version.has_entry_ids_endpoint() && (due || !diverging.is_empty()) {
        report.entries_deleted = pull::reconcile(db, client).await?;
        report.reconciled = true;
    } else if !diverging.is_empty() {
        // An older server has no cheap id listing. Rather than re-pulling the
        // whole corpus on every divergence, note it and let the periodic full
        // refresh handle it; a stale read entry is a much smaller problem than
        // a phone that re-downloads everything whenever a feed is trimmed.
        tracing::info!(
            feeds = diverging.len(),
            "feeds diverge from the server's counts, but this server has no id listing endpoint"
        );
    }

    // 5.
    report.icons_fetched = fetch_icons(db, client, options.icons_per_pass).await?;

    // 6. Only now, with everything above committed.
    let next = store::SyncState {
        cursor_changed_after: report.pull.next_cursor.or(state.cursor_changed_after),
        sync_generation: generation,
        last_full_reconcile_at: if report.reconciled { Some(now) } else { state.last_full_reconcile_at },
        server_era: Some(era_label(&version).to_owned()),
        server_version: report.server_version.clone(),
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
                // A missing or broken icon is never a reason to fail a sync.
                tracing::debug!(feed = %feed_id, error = %e, "could not fetch a feed icon");
                continue;
            }
        };
        // Validated by content, not by claimed type (§9.3).
        match decode_icon(&wire, IconLimits::default()) {
            Ok(icon) => {
                db.with_tx(|tx| store::upsert_icon(tx, &icon))?;
                fetched += 1;
            }
            Err(e) => tracing::debug!(feed = %feed_id, error = %e, "rejected a feed icon"),
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
        assert!(o.icons_per_pass > 0 && o.icons_per_pass <= 32, "avoid a thundering herd");
        assert!(o.reconcile_interval_secs >= 3600, "a full reconcile is not cheap");
        assert!(!o.skip_replay, "the user's own actions must be sent by default");
    }
}
