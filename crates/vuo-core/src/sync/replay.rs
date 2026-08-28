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
            Err(e) if e.is_permanently_rejected() => {
                // The server rejected the payload itself, so resending it will
                // be rejected identically forever. Dropping is the least-bad
                // option: keeping it would retry endlessly and block every
                // later intent behind it.
                //
                // Note the failure is NOT recorded on the row first: `discard`
                // deletes the row, taking the `last_error` with it, so writing
                // one would be theatre. The log line below is where this
                // actually becomes visible, and `ReplayOutcome::dropped`
                // carries the count to the UI.
                db.with_tx(|tx| outbox::discard(tx, &batch))?;
                outcome.dropped += batch.entry_ids.len();
                tracing::warn!(
                    error = %e,
                    count = batch.entry_ids.len(),
                    ids = ?batch.entry_ids,
                    "DROPPED queued user actions: the server rejected the request permanently"
                );
            }
            Err(e) => {
                // Everything else stays queued -- including failures that are
                // not "transient" in the retry sense, such as a refused
                // redirect or an oversized response. Those mean the server is
                // misconfigured or hostile, NOT that the user's intent is
                // invalid, and discarding marks and stars because someone
                // typed the wrong server URL is not a trade this app makes.
                // They wait for a human to fix the configuration.
                db.with_tx(|tx| outbox::record_failure(tx, &batch, &e.to_string()))?;
                outcome.deferred += batch.entry_ids.len();
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

/// Whether a batch would be DISCARDED, losing the user's intent.
///
/// Separate from [`should_retry`] on purpose: the two are not complements, and
/// treating them as such is what made a misconfigured server URL silently
/// delete queued marks and stars.
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
    fn revoked_credentials_stop_the_flush() {
        for status in [401, 403] {
            assert!(!should_retry(&http(status)));
            assert!(http(status).is_auth_failure());
        }
    }

    #[test]
    fn only_a_server_side_rejection_ever_discards_user_intent() {
        // Regression: `is_transient()` is false for a refused redirect and for
        // an oversized body, and the flush used to drop anything that was not
        // transient. So pointing Vuo at a URL that redirects off-origin --
        // a typo, a moved instance, a captive portal -- silently destroyed
        // every queued mark and star.
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

        // A genuine payload rejection is still dropped, or it would block the
        // queue forever.
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
        assert!(
            outbox::batches(&[]).is_empty(),
            "an empty entry_ids list is a hard 400"
        );
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
