//! Hardened HTTP layer.
//!
//! Every §9.1 rule here, each commented with the attack it closes (not recoverable from code).
//!
//! # Redirects followed by hand
//!
//! §9.1: don't follow redirects with API token attached; clients stripping credentials cross-origin
//! usually special-case only `Authorization`.
//!
//! So does `reqwest`: strips `Authorization`, `Cookie`, `Proxy-Authorization` on origin change, not
//! `X-Auth-Token` (non-standard). Hostile or compromised instance answering `302` to attacker host
//! would get API key.
//!
//! So client uses [`redirect::Policy::none`](reqwest::redirect::Policy::none); [`Transport::send`]
//! resolves redirects, attaching token per hop after explicit origin check.
//!
//! # Per-host CA without per-host CA API
//!
//! §9.1: explicit, per-host, user-supplied CA; narrow, auditable, no global verification bypass.
//! `reqwest` can't scope a root to one host.
//!
//! Done structurally: extra CA on **API client only**, client built per account, redirect policy
//! keeps it on configured origin. Private CA trusted for exactly one host.
//!
//! Media: [`Transport::media_client`] carries neither token nor extra CA, but article images are
//! fetched by **Qt** (QML `Image`), not this crate. Rust decides *which URLs UI gets*: media policy
//! runs in transform, before URL reaches QML; that's where §9.3 lives. Client here = media Rust
//! fetches itself (none now) and tests.
//!
//! No way to disable verification: no setting, feature flag, env var.

use std::time::Duration;

use futures_util::StreamExt as _;
use url::Url;

use crate::error::{Error, Result, TransportKind};
use crate::redact::{ApiToken, SafeUrl};

/// Miniflux API-key header. Not `Authorization`, hence manual redirects.
const AUTH_HEADER: &str = "X-Auth-Token";

/// Limits and settings for a [`Transport`]; defaults suit a phone.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Maximum time to establish a connection to the server.
    pub connect_timeout: Duration,
    /// Max time between bytes. Server that accepts then dribbles must not wedge
    /// sync.
    pub read_timeout: Duration,
    /// Ceiling on whole `send`, INCLUDING every redirect hop.
    ///
    /// Shrinking deadline, not per-hop: per-hop × `max_redirects` lets slow
    /// redirector hold sync 4× longer.
    pub request_timeout: Duration,
    /// Hard cap on any response body (§9.1: this is a phone).
    pub max_response_bytes: usize,
    /// Most redirect hops [`Transport::send`] follows before refusing.
    pub max_redirects: usize,
    /// PEM for user-supplied CA (self-hosted, private CA). Trusted only by API
    /// client (see module docs).
    pub extra_ca_pem: Option<Vec<u8>>,
    /// The `User-Agent` sent with every request.
    pub user_agent: String,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(120),
            // Fits page of 500 entries with full content; hostile length can't
            // exhaust device.
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
    /// HTTP status, any non-redirect.
    pub status: u16,
    /// The whole body, no longer than the configured cap.
    pub body: Vec<u8>,
    /// `Date` header, parsed. Sync cursor anchors to server clock: phone clock
    /// untrusted, comparison happens server-side.
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
    /// Build transport for one account.
    ///
    /// No cookie jar: `reqwest` cookies need `cookies` feature, not enabled.
    /// Absent by construction, so no later edit here can undo it. Vuo
    /// authenticates by header; cookie could only be server-set state replayed
    /// where unintended.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when `origin` not `http`/`https` or has no host,
    /// `extra_ca_pem` invalid PEM or holds no certificates, or either HTTP
    /// client can't be built.
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
                // Redirects resolved by hand; see module docs.
                .redirect(reqwest::redirect::Policy::none())
        };

        let mut api_builder = base();
        if let Some(pem) = &config.extra_ca_pem {
            // Additive: platform roots stay trusted; private CA disables no
            // verification.
            let certs = reqwest::Certificate::from_pem_bundle(pem).map_err(|e| {
                Error::Config(format!("the supplied CA certificate is not valid PEM: {e}"))
            })?;
            // `from_pem_bundle` returns `Ok(vec![])` for input without PEM
            // blocks. Without this, typo'd/truncated cert = silently "no extra
            // CA", opaque TLS failure at sync instead of clear error at setup.
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
            .map_err(|e| Error::Config(format!("could not build the HTTP client: {e}")))?;

        // Media client: no token, no extra CA, for third-party hosts.
        // Separation keeps API client's private CA host-scoped.
        let media = base()
            .build()
            .map_err(|e| Error::Config(format!("could not build the media HTTP client: {e}")))?;

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

    /// Configured server origin; only one token goes to.
    #[must_use]
    pub fn origin(&self) -> &Url {
        &self.origin
    }

    /// Client for media: no token, no cookies, no private CA.
    ///
    /// Not used by article view (Qt fetches images). See module docs on where
    /// §9.3 lives.
    #[must_use]
    pub fn media_client(&self) -> &reqwest::Client {
        &self.media
    }

    /// True when `url` is configured instance's origin.
    fn is_configured_origin(&self, url: &Url) -> bool {
        url.scheme() == self.origin.scheme()
            && url.host_str() == self.origin.host_str()
            && url.port_or_known_default() == self.origin.port_or_known_default()
    }

    /// Request, following redirects under explicit policy.
    ///
    /// Any non-redirect status (4xx, 5xx too) returns `Ok` for caller; except
    /// 304-306.
    ///
    /// # Errors
    ///
    /// [`Error::Transport`] when server unreachable, TLS fails, timeout
    /// (deadline covers every hop), redirect refused (too many hops,
    /// missing/invalid `Location`, https downgrade, off configured origin), or
    /// body over [`TransportConfig::max_response_bytes`] (declared or read).
    /// [`Error::Http`] for 304, 305, 306.
    pub async fn send(
        &self,
        method: reqwest::Method,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> Result<BoundedResponse> {
        let mut current = url;
        let mut hops = 0usize;
        // One deadline for whole call, not per hop.
        let started = std::time::Instant::now();

        loop {
            let safe = SafeUrl::from(&current);

            // Token attached only for configured origin.
            //
            // Belt-and-braces: origin check below refuses off-origin hops, so
            // condition always true here. Second line of defence if refusal
            // loosens; unreachable, so no test can observe it (see
            // `tests::the_token_is_withheld_from_any_origin_but_the_configured_one`).
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

            let response = req.send().await.map_err(|e| classify(&e, &safe))?;
            let status = response.status();

            // 304/305/306 are 3xx but not redirects; 304 has no Location. Vuo
            // sends no conditional headers, so 304 = misbehaving server/proxy:
            // say that, not "redirect without a Location header".
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

                let next = current.join(location).map_err(|e| Error::Transport {
                    endpoint: safe.clone(),
                    kind: TransportKind::RedirectRefused,
                    detail: format!("redirect Location is not a valid URL: {e}"),
                })?;

                // Refuse downgrade to plaintext (§9.1).
                if current.scheme() == "https" && next.scheme() != "https" {
                    return Err(Error::Transport {
                        endpoint: safe,
                        kind: TransportKind::RedirectRefused,
                        detail: "refused a redirect from https to plaintext".to_owned(),
                    });
                }

                // Refuse leaving configured origin at all, stricter than
                // dropping token: API redirecting off-instance is misconfigured
                // or hostile. User gets actionable error about server URL.
                if !self.is_configured_origin(&next) {
                    return Err(Error::Transport {
                        endpoint: safe,
                        kind: TransportKind::RedirectRefused,
                        detail: "refused a redirect away from the configured server".to_owned(),
                    });
                }

                hops = hops.saturating_add(1);
                current = next;
                continue;
            }

            let server_date = response
                .headers()
                .get(reqwest::header::DATE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| chrono::DateTime::parse_from_rfc2822(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));

            // Reject on declared length before reading, enforce again while
            // reading: Content-Length is foreign, may be absent or lie.
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

    /// Read body incrementally, abandon once over cap.
    ///
    /// §9.1: bound every response, cap body, read incrementally. `bytes()` then
    /// length check = allocation already made.
    async fn read_bounded(&self, response: reqwest::Response, safe: &SafeUrl) -> Result<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| classify(&e, safe))?;
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

/// Turn `reqwest::Error` into ours **without** keeping it as source.
///
/// Its `Display` embeds failing URL; chaining leaks base-URL userinfo past
/// [`crate::redact`]. Keep classification only; detail string fixed, nothing
/// foreign.
fn classify(e: &reqwest::Error, endpoint: &SafeUrl) -> Error {
    // `is_connect` FIRST: reqwest sets `is_timeout` on connect timeouts too.
    // "Nothing answered" (network/VPN) and "accepted then stalled" (server)
    // need different user actions.
    let kind = if e.is_connect() {
        // reqwest folds TLS failures into connect errors. Split them: "check
        // certificate" vs "check network" are different actions.
        if format!("{e:?}").contains("certificate") || format!("{e:?}").contains("Tls") {
            TransportKind::Tls
        } else {
            TransportKind::Connect
        }
    } else if e.is_timeout() {
        TransportKind::Timeout
    } else {
        // Body and decode failures not split: callers act on retry class; both
        // = "response unusable".
        TransportKind::Other
    };

    Error::Transport {
        endpoint: endpoint.clone(),
        kind,
        // Fixed string per class, not `e.to_string()`.
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

    #[tokio::test]
    async fn an_unreachable_server_says_so_rather_than_just_timing_out() {
        // Connect timeout sets BOTH `is_timeout` and `is_connect`; must
        // classify as connect (network/VPN), not vague "timed out".
        //
        // 203.0.113.0/24 = TEST-NET-3 (RFC 5737), routed nowhere: connects to
        // nothing whether runner has network or not.
        let config = TransportConfig {
            connect_timeout: Duration::from_millis(250),
            request_timeout: Duration::from_secs(5),
            ..TransportConfig::default()
        };
        let t = Transport::new(
            Url::parse("http://203.0.113.1:8083/").unwrap(),
            ApiToken::new("t"),
            &config,
        )
        .unwrap();

        let err = t
            .send(
                reqwest::Method::GET,
                Url::parse("http://203.0.113.1:8083/v1/me").unwrap(),
                None,
            )
            .await
            .expect_err("nothing answers there");
        match err {
            Error::Transport { kind, .. } => assert_eq!(
                kind,
                TransportKind::Connect,
                "an unreachable address is a reachability failure, not a stalled server"
            ),
            other => panic!("expected a transport error, got {other:?}"),
        }
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
        // Different host = different origin even if similar.
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
        // Origin comparison behind off-origin redirect refusal. Accepting other
        // port, scheme, or suffix like `miniflux.example.evil.invalid` = key
        // leak; integration test exercises only one foreign origin.
        //
        // Not covered, can't be: conditional at attach site (`if
        // self.is_configured_origin(&current)`). `current` only advances past
        // refusal, so always configured origin; belt-and-braces, unobservable.
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
            // scheme's default port, NOT 8080
            "https://miniflux.example/v1/entries",
            // a different scheme
            "http://miniflux.example:8080/v1/entries",
            // a different host
            "https://evil.invalid:8080/v1/entries",
            // SUBDOMAIN. `ends_with` accepts it: classic suffix-check
            // credential leak; anyone with a host under the domain gets token.
            "https://evil.miniflux.example:8080/v1/entries",
            // Host merely ending in same chars; careless suffix check accepts
            // it.
            "https://notminiflux.example:8080/v1/entries",
            // Other direction: longer host starting the same.
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
        // Guard against later "trust invalid certs" toggle. §9.1: no setting.
        //
        // Scans WHOLE crate, not just this file: `reqwest` is a workspace
        // dependency, any module can build a `ClientBuilder`.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(
            files.len() > 5,
            "found too few sources to be scanning the crate"
        );

        // Needles assembled at runtime so this test's text lacks the forbidden
        // literals.
        let needles = [
            ["danger_accept", "_invalid_certs"].concat(),
            ["danger_accept", "_invalid_hostnames"].concat(),
            ["tls_built_in", "_root_certs"].concat(),
            // Hands whole verifier to caller-supplied one.
            ["use_preconfigured", "_tls"].concat(),
            ["dangerous", "()"].concat(),
            ["cookie", "_store"].concat(),
        ];

        for file in files {
            let source = std::fs::read_to_string(&file).unwrap_or_default();
            // This test module holds the needles.
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
