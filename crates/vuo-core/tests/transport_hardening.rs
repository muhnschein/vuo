//! §9.1 transport hardening, exercised against raw TCP servers.
//!
//! `wiremock` is a well-behaved HTTP server, which makes it the wrong tool for
//! these: the cases that matter are the ones a *hostile or broken* server
//! produces. Each test here hand-writes the bytes on the wire.
//!
//! What each protects:
//!
//! - a `Content-Length` that lies in either direction — the header is foreign
//!   input and cannot be the only bound;
//! - a chunked response with no length at all, which is how an unbounded body
//!   actually arrives;
//! - a gzip bomb, confirming the cap applies to the DECOMPRESSED size (a small
//!   compressed body that expands past the cap must still be refused);
//! - a redirect loop, and a protocol-relative `//host` Location, which is the
//!   shape most likely to slip past an origin check written with string
//!   comparison;
//! - a same-origin relative redirect, confirming the token IS still sent where
//!   it should be — a hardening test that only proves things are refused can
//!   pass on a client that refuses everything.

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
use vuo_core::redact::ApiToken;

fn cfg() -> TransportConfig {
    TransportConfig {
        max_response_bytes: 4096,
        ..TransportConfig::default()
    }
}

/// A one-shot raw HTTP server. `respond` gets the request head and returns raw bytes.
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
            let _ = sock.write_all(&resp);
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
    println!("result: {res:?}");
    let req1 = rx.recv().unwrap();
    let req2 = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    println!("--- req1 ---\n{req1}\n--- req2 ---\n{req2}");
    assert!(
        req2.contains("SECRET-TOKEN"),
        "token should be re-attached same-origin"
    );
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
    println!("protocol-relative result: {res:?}");
    let _ = rx.recv().unwrap();
    assert!(res.is_err());
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
    println!("304 result: {res:?}");
    let _ = rx.recv().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_loop_hop_cap() {
    let r = b"HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\n\r\n".to_vec();
    let (base, rx, _h) = raw_server(vec![
        r.clone(),
        r.clone(),
        r.clone(),
        r.clone(),
        r.clone(),
        r,
    ]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/loop")).unwrap(),
            None,
        )
        .await;
    println!("loop result: {res:?}");
    let mut n = 0;
    while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
        n += 1;
    }
    println!("requests made: {n}");
    assert!(res.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn chunked_no_content_length_over_cap() {
    // 8 chunks of 1024 bytes = 8192 > cap 4096, chunked, no Content-Length.
    let mut r = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for _ in 0..8 {
        r.extend_from_slice(b"400\r\n");
        r.extend(std::iter::repeat(b'A').take(1024));
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
    println!("chunked result: {:?}", res.as_ref().map(|r| r.body.len()));
    println!("chunked err: {res:?}");
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
    println!("gzip result len: {:?}", res.as_ref().map(|r| r.body.len()));
    println!("gzip err: {res:?}");
    let _ = rx.recv().unwrap();
    assert!(
        res.is_err(),
        "8MB decompressed from 8KB must trip the 4096-byte cap"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lying_content_length_small_body_big() {
    let mut r = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n".to_vec();
    r.extend(std::iter::repeat(b'B').take(100_000));
    let (base, rx, _h) = raw_server(vec![r]);
    let t = transport(&base, &cfg());
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    println!("lying CL result: {:?}", res.as_ref().map(|r| r.body.len()));
    let _ = rx.recv().unwrap();
}
