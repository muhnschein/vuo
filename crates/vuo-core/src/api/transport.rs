//! The hardened HTTP layer.
//!
//! Every rule in §9.1 is implemented here, and each one has a comment saying
//! which attack it closes, because the reasoning is not recoverable from the
//! code alone.
//!
//! # The redirect problem, and why redirects are followed by hand
//!
//! §9.1: *do not follow redirects with the API token attached. The token
//! travels in a custom header, and HTTP clients that strip credentials on
//! cross-origin redirect typically only special-case `Authorization`.*
//!
//! That is exactly `reqwest`'s behaviour. Its built-in redirect policies strip
//! `Authorization`, `Cookie` and `Proxy-Authorization` when the origin changes
//! — but `X-Auth-Token` is not on that list, because it is not a standard
//! header. A hostile or merely compromised instance answering `302` with a
//! `Location` on an attacker's host would hand over the user's API key.
//!
//! So the client is built with [`redirect::Policy::none`] and redirects are
//! resolved in [`Transport::send`], where the token is attached per hop
//! against an explicit origin check.
//!
//! # How a per-host CA is achieved with a client that has no per-host CA API
//!
//! §9.1 asks for *an explicit, per-host, user-supplied CA certificate — narrow,
//! auditable, and it does not silently disable verification for every other
//! host.* `reqwest` has no way to scope a root certificate to one host.
//!
//! The scoping is achieved structurally instead: the extra CA is installed on
//! the **API client only**, that client is constructed per account, and the
//! redirect policy above guarantees it never contacts any origin but the
//! configured one. So a user's private CA is trusted for exactly the one host
//! it was supplied for, which is the property §9.1 actually wants.
//!
//! A note on media, because it is easy to assume otherwise:
//! [`Transport::media_client`] exists and carries neither the token nor the
//! extra CA, but article images are fetched by **Qt**, from the QML `Image`
//! element, not through this crate. What Rust decides is *which URLs the UI is
//! given at all* -- the media policy runs in the transform, before a URL ever
//! reaches QML -- and that is where the §9.3 guarantee actually lives. The
//! client here is for media Rust fetches itself (currently none) and for tests.
//!
//! There is deliberately no way to disable verification. Not a setting, not a
//! feature flag, not an environment variable.

use std::time::Duration;

use futures_util::StreamExt as _;
use url::Url;

use crate::error::{Error, Result, TransportKind};
use crate::redact::{ApiToken, SafeUrl};

/// Miniflux's API-key header. Not `Authorization`, which is why the redirect
/// handling above has to be manual.
const AUTH_HEADER: &str = "X-Auth-Token";

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub connect_timeout: Duration,
    /// Maximum time between bytes. A server that accepts a connection and then
    /// dribbles must not wedge a sync forever.
    pub read_timeout: Duration,
    /// Ceiling on a whole `send`, INCLUDING every redirect hop.
    ///
    /// Applied as a shrinking deadline rather than per-hop: a per-hop timeout
    /// multiplied by `max_redirects` would let a server that redirects slowly
    /// hold a sync open for four times this long.
    pub request_timeout: Duration,
    /// Hard cap on any response body (§9.1: this is a phone).
    pub max_response_bytes: usize,
    pub max_redirects: usize,
    /// PEM for a user-supplied CA, for self-hosted instances with a private
    /// certificate authority. Trusted only by the API client (see module docs).
    pub extra_ca_pem: Option<Vec<u8>>,
    pub user_agent: String,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(120),
            // Large enough for a page of 500 entries with full content,
            // small enough that a hostile length cannot exhaust the device.
            max_response_bytes: 32 * 1024 * 1024,
            max_redirects: 3,
            extra_ca_pem: None,
            user_agent: concat!("Vuo/", env!("CARGO_PKG_VERSION")).to_owned(),
        }
    }
}

/// A response body already read, bounded, into memory.
#[derive(Debug)]
pub struct BoundedResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// The `Date` header, parsed. The sync cursor is anchored to the server's
    /// clock rather than the phone's, because the phone's is not trustworthy
    /// and the comparison happens server-side.
    pub server_date: Option<chrono::DateTime<chrono::Utc>>,
}

/// An HTTP transport bound to one Miniflux origin.
#[derive(Debug, Clone)]
pub struct Transport {
    api: reqwest::Client,
    media: reqwest::Client,
    origin: Url,
    token: ApiToken,
    config_max_bytes: usize,
    max_redirects: usize,
    request_timeout: Duration,
}

impl Transport {
    /// Build a transport for one account.
    ///
    /// Note there is no cookie jar, and no call to disable one: `reqwest`'s
    /// cookie support lives behind the `cookies` feature, which this crate
    /// does not enable. Cookies are therefore absent by construction rather
    /// than by configuration -- a stronger guarantee, since it cannot be
    /// undone by a later edit to this function. Vuo authenticates with a
    /// header, so a cookie could only ever be something a server set that we
    /// would then replay somewhere we did not intend.
    pub fn new(origin: Url, token: ApiToken, config: &TransportConfig) -> Result<Self> {
        if !matches!(origin.scheme(), "http" | "https") {
            return Err(Error::Config(format!(
                "server URL must be http or https, got {:?}",
                origin.scheme()
            )));
        }
        if origin.cannot_be_a_base() || origin.host_str().is_none() {
            return Err(Error::Config("server URL has no host".to_owned()));
        }

        let base = || {
            reqwest::Client::builder()
                .user_agent(config.user_agent.clone())
                .connect_timeout(config.connect_timeout)
                .read_timeout(config.read_timeout)
                .timeout(config.request_timeout)
                // Redirects are resolved by hand; see the module docs.
                .redirect(reqwest::redirect::Policy::none())
        };

        let mut api_builder = base();
        if let Some(pem) = &config.extra_ca_pem {
            // Additive: the platform roots stay trusted, so supplying a
            // private CA does not turn off verification for anything.
            let certs = reqwest::Certificate::from_pem_bundle(pem).map_err(|_| {
                Error::Config("the supplied CA certificate is not valid PEM".to_owned())
            })?;
            // `from_pem_bundle` answers `Ok(vec![])` for input that contains no
            // PEM blocks at all rather than reporting an error. Without this
            // check a typo'd or truncated certificate file would silently
            // become "no extra CA", and the user would see an opaque TLS
            // failure at sync time instead of an actionable one at setup time.
            if certs.is_empty() {
                return Err(Error::Config(
                    "the supplied CA file contains no certificates".to_owned(),
                ));
            }
            for cert in certs {
                api_builder = api_builder.add_root_certificate(cert);
            }
        }

        let api = api_builder
            .build()
            .map_err(|_| Error::Config("could not build the HTTP client".to_owned()))?;

        // The media client gets neither the token nor the extra CA, and is
        // used for third-party hosts. Keeping it separate is what makes the
        // API client's private CA genuinely host-scoped.
        let media = base()
            .build()
            .map_err(|_| Error::Config("could not build the media HTTP client".to_owned()))?;

        Ok(Transport {
            api,
            media,
            origin,
            token,
            config_max_bytes: config.max_response_bytes,
            max_redirects: config.max_redirects,
            request_timeout: config.request_timeout,
        })
    }

    #[must_use]
    pub fn origin(&self) -> &Url {
        &self.origin
    }

    /// A client for fetching media: no token, no cookies, no private CA.
    ///
    /// Not used by the article view -- Qt fetches images itself. See the note
    /// in the module docs about where the §9.3 guarantee actually lives.
    #[must_use]
    pub fn media_client(&self) -> &reqwest::Client {
        &self.media
    }

    /// True when `url` is the configured instance's origin.
    fn is_configured_origin(&self, url: &Url) -> bool {
        url.scheme() == self.origin.scheme()
            && url.host_str() == self.origin.host_str()
            && url.port_or_known_default() == self.origin.port_or_known_default()
    }

    /// Perform a request, following redirects under an explicit policy.
    pub async fn send(
        &self,
        method: reqwest::Method,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<BoundedResponse> {
        let mut current = url;
        let mut hops = 0usize;
        // One deadline for the whole call, not one per hop.
        let started = std::time::Instant::now();

        loop {
            let safe = SafeUrl::from(&current);

            // The token is attached only for the configured origin.
            //
            // Belt-and-braces: the origin check below refuses an off-origin hop
            // outright, so `current` is always the configured origin by the
            // time it gets here and this condition is always true. It stays as
            // a second line of defence if that refusal is ever loosened -- but
            // being unreachable, it has no behaviour a test can observe, and
            // the comment in
            // `tests::the_token_is_withheld_from_any_origin_but_the_configured_one`
            // says so rather than implying coverage that does not exist.
            let remaining = self.request_timeout.checked_sub(started.elapsed());
            let Some(remaining) = remaining.filter(|r| !r.is_zero()) else {
                return Err(Error::Transport {
                    endpoint: safe,
                    kind: TransportKind::Timeout,
                    detail: "the request deadline elapsed while following redirects".to_owned(),
                });
            };

            let mut req = self
                .api
                .request(method.clone(), current.clone())
                .timeout(remaining);
            if self.is_configured_origin(&current) {
                req = req.header(AUTH_HEADER, self.token.expose());
            }
            if let Some(bytes) = &body {
                req = req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(bytes.clone());
            }

            let response = req.send().await.map_err(|e| classify(e, &safe))?;
            let status = response.status();

            // 304/305/306 are in the 3xx range but are not redirects to
            // follow: 304 carries no Location by design. Vuo never sends a
            // conditional request header, so a 304 here means the server (or
            // something in front of it) is misbehaving -- report that, rather
            // than the confusing "redirect without a Location header".
            if matches!(status.as_u16(), 304..=306) {
                return Err(Error::Http {
                    status: status.as_u16(),
                    endpoint: safe,
                    message: Some("unexpected conditional or proxy response".to_owned()),
                });
            }

            if status.is_redirection() {
                if hops >= self.max_redirects {
                    return Err(Error::Transport {
                        endpoint: safe,
                        kind: TransportKind::RedirectRefused,
                        detail: format!("more than {} redirects", self.max_redirects),
                    });
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| Error::Transport {
                        endpoint: safe.clone(),
                        kind: TransportKind::RedirectRefused,
                        detail: "redirect without a Location header".to_owned(),
                    })?;

                let next = current.join(location).map_err(|_| Error::Transport {
                    endpoint: safe.clone(),
                    kind: TransportKind::RedirectRefused,
                    detail: "redirect Location is not a valid URL".to_owned(),
                })?;

                // Refuse a downgrade to plaintext (§9.1).
                if current.scheme() == "https" && next.scheme() != "https" {
                    return Err(Error::Transport {
                        endpoint: safe,
                        kind: TransportKind::RedirectRefused,
                        detail: "refused a redirect from https to plaintext".to_owned(),
                    });
                }

                // Refuse to leave the configured origin at all. This is
                // stricter than merely dropping the token: an API that
                // redirects off-instance is either misconfigured or hostile,
                // and following it silently would be surprising either way.
                // The user sees an actionable error about their server URL.
                if !self.is_configured_origin(&next) {
                    return Err(Error::Transport {
                        endpoint: safe,
                        kind: TransportKind::RedirectRefused,
                        detail: "refused a redirect away from the configured server".to_owned(),
                    });
                }

                hops += 1;
                current = next;
                continue;
            }

            let server_date = response
                .headers()
                .get(reqwest::header::DATE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| chrono::DateTime::parse_from_rfc2822(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));

            // Reject on the declared length before reading a byte, then
            // enforce again while reading -- Content-Length is foreign input
            // and may be absent or a lie in either direction.
            if let Some(len) = response.content_length() {
                if len > self.config_max_bytes as u64 {
                    return Err(Error::Transport {
                        endpoint: safe,
                        kind: TransportKind::BodyTooLarge,
                        detail: format!("declared {len} bytes"),
                    });
                }
            }

            let body = self.read_bounded(response, &safe).await?;
            return Ok(BoundedResponse {
                status: status.as_u16(),
                body,
                server_date,
            });
        }
    }

    /// Read a body incrementally, abandoning it the moment it exceeds the cap.
    ///
    /// §9.1: *bound every response. Cap body size and read incrementally.* The
    /// incremental part is the point -- calling `bytes()` and checking the
    /// length afterwards would have already made the allocation this is
    /// supposed to prevent.
    async fn read_bounded(&self, response: reqwest::Response, safe: &SafeUrl) -> Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| classify(e, safe))?;
            if out.len().saturating_add(chunk.len()) > self.config_max_bytes {
                return Err(Error::Transport {
                    endpoint: safe.clone(),
                    kind: TransportKind::BodyTooLarge,
                    detail: format!("exceeded {} bytes", self.config_max_bytes),
                });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
}

/// Turn a `reqwest::Error` into ours **without** keeping it as a source.
///
/// `reqwest::Error`'s `Display` embeds the URL it failed on. Chaining one
/// would leak any userinfo in the configured base URL through the error chain,
/// defeating the redaction in [`crate::redact`]. Only the classification is
/// kept; the detail string is written here and contains nothing foreign.
fn classify(e: reqwest::Error, endpoint: &SafeUrl) -> Error {
    let kind = if e.is_timeout() {
        TransportKind::Timeout
    } else if e.is_connect() {
        // reqwest folds TLS failures into connect errors. Distinguishing them
        // matters for the message the user sees: "check your certificate" and
        // "check your network" are different actions.
        if format!("{e:?}").contains("certificate") || format!("{e:?}").contains("Tls") {
            TransportKind::Tls
        } else {
            TransportKind::Connect
        }
    } else {
        // Body and decode failures are deliberately not distinguished here:
        // callers act on the retry classification, and both are equally
        // "the response was unusable".
        TransportKind::Other
    };

    Error::Transport {
        endpoint: endpoint.clone(),
        kind,
        // Deliberately a fixed string per class, not `e.to_string()`.
        detail: match kind {
            TransportKind::Timeout => "the server did not respond in time",
            TransportKind::Tls => "the server's certificate could not be verified",
            TransportKind::Connect => "the server could not be reached",
            _ => "the request failed",
        }
        .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(origin: &str) -> Result<Transport> {
        Transport::new(
            Url::parse(origin).unwrap(),
            ApiToken::new("secret-token"),
            &TransportConfig::default(),
        )
    }

    #[test]
    fn non_http_schemes_are_refused_at_construction() {
        for bad in ["ftp://host/", "file:///tmp/x"] {
            let t = Transport::new(
                Url::parse(bad).unwrap(),
                ApiToken::new("t"),
                &TransportConfig::default(),
            );
            assert!(t.is_err(), "{bad} should not be usable as a server URL");
        }
    }

    #[test]
    fn origin_comparison_is_scheme_host_and_port() {
        let t = transport("https://miniflux.example/").unwrap();
        assert!(t.is_configured_origin(&Url::parse("https://miniflux.example/v1/me").unwrap()));
        assert!(t.is_configured_origin(&Url::parse("https://miniflux.example:443/v1").unwrap()));
        // A different host is a different origin even if it looks similar.
        assert!(!t.is_configured_origin(&Url::parse("https://miniflux.example.evil/v1").unwrap()));
        assert!(!t.is_configured_origin(&Url::parse("http://miniflux.example/v1").unwrap()));
        assert!(!t.is_configured_origin(&Url::parse("https://miniflux.example:8443/v1").unwrap()));
    }

    #[test]
    fn a_malformed_ca_is_a_configuration_error_not_a_silent_fallback() {
        let config = TransportConfig {
            extra_ca_pem: Some(b"not a certificate".to_vec()),
            ..TransportConfig::default()
        };
        let t = Transport::new(
            Url::parse("https://host.example/").unwrap(),
            ApiToken::new("t"),
            &config,
        );
        assert!(
            t.is_err(),
            "a bad CA must fail loudly, never fall back to no CA"
        );
    }

    #[test]
    fn the_token_is_withheld_from_any_origin_but_the_configured_one() {
        // The origin comparison itself, which is what the off-origin redirect
        // refusal decides on. Getting it wrong -- accepting a different port,
        // a different scheme, or a suffix like `miniflux.example.evil.invalid`
        // -- is a key leak, and the integration test can only exercise one
        // foreign origin.
        //
        // Note what this does NOT cover, because nothing can: the conditional
        // at the attach site (`if self.is_configured_origin(&current)`).
        // `current` only ever advances past the refusal above, so it is always
        // the configured origin by construction, and attaching the header
        // unconditionally there is behaviourally identical. It is deliberate
        // belt-and-braces for a state the refusal makes unreachable -- not a
        // second layer with its own observable behaviour, and not something a
        // test can distinguish.
        let t = Transport::new(
            Url::parse("https://miniflux.example:8080/").unwrap(),
            ApiToken::new("t"),
            &TransportConfig::default(),
        )
        .unwrap();

        assert!(t.is_configured_origin(
            &Url::parse("https://miniflux.example:8080/v1/entries?x=1").unwrap()
        ));

        for foreign in [
            // a different port
            "https://miniflux.example:8443/v1/entries",
            // the default port for the scheme, which is NOT 8080
            "https://miniflux.example/v1/entries",
            // a different scheme
            "http://miniflux.example:8080/v1/entries",
            // a different host
            "https://evil.invalid:8080/v1/entries",
            // A SUBDOMAIN. `ends_with` accepts this, and it is the classic
            // way an origin check written as a suffix comparison leaks a
            // credential -- anyone who can create a host under the domain gets
            // the token.
            "https://evil.miniflux.example:8080/v1/entries",
            // A host that merely ends with the same characters, which a
            // careless suffix comparison also accepts.
            "https://notminiflux.example:8080/v1/entries",
            // And the other direction: a longer host that starts the same.
            "https://miniflux.example.evil.invalid:8080/v1/entries",
            "https://miniflux.example.co:8080/v1/entries",
        ] {
            assert!(
                !t.is_configured_origin(&Url::parse(foreign).unwrap()),
                "{foreign} is not the configured origin; the token must not be attached"
            );
        }
    }

    #[test]
    fn there_is_no_way_to_disable_verification() {
        // A guard against someone adding a "trust invalid certs" toggle later.
        // §9.1 is explicit that this gets no setting.
        //
        // Scans the WHOLE crate, not `include_str!("transport.rs")`. Reading
        // only its own file made the guard trivial to walk around: a
        // `ClientBuilder` configured in any sibling module -- api/icon.rs,
        // api/client.rs, a new api/media.rs -- was invisible to it, and
        // `reqwest` is a workspace dependency so any of them can build one.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(
            files.len() > 5,
            "found too few sources to be scanning the crate"
        );

        // The needles are assembled at runtime so that this test's own text
        // does not contain the literals it forbids.
        let needles = [
            ["danger_accept", "_invalid_certs"].concat(),
            ["danger_accept", "_invalid_hostnames"].concat(),
            ["tls_built_in", "_root_certs"].concat(),
            // Hands the whole verifier over to a caller-supplied one.
            ["use_preconfigured", "_tls"].concat(),
            ["dangerous", "()"].concat(),
            ["cookie", "_store"].concat(),
        ];

        for file in files {
            let source = std::fs::read_to_string(&file).unwrap_or_default();
            // This test's own module is where the needles are written down.
            let source = source.split("#[cfg(test)]").next().unwrap_or("").to_owned();
            for needle in &needles {
                assert!(
                    !source.contains(needle.as_str()),
                    "TLS verification must not be defeatable: found {needle} in {}",
                    file.display()
                );
            }
        }
    }

    /// Every `.rs` file under a directory.
    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
}
