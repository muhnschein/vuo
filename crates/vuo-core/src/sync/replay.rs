//! Replaying the outbox against the server.
//!
//! The hard part of this is not the sending; it is knowing what a failure
//! means. Because every payload is an absolute set (see [`crate::db::outbox`]),
//! there is no partial-application hazard, and the retry rule reduces to one
//! question: *could this same request ever succeed?*
//!
//! - **4xx, except 429** — no. A 400 means the payload is malformed and will
//!   be rejected identically forever. Retrying is a loop; the batch is dropped
//!   and reported.
//! - **5xx, 429, transport errors** — yes. Including an ambiguous timeout,
//!   where the server may well have applied the change: resending an absolute
//!   value is a no-op, which is exactly why the outbox is shaped this way.
//! - **401/403** — no, and stop the whole flush. The API key has been revoked
//!   or the account changed; every subsequent batch will fail the same way and
//!   hammering the server helps nobody.

use crate::api::{EntryMutation, MinifluxClient};
use crate::db::outbox::{self, DesiredValue};
use crate::db::Database;
use crate::error::{Error, Result};

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
/// Never holds a database transaction across an await: each batch is sent, and
/// only then is a short transaction opened to record the result. Holding one
/// open across the network call would block the UI's readers for the duration
/// of a request, which on a phone with poor signal is seconds.
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
                // Compare-and-delete: a re-toggle during the request leaves a
                // row with a different value, which must survive.
                let cleared = db.with_tx(|tx| outbox::confirm(tx, &batch))?;
                outcome.confirmed += cleared;
                outcome.deferred += batch.entry_ids.len().saturating_sub(cleared);
            }
            Err(e) if e.is_auth_failure() => {
                // Every remaining batch would fail identically.
                db.with_tx(|tx| outbox::record_failure(tx, &batch, &e.to_string()))?;
                outcome.deferred += batch.entry_ids.len();
                outcome.auth_failed = true;
                return Ok(outcome);
            }
            Err(e) if e.is_transient() => {
                db.with_tx(|tx| outbox::record_failure(tx, &batch, &e.to_string()))?;
                outcome.deferred += batch.entry_ids.len();
            }
            Err(e) => {
                // Permanent. Dropping is the least-bad option: keeping it would
                // retry forever and block every later intent behind it. The
                // error is recorded first so it is visible rather than silent.
                db.with_tx(|tx| {
                    outbox::record_failure(tx, &batch, &e.to_string())?;
                    outbox::discard(tx, &batch)
                })?;
                outcome.dropped += batch.entry_ids.len();
                tracing::warn!(
                    error = %e,
                    count = batch.entry_ids.len(),
                    "dropped outbox entries the server permanently rejected"
                );
            }
        }
    }

    Ok(outcome)
}

/// Whether a batch should be retried, exposed for testing the classifier
/// without a server.
#[must_use]
pub fn should_retry(error: &Error) -> bool {
    !error.is_auth_failure() && error.is_transient()
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
        assert!(!should_retry(&http(400)), "a 400 will be rejected identically on replay");
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
    fn revoked_credentials_stop_the_flush() {
        for status in [401, 403] {
            assert!(!should_retry(&http(status)));
            assert!(http(status).is_auth_failure());
        }
    }

    #[test]
    fn batching_a_realistic_offline_burst() {
        // 1200 marks and a handful of stars, queued offline, become five
        // requests rather than 1200.
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
        assert!(outbox::batches(&[]).is_empty(), "an empty entry_ids list is a hard 400");
    }

    #[test]
    fn batch_construction_is_deterministic() {
        // Deterministic batching makes a failed flush replay identically,
        // which is what lets `attempts` mean anything.
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
