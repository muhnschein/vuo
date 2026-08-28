//! The error taxonomy for `vuo-core`.
//!
//! Two constraints from the scope shape this module more than ergonomics does.
//!
//! **§9.5 — panics are not a safe failure mode.** Unwinding out of Rust into
//! Qt's C++ frames is undefined behaviour, so every foreign-input path returns
//! a `Result` rather than unwrapping. `clippy::unwrap_used`, `expect_used`,
//! `panic` and `indexing_slicing` are denied workspace-wide to keep it that
//! way.
//!
//! **§9.1 — redact in error paths.** There is deliberately no
//! `#[from] reqwest::Error` variant. `reqwest::Error`'s own `Display` embeds
//! the URL it failed on, so storing one as an error source would leak any
//! userinfo in the configured base URL through the error chain even though we
//! never format a `Url` ourselves. Transport failures are classified into
//! [`TransportKind`] at the boundary and the URL is reduced to a
//! [`SafeUrl`](crate::redact::SafeUrl) first.
//!
//! **§9.2 — one bad entry must not stall the sync.** [`Error::is_item_local`]
//! distinguishes "this one item was malformed" from "the whole operation
//! failed", so a pull can drop a single unparseable entry and carry on.

use std::fmt;

use crate::redact::SafeUrl;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Why a request never produced an HTTP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// DNS failure, connection refused, unreachable network.
    Connect,
    /// The TLS handshake failed, including certificate verification.
    ///
    /// §9.1: verification is not optional and gets no toggle, so this is
    /// always a hard error. A user with a private CA supplies that CA
    /// explicitly rather than being offered a "trust anything" switch.
    Tls,
    /// A connect, read or overall-request deadline elapsed.
    Timeout,
    /// The body exceeded the configured cap and the read was abandoned.
    ///
    /// This is a phone: an unbounded `Vec<u8>` sized by an untrusted
    /// `Content-Length` is an out-of-memory waiting to happen (§9.1).
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
            TransportKind::Connect => "could not connect",
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

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The account or sync settings are unusable. Raised before any I/O.
    #[error("configuration error: {0}")]
    Config(String),

    /// No HTTP response was obtained.
    #[error("{kind} ({endpoint})")]
    Transport {
        endpoint: SafeUrl,
        kind: TransportKind,
        /// A short, already-redacted detail string. Never the raw
        /// `reqwest::Error` display, which would carry the URL.
        detail: String,
    },

    /// The server answered, but not with success.
    ///
    /// `message` is the server's own `error_message` field when it sent one.
    /// It is foreign text: render it as plain text, never as markup (§9.3).
    #[error("server returned HTTP {status} for {endpoint}{}", .message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Http {
        status: u16,
        endpoint: SafeUrl,
        message: Option<String>,
    },

    /// The response was structurally valid HTTP but not what the API promises:
    /// wrong JSON shape, an absurd field, a value that fails domain validation.
    #[error("malformed response from server: {0}")]
    Protocol(String),

    /// A single item inside an otherwise good response was unusable.
    ///
    /// Callers are expected to drop the item, count it, and continue. §9.2:
    /// *reject the entry rather than the sync when one item is malformed.*
    #[error("unusable {kind} (id {id:?}): {reason}")]
    Item {
        kind: &'static str,
        id: Option<i64>,
        reason: String,
    },

    /// The local SQLite mirror failed.
    #[error("database error: {0}")]
    Db(String),

    /// A schema migration failed. Distinguished from [`Error::Db`] because the
    /// recovery is different and because a failed migration must never be
    /// allowed to silently drop a pending outbox (§9.4).
    #[error("database migration {version} failed: {reason}")]
    Migration { version: i64, reason: String },

    /// The content transform refused its input: too deep, too large, or
    /// malformed past the point of usefulness (§9.2).
    #[error("could not transform article content: {0}")]
    Content(String),

    /// The operation was cancelled, e.g. the app is shutting down.
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    /// `true` when the failure concerns one item rather than the operation.
    ///
    /// A pull loop uses this to decide whether to drop a row and continue or
    /// to abort the sync.
    #[must_use]
    pub fn is_item_local(&self) -> bool {
        matches!(self, Error::Item { .. })
    }

    /// `true` when this failure means the server will reject this payload
    /// forever, so the queued intent should be dropped rather than retried.
    ///
    /// Deliberately narrow: **only** a non-429 client error from the server.
    ///
    /// This is not the inverse of [`Error::is_transient`], and conflating the
    /// two loses user data. A refused redirect or an oversized body is not
    /// "transient" — retrying replays the same refusal — but it is also not
    /// the *payload's* fault: it means the server is misconfigured or hostile.
    /// Treating those as permanent made the outbox discard marks and stars the
    /// user had made, because a misdirected server URL looked exactly like a
    /// malformed request. Anything that is neither transient nor a 4xx stays
    /// queued and waits for a human to fix the configuration.
    #[must_use]
    pub fn is_permanently_rejected(&self) -> bool {
        matches!(self, Error::Http { status, .. } if (400..500).contains(status) && *status != 429)
    }

    /// `true` when retrying the identical request could plausibly succeed.
    ///
    /// This is the outbox's retry classifier. Because every outbox write is an
    /// absolute set (see [`crate::outbox`]), retrying after an ambiguous
    /// timeout is safe -- there is no partial-application hazard to reason
    /// about, which is the whole payoff of the desired-state design.
    ///
    /// 4xx is permanent *except* 429: a 400 means the payload is malformed and
    /// resending it will fail identically until the code changes.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Transport { kind, .. } => !matches!(
                kind,
                // A refused redirect and an oversized body are properties of
                // what the server sent, not of the moment. Retrying replays
                // the same refusal.
                TransportKind::RedirectRefused
                    | TransportKind::BodyTooLarge
                    | TransportKind::Cancelled
            ),
            Error::Http { status, .. } => *status == 429 || (500..600).contains(status),
            // A locked database is the common SQLite failure and it clears.
            Error::Db(_) => true,
            _ => false,
        }
    }

    /// `true` when the server rejected our credentials.
    ///
    /// Worth surfacing distinctly: the fix is "your API key was revoked",
    /// which no amount of retrying addresses.
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
        // `rusqlite::Error`'s Display carries SQL text and, in some variants,
        // bound parameter values. No token is ever bound into SQL -- the API
        // key lives in a separate 0600 file, deliberately outside the mirror
        // (§7) -- but article content and feed titles are foreign text and do
        // get bound, so the message is still not something to hand to a
        // renderer unexamined. Stringified rather than chained so the shape of
        // what escapes is fixed here rather than by rusqlite's version.
        Error::Db(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn ep() -> SafeUrl {
        // Deliberately includes userinfo: the assertion below is that it never
        // reaches a rendered error.
        SafeUrl::from(&Url::parse("https://user:pw@miniflux.example/v1/entries").unwrap())
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
    fn rendered_errors_never_carry_credentials() {
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
            }
        }
    }
}
