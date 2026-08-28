//! URL validation and the media-fetch policy.
//!
//! Two separate rules from the scope meet in this module.
//!
//! **§9.2 — validate URL schemes after parsing.** Only `http` and `https`
//! survive into a rendered link or image. `javascript:`, `data:`, `file:` and
//! everything else are dropped. [`MediaUrl`] is the type that carries that
//! guarantee: it cannot be constructed with any other scheme, including via
//! `serde`, so a call site holding one does not need to re-check.
//!
//! **§9.3 — route remote images through the server's media proxy.** Fetching
//! third-party image URLs directly from the phone leaks the user's IP address
//! and reading times to every host that appears in a feed. That is exactly the
//! tracking Miniflux strips server-side, reintroduced by the client. Proxying
//! is therefore the default and the direct path is an explicit opt-out, not the
//! other way around.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

/// A URL that is guaranteed to be `http` or `https`.
///
/// The invariant is enforced in every constructor including the `Deserialize`
/// impl, so it holds for values loaded from the local database and from test
/// fixtures as well as for freshly parsed markup.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MediaUrl(Url);

impl MediaUrl {
    /// Parse and validate. Returns `None` for anything that is not `http(s)`.
    ///
    /// Note this rejects rather than sanitising: there is no attempt to
    /// "fix up" a `javascript:` URL into something safe, because there is no
    /// such thing.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let url = Url::parse(raw.trim()).ok()?;
        Self::from_url(url)
    }

    /// Resolve a possibly-relative reference against a base document URL.
    ///
    /// Feed content routinely contains relative `src`/`href` values that only
    /// make sense against the article's own URL.
    #[must_use]
    pub fn parse_relative(raw: &str, base: &Url) -> Option<Self> {
        let url = base.join(raw.trim()).ok()?;
        Self::from_url(url)
    }

    #[must_use]
    pub fn from_url(url: Url) -> Option<Self> {
        match url.scheme() {
            "http" | "https" => Some(MediaUrl(url)),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    #[must_use]
    pub fn into_url(self) -> Url {
        self.0
    }

    /// `true` when this URL points at the given origin (scheme, host, port).
    #[must_use]
    pub fn is_same_origin_as(&self, other: &Url) -> bool {
        self.0.scheme() == other.scheme()
            && self.0.host_str() == other.host_str()
            && self.0.port_or_known_default() == other.port_or_known_default()
    }
}

impl fmt::Display for MediaUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl fmt::Debug for MediaUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MediaUrl({})", self.0.as_str())
    }
}

impl Serialize for MediaUrl {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for MediaUrl {
    /// Re-validates on the way in.
    ///
    /// Deserialisation is a construction path like any other. If it skipped the
    /// scheme check, the type's guarantee would hold only for URLs that
    /// happened to arrive by `parse()`, which is the kind of partial invariant
    /// that eventually fails in the one place nobody looked.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        MediaUrl::parse(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "expected an http(s) URL, got a {} one",
                Url::parse(&raw)
                    .map(|u| u.scheme().to_owned())
                    .unwrap_or_else(|_| "malformed".to_owned())
            ))
        })
    }
}

/// What to do about remote media referenced by article content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPolicy {
    /// Rewrite third-party media through the Miniflux instance (the default).
    ///
    /// Vuo does not attempt to *construct* proxy URLs: Miniflux signs them with
    /// a server-side key, so a client cannot mint one. What this policy does is
    /// accept URLs the server already rewrote (they are same-origin with the
    /// instance) and apply `fallback` to everything else.
    ProxyThroughInstance {
        /// The configured Miniflux origin. Media already pointing here is
        /// assumed to be server-proxied and is passed through.
        instance: Url,
        /// What to do with media the server did *not* rewrite.
        fallback: UnproxiedMedia,
    },
    /// Fetch everything directly. An explicit, informed opt-out (§9.3).
    Direct,
}

/// What happens to media the server has not proxied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnproxiedMedia {
    /// Do not render it. The privacy-preserving default: §11 explicitly
    /// contemplates deciding that such media "simply do not render rather than
    /// fetching them from the phone".
    Drop,
    /// Render it, accepting the IP leak. Only reachable by explicit opt-in.
    FetchDirectly,
}

/// The decision for one media URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaDecision {
    /// Safe to fetch at this URL.
    Fetch(MediaUrl),
    /// Do not fetch. The UI shows a placeholder.
    Drop,
}

impl MediaPolicy {
    /// The default policy for a given instance: proxy, and drop what is not
    /// proxied.
    #[must_use]
    pub fn default_for(instance: Url) -> Self {
        MediaPolicy::ProxyThroughInstance { instance, fallback: UnproxiedMedia::Drop }
    }

    /// Decide whether a media URL may be fetched, and from where.
    #[must_use]
    pub fn decide(&self, url: &MediaUrl) -> MediaDecision {
        match self {
            MediaPolicy::Direct => MediaDecision::Fetch(url.clone()),
            MediaPolicy::ProxyThroughInstance { instance, fallback } => {
                if url.is_same_origin_as(instance) {
                    // Already pointing at the user's own instance: either the
                    // server rewrote it into a proxy URL, or it is genuinely
                    // hosted there. Either way no third party learns anything.
                    MediaDecision::Fetch(url.clone())
                } else {
                    match fallback {
                        UnproxiedMedia::Drop => MediaDecision::Drop,
                        UnproxiedMedia::FetchDirectly => MediaDecision::Fetch(url.clone()),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dangerous_schemes() {
        for raw in [
            "javascript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "file:///etc/passwd",
            "ftp://example.com/x",
            "vbscript:msgbox(1)",
            "not a url at all",
            "",
        ] {
            assert!(MediaUrl::parse(raw).is_none(), "should have rejected {raw:?}");
        }
    }

    #[test]
    fn accepts_http_and_https() {
        assert!(MediaUrl::parse("http://example.com/a.png").is_some());
        assert!(MediaUrl::parse("https://example.com/a.png").is_some());
        // Leading/trailing whitespace is common in real feed markup.
        assert!(MediaUrl::parse("  https://example.com/a.png\n").is_some());
    }

    #[test]
    fn deserialize_revalidates_the_scheme() {
        // The guarantee must survive a round trip through storage, not just
        // through `parse`.
        let err = serde_json::from_str::<MediaUrl>("\"javascript:alert(1)\"");
        assert!(err.is_err(), "deserialize must not bypass scheme validation");

        let ok: MediaUrl = serde_json::from_str("\"https://example.com/a\"").unwrap();
        assert_eq!(ok.as_str(), "https://example.com/a");
    }

    #[test]
    fn relative_urls_resolve_against_the_article() {
        let base = Url::parse("https://blog.example/posts/1/").unwrap();
        let resolved = MediaUrl::parse_relative("../img/x.png", &base).unwrap();
        assert_eq!(resolved.as_str(), "https://blog.example/posts/img/x.png");
    }

    #[test]
    fn relative_resolution_cannot_escape_into_another_scheme() {
        let base = Url::parse("https://blog.example/p/").unwrap();
        // An absolute javascript: reference must still be rejected even when
        // resolved against an https base.
        assert!(MediaUrl::parse_relative("javascript:alert(1)", &base).is_none());
    }

    #[test]
    fn default_policy_drops_third_party_media() {
        let instance = Url::parse("https://miniflux.example/").unwrap();
        let policy = MediaPolicy::default_for(instance);

        let third_party = MediaUrl::parse("https://tracker.example/pixel.gif").unwrap();
        assert_eq!(
            policy.decide(&third_party),
            MediaDecision::Drop,
            "third-party media must not be fetched from the phone by default"
        );

        let proxied = MediaUrl::parse("https://miniflux.example/proxy/abc/def").unwrap();
        assert_eq!(policy.decide(&proxied), MediaDecision::Fetch(proxied.clone()));
    }

    #[test]
    fn opt_out_policies_are_explicit() {
        let instance = Url::parse("https://miniflux.example/").unwrap();
        let third_party = MediaUrl::parse("https://tracker.example/pixel.gif").unwrap();

        let lenient = MediaPolicy::ProxyThroughInstance {
            instance,
            fallback: UnproxiedMedia::FetchDirectly,
        };
        assert_eq!(lenient.decide(&third_party), MediaDecision::Fetch(third_party.clone()));
        assert_eq!(MediaPolicy::Direct.decide(&third_party), MediaDecision::Fetch(third_party));
    }

    #[test]
    fn origin_comparison_respects_port_and_scheme() {
        let instance = Url::parse("https://host.example/").unwrap();
        // Same host, different scheme: not the same origin.
        let plaintext = MediaUrl::parse("http://host.example/x.png").unwrap();
        assert!(!plaintext.is_same_origin_as(&instance));
        // Explicit default port is still the same origin.
        let explicit = MediaUrl::parse("https://host.example:443/x.png").unwrap();
        assert!(explicit.is_same_origin_as(&instance));
    }
}
