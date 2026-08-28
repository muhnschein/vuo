//! Keeping secrets out of error paths.
//!
//! §9.1 of the scope: *never log the token or a URL containing it, and redact
//! in error paths too, which is where secrets usually escape.*
//!
//! Vuo sends its API key in the `X-Auth-Token` header rather than in a URL, so
//! the obvious leak is closed by construction. Two less obvious ones are not:
//!
//! 1. A user may paste a base URL carrying userinfo (`https://u:pw@host/`).
//!    [`Url::to_string`] round-trips the password verbatim.
//! 2. [`reqwest::Error`]'s own `Display` embeds the URL it failed on. Wrapping
//!    one in a `#[from]` variant would reintroduce the leak through the error
//!    chain even though we never format the URL ourselves.
//!
//! Everything user-facing therefore goes through [`SafeUrl`], which keeps only
//! scheme, host, port and path. Query strings are dropped wholesale: Miniflux
//! does not put the token in one, but a future endpoint might, and a
//! deny-by-default rule is the only kind that stays true.

use std::fmt;

use url::Url;

/// A URL stripped of everything that could carry a credential.
///
/// Construct with [`SafeUrl::from`]. The inner string is safe to log, to put
/// in an error message, and to show a user.
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

        // Userinfo is dropped entirely -- both the username and the password.
        // A username is not a secret, but it is an identifier we have no
        // reason to write to a log file.
        if let Some(host) = url.host_str() {
            out.push_str(host);
        } else {
            out.push_str("<no-host>");
        }
        if let Some(port) = url.port() {
            out.push(':');
            // `port` is a u16 from the parser, so this cannot fail.
            out.push_str(&port.to_string());
        }
        out.push_str(url.path());

        // The query and fragment are dropped without inspection. See the
        // module docs: deny-by-default is the only rule that stays true.
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

/// `Debug` is deliberately identical to `Display`.
///
/// Anything holding a `SafeUrl` will be `#[derive(Debug)]`-printed sooner or
/// later, and a derived tuple-struct debug (`SafeUrl("https://...")`) is just
/// noise. More importantly it keeps the redaction guarantee true under both
/// formatters rather than only the one we remembered.
impl fmt::Debug for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An API token that cannot be printed.
///
/// The only way to read the secret back out is [`ApiToken::expose`], which is
/// named to make a review notice it. `Debug` and `Display` both render a fixed
/// placeholder, so a token cannot escape through a `{:?}` on some enclosing
/// struct -- which is exactly how this class of bug usually happens.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken(String);

impl ApiToken {
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        ApiToken(token.into())
    }

    /// Yield the secret. Call sites should be countable on one hand.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

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
        let t = ApiToken::new("super-secret-token");
        assert_eq!(t.to_string(), "<redacted>");
        assert_eq!(format!("{t:?}"), "ApiToken(<redacted>)");
        // The struct that holds it must not leak it via a derived Debug either.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            token: ApiToken,
        }
        let h = Holder { token: t.clone() };
        assert!(!format!("{h:?}").contains("super-secret-token"));
        assert_eq!(t.expose(), "super-secret-token");
    }
}
