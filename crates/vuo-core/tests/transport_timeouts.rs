//! §9.1: *set timeouts on everything, including connect, read, and total
//! request duration. A server that accepts a connection and never speaks must
//! not wedge a sync forever.*
//!
//! Three shapes a phone meets on bad network: accepts and says nothing; headers
//! then mid-body stall; dribbles a byte at a time forever. Last never stalls
//! long enough for read timeout; only TOTAL deadline stops it.

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
use std::time::Duration;

use url::Url;
use vuo_core::api::{Transport, TransportConfig};
use vuo_core::error::TransportKind;
use vuo_core::redact::ApiToken;
use vuo_core::Error;

/// Timeout no test reaches: only deadline under test can end request.
const NEVER: Duration = Duration::from_secs(3600);

/// Turns hang into failure. Not a timing claim: deadlines under test are
/// 100x shorter; error KIND says which fired.
const WATCHDOG: Duration = Duration::from_secs(60);

fn transport(origin: &str, c: &TransportConfig) -> Transport {
    Transport::new(Url::parse(origin).unwrap(), ApiToken::new("SECRET"), c).unwrap()
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

/// GET `base`/v1/me; fails test if it hangs past [`WATCHDOG`].
async fn get(
    t: &Transport,
    base: &str,
) -> vuo_core::Result<vuo_core::api::transport::BoundedResponse> {
    let url = Url::parse(&format!("{base}/v1/me")).unwrap();
    tokio::time::timeout(WATCHDOG, t.send(reqwest::Method::GET, url, None))
        .await
        .unwrap_or_else(|_| panic!("the request hung for {WATCHDOG:?}: no timeout fired"))
}

fn assert_timed_out<T: std::fmt::Debug>(res: &vuo_core::Result<T>) {
    assert!(
        matches!(
            res,
            Err(Error::Transport {
                kind: TransportKind::Timeout,
                ..
            })
        ),
        "expected a timeout: {res:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn read_timeout_on_silent_server() {
    let base = silent_server();
    let t = transport(
        &base,
        &TransportConfig {
            read_timeout: Duration::from_millis(400),
            request_timeout: NEVER,
            ..TransportConfig::default()
        },
    );
    assert_timed_out(&get(&t, &base).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn total_timeout_during_slow_body() {
    // Dribbles below the read timeout forever: only a TOTAL deadline stops it.
    let base = dribble_server(Duration::from_millis(50));
    let t = transport(
        &base,
        &TransportConfig {
            read_timeout: NEVER,
            request_timeout: Duration::from_millis(800),
            max_response_bytes: 100 * 1024 * 1024,
            ..TransportConfig::default()
        },
    );
    assert_timed_out(&get(&t, &base).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_timeout_during_slow_body() {
    let base = dribble_server(Duration::from_millis(900));
    let t = transport(
        &base,
        &TransportConfig {
            read_timeout: Duration::from_millis(300),
            request_timeout: NEVER,
            max_response_bytes: 100 * 1024 * 1024,
            ..TransportConfig::default()
        },
    );
    assert_timed_out(&get(&t, &base).await);
}
