//! Replaying outbox against server.
//!
//! Hard part: knowing what failure means, not sending. Every payload absolute
//! set (see [`crate::db::outbox`]): no partial-application hazard; retry rule =
//! *could this same request ever succeed?*
//!
//! - **4xx, except 429**: no. 400 = malformed payload, rejected identically
//!   forever. Retrying loops; batch dropped and reported.
//! - **5xx, 429, transport errors**: yes. Incl. ambiguous timeout where server
//!   may have applied change: resending absolute value = no-op (why outbox has
//!   this shape).
//! - **401/403**: no, and stop whole flush. Key revoked or account changed;
//!   every later batch fails same way; hammering helps nobody.

use crate::api::{EntryMutation, MinifluxClient};
use crate::db::outbox::{self, DesiredValue};
use crate::db::Database;
use crate::error::{Error, Result};

/// What a [`flush`] did with the outbox. Counts are entries, not batches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayOutcome {
    /// Intents the server accepted and that were cleared locally.
    pub confirmed: usize,
    /// Intents left queued for a later attempt.
    pub deferred: usize,
    /// Intents dropped because the server will never accept them.
    pub dropped: usize,
    /// Set when the flush stopped early because credentials failed.
    pub auth_failed: bool,
}

/// Send every pending intent, oldest first.
///
/// No transaction across await: batch sent, then short transaction records
/// result. Open across network call blocks UI readers for request duration
/// (seconds on poor signal).
///
/// Server refusal not error: each batch confirmed, deferred or dropped; counts
/// say which. Credential failure stops flush early with `auth_failed` set.
///
/// # Errors
///
/// [`Error::Db`] when reading outbox or recording batch result fails. Recorded
/// batches stay recorded.
pub async fn flush(db: &mut Database, client: &MinifluxClient) -> Result<ReplayOutcome> {
    let pending = outbox::pending(db.conn())?;
    let batches = outbox::batches(&pending);
    let mut outcome = ReplayOutcome::default();

    for batch in batches {
        let mutation = match batch.value {
            DesiredValue::Status(status) => EntryMutation::Status(status),
            DesiredValue::Starred(starred) => EntryMutation::Starred(starred),
        };

        match client.update_entries(&batch.entry_ids, mutation).await {
            Ok(()) => {
                // Compare-and-delete: re-toggle during request leaves row with
                // different value; must survive.
                let cleared = db.with_tx(|tx| outbox::confirm(tx, &batch))?;
                outcome.confirmed = outcome.confirmed.saturating_add(cleared);
                outcome.deferred = outcome
                    .deferred
                    .saturating_add(batch.entry_ids.len().saturating_sub(cleared));
            }
            Err(e) if e.is_auth_failure() => {
                // Every remaining batch would fail identically.
                db.with_tx(|tx| outbox::record_failure(tx, &batch, &e.to_string()))?;
                outcome.deferred = outcome.deferred.saturating_add(batch.entry_ids.len());
                outcome.auth_failed = true;
                return Ok(outcome);
            }
            Err(e) if would_discard(&e) => {
                // Server rejected payload itself; resend rejected identically
                // forever. Dropping least bad: keeping retries endlessly,
                // blocks every later intent.
                //
                // Failure NOT recorded on row first: `discard` deletes row with
                // its `last_error`. Visible via log line below;
                // `ReplayOutcome::dropped` carries count to UI.
                db.with_tx(|tx| outbox::discard(tx, &batch))?;
                outcome.dropped = outcome.dropped.saturating_add(batch.entry_ids.len());
                tracing::warn!(
                    error = %e,
                    count = batch.entry_ids.len(),
                    ids = ?batch.entry_ids,
                    "DROPPED queued user actions: the server rejected the request permanently"
                );
            }
            Err(e) => {
                // Everything else stays queued, incl. non-"transient" failures
                // (refused redirect, oversized response). Those = server
                // misconfigured or hostile, NOT invalid intent; no discarding
                // marks and stars over mistyped server URL. Waits for human to
                // fix config.
                db.with_tx(|tx| outbox::record_failure(tx, &batch, &e.to_string()))?;
                outcome.deferred = outcome.deferred.saturating_add(batch.entry_ids.len());

                // Both cases stay queued, DATABASE can't tell apart; user
                // debugging "sync does nothing" needs to. `should_retry` = that
                // distinction; this is its production caller.
                if should_retry(&e) {
                    tracing::debug!(
                        error = %e,
                        count = batch.entry_ids.len(),
                        "queued user actions deferred; the next pass will retry"
                    );
                } else {
                    tracing::warn!(
                        error = %e,
                        count = batch.entry_ids.len(),
                        "queued user actions are STUCK: this will not clear by \
                         retrying, and needs the server URL or configuration fixed"
                    );
                }
            }
        }
    }

    Ok(outcome)
}

/// Whether batch retried on later pass.
///
/// NOT complement of [`would_discard`]; gap is the point: neither retried nor
/// discarded → stays queued for human. Treating as complements lets mistyped
/// server URL silently delete queued marks and stars.
#[must_use]
pub fn should_retry(error: &Error) -> bool {
    !error.is_auth_failure() && error.is_transient()
}

/// Whether batch would be DISCARDED, losing user's intent.
///
/// [`flush`] matches on this directly, doesn't repeat condition: tests below
/// exercise the real rule, not parallel copy.
#[must_use]
pub fn would_discard(error: &Error) -> bool {
    !error.is_auth_failure() && error.is_permanently_rejected()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EntryId;
    use crate::redact::SafeUrl;

    fn http(status: u16) -> Error {
        Error::Http {
            status,
            endpoint: SafeUrl::from(&url::Url::parse("https://h.example/v1/entries").unwrap()),
            message: None,
        }
    }

    #[test]
    fn a_timeout_is_retried_because_replay_is_safe() {
        let timeout = Error::Transport {
            endpoint: SafeUrl::from(&url::Url::parse("https://h.example/").unwrap()),
            kind: crate::error::TransportKind::Timeout,
            detail: String::new(),
        };
        assert!(
            should_retry(&timeout),
            "an ambiguous timeout is safe to resend: the payload is an absolute set"
        );
    }

    #[test]
    fn a_malformed_request_is_not_retried_forever() {
        assert!(
            !should_retry(&http(400)),
            "a 400 will be rejected identically on replay"
        );
    }

    #[test]
    fn throttling_is_retried_but_other_client_errors_are_not() {
        assert!(should_retry(&http(429)));
        assert!(!should_retry(&http(404)));
        assert!(!should_retry(&http(422)));
    }

    #[test]
    fn server_errors_are_retried() {
        for status in [500, 502, 503, 504] {
            assert!(should_retry(&http(status)), "{status} should be retried");
        }
    }

    #[test]
    fn a_401_and_a_403_classify_as_auth_failures() {
        // Named for what it checks. `flush` early return on auth tested end to
        // end in tests/outbox_reconciliation.rs.
        for status in [401, 403] {
            assert!(!should_retry(&http(status)));
            assert!(http(status).is_auth_failure());
        }
    }

    #[test]
    fn only_a_server_side_rejection_ever_discards_user_intent() {
        // `is_transient()` false for refused redirect and oversized body;
        // dropping non-transient would make off-origin redirect (typo, moved
        // instance, captive portal) destroy every queued mark and star.
        let policy_refusals = [
            crate::error::TransportKind::RedirectRefused,
            crate::error::TransportKind::BodyTooLarge,
            crate::error::TransportKind::Tls,
            crate::error::TransportKind::Connect,
        ];
        for kind in policy_refusals {
            let e = Error::Transport {
                endpoint: SafeUrl::from(&url::Url::parse("https://h.example/").unwrap()),
                kind,
                detail: String::new(),
            };
            assert!(
                !would_discard(&e),
                "{kind} would discard the user's queued actions; it means the server is \
                 misconfigured, not that the intent is invalid"
            );
        }

        // Genuine payload rejection still dropped, else blocks queue forever.
        assert!(would_discard(&http(400)));
        assert!(would_discard(&http(422)));
        assert!(
            !would_discard(&http(429)),
            "throttling is not a payload problem"
        );
        assert!(!would_discard(&http(503)));
        assert!(
            !would_discard(&http(401)),
            "auth failure stops the flush, it does not drop"
        );
    }

    #[test]
    fn batching_a_realistic_offline_burst() {
        // 1200 marks plus few stars queued offline → five requests, not 1200.
        let mut pending: Vec<_> = (1..=1200)
            .map(|i| outbox::PendingMutation {
                entry_id: EntryId(i),
                value: DesiredValue::Status(crate::model::EntryStatus::Read),
                attempts: 0,
            })
            .collect();
        pending.push(outbox::PendingMutation {
            entry_id: EntryId(5),
            value: DesiredValue::Starred(true),
            attempts: 0,
        });

        let batches = outbox::batches(&pending);
        assert_eq!(batches.len(), 4, "3 status chunks + 1 starred chunk");
        let total: usize = batches.iter().map(|b| b.entry_ids.len()).sum();
        assert_eq!(total, 1201, "no intent may be lost in batching");
    }

    #[test]
    fn an_empty_outbox_produces_no_requests() {
        assert!(
            outbox::batches(&[]).is_empty(),
            "an empty entry_ids list is a hard 400"
        );
    }

    #[test]
    fn batch_construction_is_deterministic() {
        // Deterministic batching: failed flush replays identically, so
        // `attempts` means something.
        let pending: Vec<_> = (1..=10)
            .map(|i| outbox::PendingMutation {
                entry_id: EntryId(i),
                value: DesiredValue::Starred(i % 2 == 0),
                attempts: 0,
            })
            .collect();
        assert_eq!(outbox::batches(&pending), outbox::batches(&pending));
    }
}
