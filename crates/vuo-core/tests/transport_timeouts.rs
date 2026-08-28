//! §9.1: *set timeouts on everything, including connect, read, and total
//! request duration. A server that accepts a connection and never speaks must
//! not wedge a sync forever.*
//!
//! Three shapes, all of which a real phone meets on a bad network:
//! a server that accepts and says nothing, one that sends headers then stalls
//! mid-body, and one that dribbles a byte at a time forever — the last being
//! the interesting case, because it never stalls long enough to trip a read
//! timeout and only a TOTAL deadline stops it.

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
use std::time::{Duration, Instant};

use url::Url;
use vuo_core::api::{Transport, TransportConfig};
use vuo_core::redact::ApiToken;

fn transport(origin: &str, c: TransportConfig) -> Transport {
    Transport::new(Url::parse(origin).unwrap(), ApiToken::new("SECRET"), &c).unwrap()
}

/// Server that sends headers then dribbles one byte every `gap`, forever.
fn dribble_server(gap: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf);
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        loop {
            if sock.write_all(b"1\r\nA\r\n").is_err() {
                return;
            }
            let _ = sock.flush();
            std::thread::sleep(gap);
        }
    });
    format!("http://{addr}")
}

/// Server that accepts and never says anything.
fn silent_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf);
        std::thread::sleep(Duration::from_secs(600));
    });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn read_timeout_on_silent_server() {
    let base = silent_server();
    let t = transport(
        &base,
        TransportConfig {
            read_timeout: Duration::from_millis(400),
            request_timeout: Duration::from_secs(30),
            ..TransportConfig::default()
        },
    );
    let start = Instant::now();
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    println!("silent: {:?} after {:?}", res, start.elapsed());
    assert!(res.is_err());
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "read timeout must fire"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn total_timeout_during_slow_body() {
    // Dribbles below the read timeout forever: only a TOTAL deadline stops it.
    let base = dribble_server(Duration::from_millis(50));
    let t = transport(
        &base,
        TransportConfig {
            read_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_millis(800),
            max_response_bytes: 100 * 1024 * 1024,
            ..TransportConfig::default()
        },
    );
    let start = Instant::now();
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    println!("dribble: {:?} after {:?}", res, start.elapsed());
    assert!(res.is_err(), "total timeout must stop an endless slow body");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn read_timeout_during_slow_body() {
    let base = dribble_server(Duration::from_millis(900));
    let t = transport(
        &base,
        TransportConfig {
            read_timeout: Duration::from_millis(300),
            request_timeout: Duration::from_secs(60),
            max_response_bytes: 100 * 1024 * 1024,
            ..TransportConfig::default()
        },
    );
    let start = Instant::now();
    let res = t
        .send(
            reqwest::Method::GET,
            Url::parse(&format!("{base}/v1/me")).unwrap(),
            None,
        )
        .await;
    println!(
        "slow-body read timeout: {:?} after {:?}",
        res,
        start.elapsed()
    );
    assert!(res.is_err());
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
}
