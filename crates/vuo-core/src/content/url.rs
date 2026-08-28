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
///
/// # Why this is not simply "proxy everything"
///
/// Vuo *cannot* construct a proxy URL. Miniflux signs them
/// `HMAC-SHA256(media_url)` with `MEDIA_PROXY_PRIVATE_KEY`, a server-only
/// secret that is randomly regenerated at every startup when unset, and no API
/// endpoint will sign one on request. The client can only consume URLs the
/// server already rewrote.
///
/// The server does rewrite: every read path Vuo uses runs entry content
/// through the proxy rewriter. But `MEDIA_PROXY_MODE` defaults to `http-only`,
/// which proxies plain-`http` images only — and essentially every feed image
/// is `https` now. **On a stock Miniflux, most images arrive as raw
/// third-party URLs.** Un-proxied media is the common case, not the edge case,
/// and a policy that silently drops it would blank out most articles.
///
/// Hence three states rather than two, defaulting to [`UnproxiedMedia::Ask`]:
/// never leak silently, but do not pretend the images do not exist either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPolicy {
    /// Rewrite third-party media through the Miniflux instance (the default).
    ProxyThroughInstance {
        /// The configured Miniflux origin. Media here is server-proxied or
        /// server-hosted; either way no third party learns anything.
        instance: Url,
        /// Additional origins to treat as trusted media proxies.
        ///
        /// Exists because `MEDIA_PROXY_CUSTOM_URL` lets an admin host the
        /// proxy on a different origin entirely, where it would otherwise
        /// classify as third-party. This is a user-supplied setting rather
        /// than something Vuo tries to detect.
        extra_trusted: Vec<Url>,
        /// What to do with media the server did not rewrite.
        fallback: UnproxiedMedia,
    },
    /// Fetch everything directly. An explicit, informed opt-out (§9.3).
    Direct,
}

/// What happens to media the server has not proxied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnproxiedMedia {
    /// Never fetch it. The UI renders a placeholder naming the host.
    Strict,
    /// Do not fetch it now; let the user decide per origin (the default).
    ///
    /// The UI shows a placeholder and an affordance to load. Consent is
    /// remembered per origin, so agreeing once to `images.example.com` does
    /// not agree to every host in every feed.
    #[default]
    Ask,
    /// Fetch it directly, accepting the IP leak. Explicit opt-in only.
    Allow,
}

/// The decision for one media URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaDecision {
    /// Safe to fetch now.
    ///
    /// Media is always fetched with a separate, cookie-less client that never
    /// attaches the API token: the `/proxy/` route is public and unauthenticated,
    /// so sending credentials to it would be a needless exposure.
    Fetch(MediaUrl),
    /// Do not fetch, and do not offer to.
    Drop,
    /// Do not fetch yet; ask the user about this origin first.
    NeedsConsent(MediaUrl),
}

impl MediaPolicy {
    /// The default policy for a given instance: proxy, and ask about the rest.
    #[must_use]
    pub fn default_for(instance: Url) -> Self {
        MediaPolicy::ProxyThroughInstance {
            instance,
            extra_trusted: Vec::new(),
            fallback: UnproxiedMedia::Ask,
        }
    }

    /// A policy that never fetches un-proxied media.
    #[must_use]
    pub fn strict_for(instance: Url) -> Self {
        MediaPolicy::ProxyThroughInstance {
            instance,
            extra_trusted: Vec::new(),
            fallback: UnproxiedMedia::Strict,
        }
    }

    /// Decide what to do with one media URL.
    ///
    /// `consented` are origins the user has already agreed to load from.
    #[must_use]
    pub fn decide_with_consent(&self, url: &MediaUrl, consented: &[Url]) -> MediaDecision {
        match self {
            MediaPolicy::Direct => MediaDecision::Fetch(url.clone()),
            MediaPolicy::ProxyThroughInstance {
                instance,
                extra_trusted,
                fallback,
            } => {
                // Classify by parsed ORIGIN, never by looking for "/proxy/" in
                // the path: any third-party host can serve a /proxy/ path, and
                // matching on the string would trust it.
                let trusted = url.is_same_origin_as(instance)
                    || extra_trusted.iter().any(|o| url.is_same_origin_as(o));
                if trusted {
                    return MediaDecision::Fetch(url.clone());
                }
                if consented.iter().any(|o| url.is_same_origin_as(o)) {
                    return MediaDecision::Fetch(url.clone());
                }
                match fallback {
                    UnproxiedMedia::Strict => MediaDecision::Drop,
                    UnproxiedMedia::Ask => MediaDecision::NeedsConsent(url.clone()),
                    UnproxiedMedia::Allow => MediaDecision::Fetch(url.clone()),
                }
            }
        }
    }

    /// Decide with no prior consent recorded.
    #[must_use]
    pub fn decide(&self, url: &MediaUrl) -> MediaDecision {
        self.decide_with_consent(url, &[])
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
            assert!(
                MediaUrl::parse(raw).is_none(),
                "should have rejected {raw:?}"
            );
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
        assert!(
            err.is_err(),
            "deserialize must not bypass scheme validation"
        );

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
    fn default_policy_never_silently_fetches_third_party_media() {
        let instance = Url::parse("https://miniflux.example/").unwrap();
        let policy = MediaPolicy::default_for(instance);

        let third_party = MediaUrl::parse("https://tracker.example/pixel.gif").unwrap();
        assert_eq!(
            policy.decide(&third_party),
            MediaDecision::NeedsConsent(third_party.clone()),
            "third-party media must never be fetched from the phone unasked"
        );

        let proxied = MediaUrl::parse("https://miniflux.example/proxy/abc/def").unwrap();
        assert_eq!(policy.decide(&proxied), MediaDecision::Fetch(proxied));
    }

    #[test]
    fn strict_policy_drops_rather_than_asking() {
        let instance = Url::parse("https://miniflux.example/").unwrap();
        let third_party = MediaUrl::parse("https://tracker.example/pixel.gif").unwrap();
        assert_eq!(
            MediaPolicy::strict_for(instance).decide(&third_party),
            MediaDecision::Drop
        );
    }

    #[test]
    fn consent_is_remembered_per_origin_not_globally() {
        let instance = Url::parse("https://miniflux.example/").unwrap();
        let policy = MediaPolicy::default_for(instance);

        let agreed = MediaUrl::parse("https://images.example/a.png").unwrap();
        let other = MediaUrl::parse("https://tracker.example/b.png").unwrap();
        let consented = vec![Url::parse("https://images.example/").unwrap()];

        assert_eq!(
            policy.decide_with_consent(&agreed, &consented),
            MediaDecision::Fetch(agreed)
        );
        assert_eq!(
            policy.decide_with_consent(&other, &consented),
            MediaDecision::NeedsConsent(other),
            "agreeing to one host must not agree to every host"
        );
    }

    #[test]
    fn a_third_party_proxy_path_is_not_trusted() {
        // MEDIA_PROXY_CUSTOM_URL means proxy URLs can live on another origin,
        // so it is tempting to detect them by path. That would trust any host
        // willing to serve a /proxy/ path.
        let instance = Url::parse("https://miniflux.example/").unwrap();
        let policy = MediaPolicy::default_for(instance);
        let impostor = MediaUrl::parse("https://evil.example/proxy/sig/aHR0cA==").unwrap();
        assert_eq!(
            policy.decide(&impostor),
            MediaDecision::NeedsConsent(impostor)
        );
    }

    #[test]
    fn an_admin_configured_proxy_origin_is_trusted() {
        let policy = MediaPolicy::ProxyThroughInstance {
            instance: Url::parse("https://miniflux.example/").unwrap(),
            extra_trusted: vec![Url::parse("https://cdn.example/").unwrap()],
            fallback: UnproxiedMedia::Strict,
        };
        let custom = MediaUrl::parse("https://cdn.example/aHR0cHM6Ly94").unwrap();
        assert_eq!(policy.decide(&custom), MediaDecision::Fetch(custom));
    }

    #[test]
    fn opt_out_policies_are_explicit() {
        let instance = Url::parse("https://miniflux.example/").unwrap();
        let third_party = MediaUrl::parse("https://tracker.example/pixel.gif").unwrap();

        let lenient = MediaPolicy::ProxyThroughInstance {
            instance,
            extra_trusted: Vec::new(),
            fallback: UnproxiedMedia::Allow,
        };
        assert_eq!(
            lenient.decide(&third_party),
            MediaDecision::Fetch(third_party.clone())
        );
        assert_eq!(
            MediaPolicy::Direct.decide(&third_party),
            MediaDecision::Fetch(third_party)
        );
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
