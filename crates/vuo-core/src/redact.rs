//! Secrets out of error paths.
//!
//! Scope §9.1: *never log the token or a URL containing it, and redact in error
//! paths too, which is where secrets usually escape.*
//!
//! API key goes in `X-Auth-Token` header, not URL: obvious leak closed by
//! construction. Two less obvious:
//!
//! 1. Pasted base URL with userinfo (`https://u:pw@host/`);
//!    [`Url::to_string`](url::Url) round-trips password verbatim.
//! 2. [`reqwest::Error`] `Display` embeds failing URL; `#[from]` variant would
//!    leak through chain without us formatting URL.
//!
//! So everything user-facing goes through [`SafeUrl`]: scheme, host, port, path
//! only. Query dropped wholesale: Miniflux doesn't put token there, future
//! endpoint might; deny-by-default only rule that stays true.

use std::fmt;

use url::Url;

/// URL stripped of anything that could carry credential.
///
/// Build with [`SafeUrl::from`]. Inner string safe to log, put in error, show
/// user.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SafeUrl(String);

impl SafeUrl {
    /// The redacted representation, e.g. `https://miniflux.example/v1/entries`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&Url> for SafeUrl {
    fn from(url: &Url) -> Self {
        let mut out = String::with_capacity(url.as_str().len());
        out.push_str(url.scheme());
        out.push_str("://");

        // Userinfo dropped entirely, username and password. Username not
        // secret, but no reason to log identifier.
        if let Some(host) = url.host_str() {
            out.push_str(host);
        } else {
            out.push_str("<no-host>");
        }
        if let Some(port) = url.port() {
            out.push(':');
            // `port` = u16 from parser; can't fail.
            out.push_str(&port.to_string());
        }
        out.push_str(url.path());

        // Query and fragment dropped uninspected. Deny-by-default (module
        // docs).
        SafeUrl(out)
    }
}

impl From<Url> for SafeUrl {
    fn from(url: Url) -> Self {
        SafeUrl::from(&url)
    }
}

impl fmt::Display for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `Debug` identical to `Display` on purpose.
///
/// Holders get `#[derive(Debug)]`-printed eventually; derived
/// `SafeUrl("https://...")` = noise. Keeps redaction true under both
/// formatters.
impl fmt::Debug for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// API token that can't be printed.
///
/// Only way out: [`ApiToken::expose`], named so review notices. `Debug` and
/// `Display` render fixed placeholder: no escape via `{:?}` on enclosing struct
/// (usual bug shape).
#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken(String);

impl ApiToken {
    /// Wrap a token. The value is kept as given, without trimming.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        ApiToken(token.into())
    }

    /// Yield secret. Call sites countable on one hand.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `true` when the token is empty or only whitespace.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiToken(<redacted>)")
    }
}

impl fmt::Display for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_url_drops_userinfo() {
        let url = Url::parse("https://alice:hunter2@miniflux.example/v1/entries").unwrap();
        let safe = SafeUrl::from(&url);
        assert_eq!(safe.as_str(), "https://miniflux.example/v1/entries");
        assert!(!safe.to_string().contains("hunter2"));
        assert!(!safe.to_string().contains("alice"));
        assert!(!format!("{safe:?}").contains("hunter2"));
    }

    #[test]
    fn safe_url_drops_query_and_fragment() {
        let url = Url::parse("https://host.example/v1/entries?token=sekrit&limit=5#frag").unwrap();
        let safe = SafeUrl::from(&url);
        assert_eq!(safe.as_str(), "https://host.example/v1/entries");
        assert!(!safe.to_string().contains("sekrit"));
    }

    #[test]
    fn safe_url_keeps_port() {
        let url = Url::parse("http://192.0.2.10:8080/v1/me").unwrap();
        assert_eq!(SafeUrl::from(&url).as_str(), "http://192.0.2.10:8080/v1/me");
    }

    #[test]
    fn token_never_renders_itself() {
        // Holding struct must not leak via derived Debug either.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            token: ApiToken,
        }

        let t = ApiToken::new("super-secret-token");
        assert_eq!(t.to_string(), "<redacted>");
        assert_eq!(format!("{t:?}"), "ApiToken(<redacted>)");
        let h = Holder { token: t.clone() };
        assert!(!format!("{h:?}").contains("super-secret-token"));
        assert_eq!(t.expose(), "super-secret-token");
    }
}
