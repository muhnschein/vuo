//! URL validation and media-fetch policy.
//!
//! Two scope rules meet here.
//!
//! **§9.2: validate URL schemes after parsing.** Only `http`/`https` survive
//! into rendered link or image; `javascript:`, `data:`, `file:` etc. dropped.
//! [`MediaUrl`] carries that guarantee: no other scheme constructible, `serde`
//! included; holder needn't re-check.
//!
//! **§9.3: route remote images through server's media proxy.** Direct
//! third-party fetches leak IP and reading times to every host in a feed:
//! tracking Miniflux strips server-side. Proxy is default; direct is explicit
//! opt-out.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

/// URL guaranteed `http` or `https`.
///
/// Enforced in every constructor incl. `Deserialize`: holds for database values
/// and fixtures, not just parsed markup.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MediaUrl(Url);

impl MediaUrl {
    /// Parse and validate. `None` for anything not `http(s)`.
    ///
    /// Rejects, doesn't sanitise: no "safe" `javascript:` URL exists.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let url = Url::parse(raw.trim()).ok()?;
        Self::from_url(url)
    }

    /// Resolve possibly-relative reference against base document URL.
    ///
    /// Feed content often has relative `src`/`href` meaningful only against
    /// article URL.
    #[must_use]
    pub fn parse_relative(raw: &str, base: &Url) -> Option<Self> {
        let url = base.join(raw.trim()).ok()?;
        Self::from_url(url)
    }

    /// Wrap parsed URL; `None` unless `http(s)`.
    #[must_use]
    pub fn from_url(url: Url) -> Option<Self> {
        match url.scheme() {
            "http" | "https" => Some(MediaUrl(url)),
            _ => None,
        }
    }

    /// The URL as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// The underlying parsed URL.
    #[must_use]
    pub fn as_url(&self) -> &Url {
        &self.0
    }

    /// `true` when URL points at given origin (scheme, host, port).
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
    /// Deserialisation is a construction path; skipping scheme check would make
    /// guarantee partial.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        MediaUrl::parse(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "expected an http(s) URL, got a {} one",
                Url::parse(&raw).map_or_else(|_| "malformed".to_owned(), |u| u.scheme().to_owned())
            ))
        })
    }
}

/// What to do about remote media in article content.
///
/// # Why not "proxy everything"
///
/// Vuo *can't* build proxy URLs: Miniflux signs them `HMAC-SHA256(media_url)`
/// with `MEDIA_PROXY_PRIVATE_KEY` (server-only, random per startup when unset),
/// no API signs on request. Client consumes URLs server already rewrote.
///
/// Server rewrites on every read path Vuo uses, but `MEDIA_PROXY_MODE` defaults
/// to `http-only` (plain-`http` images only), and nearly every feed image is
/// `https`. **Stock Miniflux: most images arrive as raw third-party URLs.**
/// Dropping them would blank most articles.
///
/// So three states, default [`UnproxiedMedia::Ask`]: never leak silently, never
/// pretend images don't exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPolicy {
    /// Rewrite third-party media through Miniflux instance (default).
    ProxyThroughInstance {
        /// Configured Miniflux origin. Media here is server-proxied or
        /// server-hosted: no third party learns anything.
        instance: Url,
        /// Extra origins trusted as media proxies.
        ///
        /// `MEDIA_PROXY_CUSTOM_URL` lets admin host proxy on another origin,
        /// else classified third-party. User setting; Vuo doesn't detect it.
        extra_trusted: Vec<Url>,
        /// What to do with media the server did not rewrite.
        fallback: UnproxiedMedia,
    },
    /// Fetch everything directly. Explicit, informed opt-out (§9.3).
    Direct,
}

/// What happens to media the server has not proxied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnproxiedMedia {
    /// Never fetch. UI shows placeholder naming host.
    Strict,
    /// Don't fetch now; user decides per origin (default).
    ///
    /// UI shows placeholder and load affordance. Consent remembered per origin:
    /// agreeing to `images.example.com` isn't agreeing to every host.
    #[default]
    Ask,
    /// Fetch directly, accepting IP leak. Explicit opt-in only.
    Allow,
}

/// The decision for one media URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaDecision {
    /// Safe to fetch now.
    ///
    /// Media always fetched with separate cookie-less client, never API token:
    /// `/proxy/` route is public, credentials needless exposure.
    Fetch(MediaUrl),
    /// Don't fetch, don't offer.
    Drop,
    /// Don't fetch yet; ask user about origin first.
    NeedsConsent(MediaUrl),
}

impl MediaPolicy {
    /// Default policy for instance: proxy, ask about rest.
    #[must_use]
    pub fn default_for(instance: Url) -> Self {
        MediaPolicy::ProxyThroughInstance {
            instance,
            extra_trusted: Vec::new(),
            fallback: UnproxiedMedia::Ask,
        }
    }

    /// Decide what to do with one media URL.
    ///
    /// `consented`: origins user already agreed to load from.
    #[must_use]
    pub fn decide_with_consent(&self, url: &MediaUrl, consented: &[Url]) -> MediaDecision {
        match self {
            MediaPolicy::Direct => MediaDecision::Fetch(url.clone()),
            MediaPolicy::ProxyThroughInstance {
                instance,
                extra_trusted,
                fallback,
            } => {
                // Classify by parsed ORIGIN, never "/proxy/" in path: any host
                // can serve that path.
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
        // Leading/trailing whitespace common in feed markup.
        assert!(MediaUrl::parse("  https://example.com/a.png\n").is_some());
    }

    #[test]
    fn deserialize_revalidates_the_scheme() {
        // Guarantee survives storage round trip, not just `parse`.
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
        // Absolute `javascript:` ref rejected even against https base.
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
            MediaPolicy::ProxyThroughInstance {
                instance,
                extra_trusted: Vec::new(),
                fallback: UnproxiedMedia::Strict,
            }
            .decide(&third_party),
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
        // `MEDIA_PROXY_CUSTOM_URL` tempts path detection; that would trust any
        // host serving /proxy/.
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
        // Same host, other scheme: not same origin.
        let plaintext = MediaUrl::parse("http://host.example/x.png").unwrap();
        assert!(!plaintext.is_same_origin_as(&instance));
        // Explicit default port: same origin.
        let explicit = MediaUrl::parse("https://host.example:443/x.png").unwrap();
        assert!(explicit.is_same_origin_as(&instance));
    }
}
