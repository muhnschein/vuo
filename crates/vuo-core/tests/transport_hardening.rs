//! §9.1 transport hardening against raw TCP servers.
//!
//! `wiremock` well-behaved: wrong tool; cases that matter come from *hostile or
//! broken* server. Each test hand-writes wire bytes.
//!
//! Protects:
//!
//! - `Content-Length` lying either direction: header foreign, can't be only
//!   bound; declared length past cap refused before body read;
//! - rendered error after real transport failure: URL and query not in message;
//! - chunked response with no length (how unbounded body really arrives);
//! - gzip bomb: cap applies to DECOMPRESSED size;
//! - redirect loop, and protocol-relative `//host` Location (likeliest to slip
//!   past string-comparison origin check);
//! - same-origin relative redirect: token IS still sent where it should be
//!   (test only proving refusals passes on client refusing everything);
//! - absolute redirect to SECOND raw server: that server hears nothing. §9.1's
//!   headline rule.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    unreachable_pub,
    dead_code
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use url::Url;
use vuo_core::api::{Transport, TransportConfig};
use vuo_core::error::TransportKind;
use vuo_core::redact::ApiToken;
use vuo_core::Error;

fn cfg() -> TransportConfig {
    TransportConfig {
        max_response_bytes: 4096,
        ..TransportConfig::default()
    }
}

/// Announce on wire what harness does: one response per connection, then socket
/// gone.
///
/// Canned responses silent on keep-alive; HTTP/1.1 reads silence as persistent,
/// so reqwest pools socket. Redirect walk draws same socket after `raw_server`
/// dropped it: request dies as transport error before code under test (~1 in 40
/// runs of `redirect_loop_hop_cap`).
///
/// Spliced here, not in each test's bytes: later test can't forget. Framing,
/// not content: every response still declares own body as written.
fn one_shot(resp: &[u8]) -> Vec<u8> {
    const CLOSE: &[u8] = b"Connection: close\r\n";
    let after_status_line = resp
        .windows(2)
        .position(|w| w == b"\r\n")
        .expect("a canned response begins with a status line")
        + 2;
    let mut out = Vec::with_capacity(resp.len() + CLOSE.len());
    out.extend_from_slice(&resp[..after_status_line]);
    out.extend_from_slice(CLOSE);
    out.extend_from_slice(&resp[after_status_line..]);
    out
}

/// Raw HTTP server serving given responses, one per connection, in order. Each
/// request head read published on returned channel.
fn raw_server(
    responses: Vec<Vec<u8>>,
) -> (String, mpsc::Receiver<String>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || {
        for resp in responses {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = sock.write_all(&one_shot(&resp));
            let _ = sock.flush();
        }
    });
    (format!("http://{addr}"), rx, h)
}

fn transport(origin: &str, config: &TransportConfig) -> Transport {
    Transport::new(
        Url::parse(origin).unwrap(),
        ApiToken::new("SECRET-TOKEN"),
        config,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn relative_redirect_is_followed_with_token() {
    let r1 = b"HTTP/1.1 302 Found\r\nLocation: /v1/other\r\nContent-Length: 0\r\n\r\n".to_vec();
    let r2 = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec();
    let (base, rx, _h) = raw_server(vec![r1, r2]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    eprintln!("result: {res:?}");
    let req1 = rx.recv().unwrap();
    let req2 = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    eprintln!("--- req1 ---\n{req1}\n--- req2 ---\n{req2}");
    assert!(
        req2.contains("SECRET-TOKEN"),
        "token should be re-attached same-origin"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_absolute_cross_origin_redirect_never_reaches_the_other_host() {
    // §9.1 headline rule: "Do not follow redirects with the API token
    // attached." Stands up SECOND host and checks what it received: removing
    // off-origin refusal and conditional attach must fail here.
    //
    // Module doc's opening scenario: hostile or compromised instance answering
    // 302 to somewhere it controls.
    let (attacker, attacker_rx, _ah) = raw_server(vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    ]);
    let redirect =
        format!("HTTP/1.1 302 Found\r\nLocation: {attacker}/collect\r\nContent-Length: 0\r\n\r\n");
    let (base, rx, _h) = raw_server(vec![redirect.into_bytes()]);

    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/entries")).unwrap(),
            None,
        )
        .await;

    let first = rx.recv().unwrap();
    assert!(
        first.contains("SECRET-TOKEN"),
        "the token belongs on the CONFIGURED origin; without this the test could \
         pass on a client that never authenticates at all"
    );

    assert!(
        matches!(
            res,
            Err(Error::Transport {
                kind: TransportKind::RedirectRefused,
                ..
            })
        ),
        "a redirect off the configured origin must be refused: {res:?}"
    );

    // Other host heard nothing: no unauthenticated request, no connection.
    match attacker_rx.recv_timeout(Duration::from_millis(500)) {
        Err(_) => {}
        Ok(head) => panic!(
            "the redirect was followed to another origin. It received:\n{head}\n\
             (a request at all is a failure; one carrying the token is a key leak)"
        ),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn protocol_relative_redirect() {
    // Location: //evil.invalid/x  -> should be refused
    let r1 = b"HTTP/1.1 302 Found\r\nLocation: //evil.invalid/steal\r\nContent-Length: 0\r\n\r\n"
        .to_vec();
    let (base, rx, _h) = raw_server(vec![r1]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    eprintln!("protocol-relative result: {res:?}");
    let _ = rx.recv().unwrap();
    res.unwrap_err();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_304_is_reported_as_a_protocol_error_not_a_redirect() {
    let r1 = b"HTTP/1.1 304 Not Modified\r\n\r\n".to_vec();
    let (base, rx, _h) = raw_server(vec![r1]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    let _ = rx.recv().unwrap();
    // Point = CLASSIFICATION, not mere failure. Without 304..=306 branch, 304
    // falls into `is_redirection()`, finds no Location, surfaces as
    // RedirectRefused "redirect without a Location header": confusion branch
    // exists to avoid.
    assert!(
        matches!(res, Err(Error::Http { status: 304, .. })),
        "a 304 must be reported as an HTTP status, not as a refused redirect: {res:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_loop_hop_cap() {
    let r = b"HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n".to_vec();
    // Far more canned responses than cap on purpose. Few → server runs out,
    // next connect fails, `is_err()` holds for reason unrelated to
    // `max_redirects`.
    let (base, rx, _h) = raw_server(vec![r; 32]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/loop")).unwrap(),
            None,
        )
        .await;
    assert!(
        matches!(
            res,
            Err(Error::Transport {
                kind: TransportKind::RedirectRefused,
                ..
            })
        ),
        "the hop cap must be what refuses, not the network: {res:?}"
    );

    // Cap stopped it: exactly original request plus `max_redirects` follows
    // reached server, nothing after. `send` returned, nothing in flight;
    // polling would measure machine load.
    let expected = cfg().max_redirects + 1;
    for i in 0..expected {
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("request {i} of {expected} never arrived: {e}"));
    }
    assert!(
        rx.try_recv().is_err(),
        "a request past the cap reached the server: the walk was not stopped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn chunked_no_content_length_over_cap() {
    // 8 chunks of 1024 bytes = 8192 > cap 4096, chunked, no Content-Length.
    let mut r = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..8 {
        r.extend_from_slice(b"400\r\n");
        r.extend(std::iter::repeat_n(b'A', 1024));
        r.extend_from_slice(b"\r\n");
    }
    r.extend_from_slice(b"0\r\n\r\n");
    let (base, rx, _h) = raw_server(vec![r]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    eprintln!("chunked result: {:?}", res.as_ref().map(|r| r.body.len()));
    eprintln!("chunked err: {res:?}");
    let _ = rx.recv().unwrap();
    assert!(res.is_err(), "chunked body over cap must be refused");
}

#[tokio::test(flavor = "multi_thread")]
async fn gzip_bomb() {
    let bomb = include_bytes!("fixtures/gzip_bomb.gz");
    let mut r = format!(
        "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
        bomb.len()
    )
    .into_bytes();
    r.extend_from_slice(bomb);
    let (base, rx, _h) = raw_server(vec![r]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    eprintln!("gzip result len: {:?}", res.as_ref().map(|r| r.body.len()));
    eprintln!("gzip err: {res:?}");
    let _ = rx.recv().unwrap();
    assert!(
        res.is_err(),
        "8MB decompressed from 8KB must trip the 4096-byte cap"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lying_content_length_small_body_big() {
    let mut r = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n".to_vec();
    r.extend(std::iter::repeat_n(b'B', 100_000));
    let (base, rx, _h) = raw_server(vec![r]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    let _ = rx.recv().unwrap();
    // Declared length = what framing believes: body = promised 10 bytes,
    // remaining 100 KB never delivered. Pins direction: reading to EOF would
    // return 100_000.
    let body = res.expect("a short body is not an error").body;
    assert_eq!(
        body.len(),
        10,
        "a lying Content-Length must not be exceeded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn content_length_above_the_cap_is_refused_before_the_body() {
    // Other direction of same lie, §9.1 wants caught "before reading a byte":
    // declared length far past cap refused on header alone. Incremental cap in
    // `read_bounded` would also stop it eventually: needs own test, else
    // streaming cap silently covers disabled pre-check.
    let mut r = b"HTTP/1.1 200 OK\r\nContent-Length: 4294967296\r\n\r\n".to_vec();
    r.extend(std::iter::repeat_n(b'C', 64));
    let (base, rx, _h) = raw_server(vec![r]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    let _ = rx.recv().unwrap();
    assert!(
        matches!(
            res,
            Err(Error::Transport {
                kind: TransportKind::BodyTooLarge,
                ..
            })
        ),
        "a declared length of 4 GiB must be refused on the header: {res:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_request_renders_without_the_url_or_its_query() {
    // §9.1: "Never log the token or a URL containing it. Redact in error paths
    // too." Guard: `classify` uses fixed string per class, not `e.to_string()`
    // (Display carries full URL incl. query). Drives REAL failure through
    // `send`: hand-built `Error` can only hold `SafeUrl`, proves by
    // construction, tests nothing.
    //
    // Closed port: bound, then dropped; connect refused.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let base = format!("http://{addr}");

    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/entries?marker=CANARY-IN-THE-QUERY")).unwrap(),
            None,
        )
        .await;
    let err = res.expect_err("a closed port must fail");
    for rendering in [err.to_string(), format!("{err:?}")] {
        assert!(
            !rendering.contains("CANARY-IN-THE-QUERY"),
            "the query reached a rendered error: {rendering}"
        );
        assert!(
            !rendering.contains("SECRET-TOKEN"),
            "the token reached a rendered error: {rendering}"
        );
    }
}
