//! Error taxonomy for `vuo-core`.
//!
//! Scope constraints shape this more than ergonomics.
//!
//! **§9.5: panics not safe failure mode.** Unwinding from Rust into Qt's C++
//! frames = undefined behaviour; every foreign-input path returns `Result`.
//! `clippy::unwrap_used`, `expect_used`, `panic`, `indexing_slicing` denied
//! workspace-wide.
//!
//! **§9.1: redact in error paths.** No `#[from] reqwest::Error` variant on
//! purpose: its `Display` embeds failing URL; as error source, leaks base-URL
//! userinfo through chain. Transport failures classified into [`TransportKind`]
//! at boundary, URL reduced to [`SafeUrl`] first.
//!
//! **§9.2: one bad entry must not stall sync.** [`Error::is_item_local`]
//! separates "this item malformed" from "whole operation failed": pull drops
//! one unparseable entry, carries on.

use std::fmt;

use crate::redact::SafeUrl;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Why a request never produced an HTTP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// DNS failure, connection refused, unreachable network, or connection
    /// never established before deadline.
    ///
    /// Distinct from [`TransportKind::Timeout`] (server answered, then
    /// stalled). Different things to check: network/VPN vs server. reqwest
    /// reports connect timeout as both, so `classify` tests this first.
    Connect,
    /// TLS handshake failed, incl. certificate verification.
    ///
    /// §9.1: verification not optional, no toggle: always hard error. Private
    /// CA: user supplies that CA, no "trust anything" switch.
    Tls,
    /// A connect, read or overall-request deadline elapsed.
    Timeout,
    /// Body exceeded configured cap; read abandoned.
    ///
    /// Phone: unbounded `Vec<u8>` sized by untrusted `Content-Length` = OOM
    /// (§9.1).
    BodyTooLarge,
    /// A redirect was refused by policy: too many hops, a cross-origin jump,
    /// or a downgrade to plaintext (§9.1).
    RedirectRefused,
    /// The request was cancelled by the caller.
    Cancelled,
    /// Anything else the HTTP stack reported.
    Other,
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TransportKind::Connect => "could not reach the server",
            TransportKind::Tls => "TLS handshake or certificate verification failed",
            TransportKind::Timeout => "timed out",
            TransportKind::BodyTooLarge => "response body exceeded the size cap",
            TransportKind::RedirectRefused => "redirect refused by policy",
            TransportKind::Cancelled => "cancelled",
            TransportKind::Other => "transport error",
        };
        f.write_str(s)
    }
}

/// Everything that can go wrong in `vuo-core`.
///
/// Rules: [module documentation](crate::error). Retry/drop/surface decisions: `is_*`
/// classifiers on this type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The account or sync settings are unusable. Raised before any I/O.
    #[error("configuration error: {0}")]
    Config(String),

    /// No HTTP response was obtained.
    #[error("{kind} ({endpoint})")]
    Transport {
        /// The endpoint the request was for, with credentials and query removed.
        endpoint: SafeUrl,
        /// Why no response arrived.
        kind: TransportKind,
        /// Short, already-redacted detail. Never raw `reqwest::Error` display
        /// (carries URL).
        detail: String,
    },

    /// Server answered, not with success.
    ///
    /// `message` = server's `error_message` field when sent. Foreign text:
    /// plain text only, never markup (§9.3).
    #[error("server returned HTTP {status} for {endpoint}{}", .message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Http {
        /// The HTTP status code.
        status: u16,
        /// The endpoint that answered, with credentials and query removed.
        endpoint: SafeUrl,
        /// The server's `error_message`, when it sent one. Foreign text.
        message: Option<String>,
    },

    /// Valid HTTP, not what API promises: wrong JSON shape, absurd field, value
    /// failing domain validation.
    #[error("malformed response from server: {0}")]
    Protocol(String),

    /// One item inside otherwise good response unusable.
    ///
    /// Caller drops item, counts it, continues. §9.2: *reject the entry rather
    /// than the sync when one item is malformed.*
    #[error("unusable {kind} (id {id:?}): {reason}")]
    Item {
        /// What sort of item it was, such as `"entry"` or `"icon"`.
        kind: &'static str,
        /// The item's server-assigned id, when known.
        id: Option<i64>,
        /// Why the item was rejected.
        reason: String,
    },

    /// The local SQLite mirror failed.
    #[error("database error: {0}")]
    Db(String),

    /// Schema migration failed. Separate from [`Error::Db`]: different
    /// recovery, and failed migration must never silently drop pending outbox
    /// (§9.4).
    #[error("database migration {version} failed: {reason}")]
    Migration {
        /// Migration that failed, or database's own version when newer than
        /// this build understands.
        version: i64,
        /// What went wrong.
        reason: String,
    },

    /// Content transform refused input: too deep, too large, or malformed past
    /// usefulness (§9.2).
    #[error("could not transform article content: {0}")]
    Content(String),

    /// The operation was cancelled, e.g. the app is shutting down.
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// `true` when failure concerns one item, not operation.
    ///
    /// Pull loop: drop row and continue vs abort sync.
    #[must_use]
    pub fn is_item_local(&self) -> bool {
        matches!(self, Error::Item { .. })
    }

    /// `true` when server rejects this payload forever: drop queued intent,
    /// don't retry.
    ///
    /// Narrow on purpose: **only** non-429 client error from server.
    ///
    /// Not inverse of [`Error::is_transient`]; conflating loses user data.
    /// Refused redirect or oversized body not transient (retry replays
    /// refusal), but not payload's fault either: server misconfigured or
    /// hostile. Misdirected server URL must not look like malformed request,
    /// else outbox discards user's marks and stars. Neither transient nor 4xx →
    /// stays queued until human fixes config.
    #[must_use]
    pub fn is_permanently_rejected(&self) -> bool {
        // Explicit list, not "any 4xx except 429". Several 4xx say nothing
        // about payload, routinely produced between Vuo and Miniflux:
        //
        //   404 — a reverse proxy that does not forward /v1/, or a path typo
        //   405 — an nginx `limit_except GET` block refusing PUT
        //   408 — a proxy's own request timeout
        //   409 — a transient conflict
        //   429 — throttling, already excluded
        //
        // Web-server config must not delete queued marks and stars. Only codes
        // meaning "this body never accepted" discard work.
        matches!(
            self,
            Error::Http { status, .. }
                if matches!(status, 400 | 411 | 413 | 414 | 415 | 422 | 431)
        )
    }

    /// `true` when retrying identical request could plausibly succeed.
    ///
    /// Outbox retry classifier. Every outbox write is absolute set (see
    /// [`crate::db::outbox`]): retry after ambiguous timeout safe, no
    /// partial-application hazard. Payoff of desired-state design.
    ///
    /// 4xx permanent *except* 429: 400 = payload malformed, resend fails
    /// identically until code changes.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Transport { kind, .. } => !matches!(
                kind,
                // Refused redirect, oversized body: properties of what server
                // sent, not of the moment. Retry replays refusal.
                TransportKind::RedirectRefused
                    | TransportKind::BodyTooLarge
                    | TransportKind::Cancelled
            ),
            Error::Http { status, .. } => *status == 429 || (500..600).contains(status),
            // Locked database: common SQLite failure, clears.
            Error::Db(_) => true,
            _ => false,
        }
    }

    /// `true` when server rejected credentials.
    ///
    /// Surface distinctly: fix = "API key revoked"; retrying never helps.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(
            self,
            Error::Http {
                status: 401 | 403,
                ..
            }
        )
    }

    /// Build an [`Error::Item`] for a rejected entry.
    pub(crate) fn item(kind: &'static str, id: Option<i64>, reason: impl Into<String>) -> Self {
        Error::Item {
            kind,
            id,
            reason: reason.into(),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        // `rusqlite::Error` Display carries SQL text, some variants bound
        // values. No token ever bound (API key in separate 0600 file outside
        // mirror, §7), but article content and feed titles are foreign and
        // bound: not for renderer unexamined. Stringified, not chained: shape
        // of what escapes fixed here, not by rusqlite version.
        Error::Db(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn ep() -> SafeUrl {
        SafeUrl::from(
            &Url::parse("https://user:pw@miniflux.example/v1/entries?token=SHOULD-NOT-APPEAR")
                .unwrap(),
        )
    }

    #[test]
    fn http_errors_classify_retryability() {
        let permanent = Error::Http {
            status: 400,
            endpoint: ep(),
            message: None,
        };
        let auth = Error::Http {
            status: 401,
            endpoint: ep(),
            message: None,
        };
        let throttled = Error::Http {
            status: 429,
            endpoint: ep(),
            message: None,
        };
        let server = Error::Http {
            status: 503,
            endpoint: ep(),
            message: None,
        };

        assert!(
            !permanent.is_transient(),
            "a 400 will fail identically on replay"
        );
        assert!(!auth.is_transient());
        assert!(auth.is_auth_failure());
        assert!(throttled.is_transient(), "429 is the one retryable 4xx");
        assert!(server.is_transient());
    }

    #[test]
    fn policy_refusals_are_not_transient() {
        for kind in [TransportKind::RedirectRefused, TransportKind::BodyTooLarge] {
            let e = Error::Transport {
                endpoint: ep(),
                kind,
                detail: String::new(),
            };
            assert!(
                !e.is_transient(),
                "{kind} is a property of the response, not the moment"
            );
        }
        let e = Error::Transport {
            endpoint: ep(),
            kind: TransportKind::Timeout,
            detail: String::new(),
        };
        assert!(e.is_transient());
    }

    #[test]
    fn item_errors_are_isolated() {
        let e = Error::item("entry", Some(7), "published_at is not a timestamp");
        assert!(e.is_item_local(), "one bad entry must not stall the sync");
        assert!(!Error::Protocol("bad envelope".into()).is_item_local());
    }

    #[test]
    fn an_errors_endpoint_can_only_be_a_safe_url() {
        // TYPE-level guarantee only: every endpoint-naming variant holds `SafeUrl`, so full
        // URL can't be stored even by mistake. `ep()` redacted before `Error` built:
        // assertions hold whatever rendering code does.
        //
        // Behavioural half (`api::transport::classify` doesn't interpolate `reqwest::Error`,
        // whose Display carries URL and query) needs real failed request:
        // tests/transport_hardening.rs::a_failed_request_renders_without_the_url_or_its_query.
        let cases = [
            Error::Transport {
                endpoint: ep(),
                kind: TransportKind::Tls,
                detail: "handshake".into(),
            },
            Error::Http {
                status: 500,
                endpoint: ep(),
                message: Some("boom".into()),
            },
        ];
        for e in cases {
            let shown = e.to_string();
            let debugged = format!("{e:?}");
            for rendering in [shown, debugged] {
                assert!(!rendering.contains("pw"), "credential leaked: {rendering}");
                assert!(!rendering.contains("user:"), "userinfo leaked: {rendering}");
                assert!(
                    !rendering.contains("SHOULD-NOT-APPEAR"),
                    "query leaked: {rendering}"
                );
            }
        }
    }
}
