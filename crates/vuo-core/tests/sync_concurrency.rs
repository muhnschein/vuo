//! Proof that the taxonomy step actually puts both listings in flight.
//!
//! # Why this needs raw sockets
//!
//! The pass overlaps its two independent listings only where the connection
//! multiplexes, because over HTTP/1.1 a second request in flight is a second
//! TCP connection and a second TLS handshake -- more than the round trip it
//! saves. That makes `concurrent` a parameter, and a parameter nothing checks
//! is a parameter that can quietly stop being read: replacing the branch in
//! `taxonomy_with` with the sequential half leaves every other test in the
//! suite green, because both halves fetch the same two listings and store the
//! same rows.
//!
//! `wiremock` cannot tell the difference either. It answers whatever arrives,
//! in whatever order, and records no arrival times -- so "were these two in
//! flight together" is not a question it can be asked. A timing assertion
//! could approximate it, but a threshold on a shared CI runner is a flake
//! waiting to happen.
//!
//! So there are two servers here, and between them they pin both halves of the
//! branch with no thresholds and nothing to tune:
//!
//! - a **rendezvous**, which accepts two connections and answers NEITHER until
//!   both have arrived. Concurrent requests satisfy it; sequential ones
//!   deadlock, because the second is never sent, and the timeout turns that
//!   into a failure with a name on it.
//! - a **connection counter**, a keep-alive server that records how many TCP
//!   connections were opened to it. One request at a time reuses a single
//!   connection; two in flight need a second. That is the cost the whole
//!   arrangement exists to avoid paying over HTTP/1.1, so it is worth having
//!   written down as something that runs rather than as a comment.

// Test code: see the note in vuo-core's lib.rs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use vuo_core::api::{MinifluxClient, Transport, TransportConfig};
use vuo_core::db::{store, Database};
use vuo_core::redact::ApiToken;
use vuo_core::sync::pull;

/// A raw HTTP/1.1 server that will not answer anybody until `n` connections
/// have arrived.
///
/// Returns its base URL and the join handle; the thread ends once it has
/// written all `n` responses.
fn rendezvous_server(n: usize) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut pending = Vec::new();
        // Phase one: take every connection and read its request line, but
        // write nothing. A sequential client never gets past the first pass
        // through this loop, which is the point.
        for _ in 0..n {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let read = sock.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..read]).to_string();
            pending.push((sock, head));
        }
        // Phase two: only now does anyone hear back.
        for (mut sock, head) in pending {
            let body = if head.contains("/v1/categories") {
                serde_json::to_vec(&vec![category_json(1, "News")]).unwrap()
            } else if head.contains("/v1/feeds") {
                serde_json::to_vec(&vec![feed_json(7, "Rendezvous Feed", 1)]).unwrap()
            } else {
                b"[]".to_vec()
            };
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes());
            let _ = sock.write_all(&body);
            let _ = sock.flush();
        }
    });
    (format!("http://{addr}"), handle)
}

fn client_at(base: &str) -> MinifluxClient {
    let transport = Transport::new(
        url::Url::parse(base).unwrap(),
        ApiToken::new("test-token"),
        &TransportConfig::default(),
    )
    .unwrap();
    MinifluxClient::new(transport)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_two_taxonomy_listings_are_asked_for_together_when_told_they_can_be() {
    let (base, _handle) = rendezvous_server(2);
    let client = client_at(&base);
    let mut db = Database::open_in_memory().unwrap();

    let done = tokio::time::timeout(
        Duration::from_secs(20),
        pull::taxonomy_with(&mut db, &client, 1, true),
    )
    .await;

    match done {
        Err(_) => panic!(
            "the taxonomy step never opened a second request: the server was still \
             waiting for one when the test gave up. `concurrent = true` is being ignored, \
             so the pass is a round trip longer than it should be on every sync."
        ),
        Ok(result) => result.expect("both listings must land"),
    }

    // And the concurrent path must leave the mirror in the same state the
    // sequential one does -- it is a different way of asking, not a different
    // thing to ask for.
    let feeds = store::feeds(db.conn()).unwrap();
    assert_eq!(feeds.len(), 1, "the feed listing must have been stored");
    assert_eq!(feeds[0].id.get(), 7);
    let categories = store::categories(db.conn()).unwrap();
    assert_eq!(
        categories.len(),
        1,
        "the category listing must have been stored"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_connection_is_enough_when_the_connection_cannot_multiplex() {
    // The other half of the branch, and the reason it exists. Over HTTP/1.1
    // the two requests can only overlap by opening a second TCP connection --
    // and on a phone, where every pass starts with a cold pool, that is a
    // second TLS handshake bought to save tens of milliseconds off a radio
    // event whose tail is measured in seconds. A loss.
    //
    // So the sequential path must take turns on ONE connection, and the
    // concurrent path must be the thing that opens a second. Both are asserted
    // here, against the same server, because either one alone can be satisfied
    // by a `taxonomy_with` that ignores its argument.
    let (base, connections) = counting_server();
    let client = client_at(&base);

    let mut db = Database::open_in_memory().unwrap();
    pull::taxonomy_with(&mut db, &client, 1, false)
        .await
        .expect("both listings must land");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "asking one at a time must reuse the connection, not open a second: \
         a handshake per request is exactly what not overlapping is for"
    );

    // Same client, so the pool is warm and the first connection is sitting
    // idle and reusable. A second one can now only mean two requests in
    // flight at once.
    let mut db = Database::open_in_memory().unwrap();
    pull::taxonomy_with(&mut db, &client, 1, true)
        .await
        .expect("both listings must land");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "overlapping on HTTP/1.1 costs a second connection -- if this is still \
         1, the concurrent branch did not run and the rendezvous test above is \
         the only thing holding it up"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_taxonomy_step_that_cannot_read_both_listings_stores_neither() {
    // Not about concurrency, but it is what makes the concurrency safe to add.
    // `taxonomy` DELETES local feeds that are missing from the server's
    // listing, so a pass that got categories and then lost the connection must
    // store nothing at all: a half-read server must never be able to look like
    // an emptied account. `try_join!` returns on the first error and drops the
    // other request, which preserves that -- but it preserves it by accident
    // unless something checks.
    let (base, _handle) = rendezvous_server(1);
    let client = client_at(&base);
    let mut db = Database::open_in_memory().unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(20),
        pull::taxonomy_with(&mut db, &client, 1, true),
    )
    .await
    .expect("the step must give up on its own rather than hang");
    assert!(
        result.is_err(),
        "a server that answered only one of the two listings is a failed step"
    );

    assert!(
        store::categories(db.conn()).unwrap().is_empty(),
        "the listing that DID arrive must not be committed on its own"
    );
    assert!(
        store::feeds(db.conn()).unwrap().is_empty(),
        "nor the other one"
    );
}

/// A keep-alive HTTP/1.1 server that counts the connections opened to it.
///
/// Answers immediately, unlike [`rendezvous_server`], and holds each
/// connection open for as many requests as the client wants to send on it --
/// which is what makes the count mean "requests that could not share a
/// connection" rather than "requests".
fn counting_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        while let Ok((sock, _)) = listener.accept() {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || serve_keepalive(sock));
        }
    });
    (format!("http://{addr}"), connections)
}

/// Serve requests on one connection until the client stops sending them.
fn serve_keepalive(mut sock: std::net::TcpStream) {
    let mut pending = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // Requests here are bodiless GETs, so a head is a whole request.
        while !pending.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => pending.extend_from_slice(&buf[..n]),
            }
        }
        let end = pending
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("checked above")
            + 4;
        let head = String::from_utf8_lossy(&pending[..end]).to_string();
        pending.drain(..end);

        let body = if head.contains("/v1/categories") {
            serde_json::to_vec(&vec![category_json(1, "News")]).unwrap()
        } else if head.contains("/v1/feeds") {
            serde_json::to_vec(&vec![feed_json(7, "Counted Feed", 1)]).unwrap()
        } else {
            b"[]".to_vec()
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        if sock.write_all(response.as_bytes()).is_err() || sock.write_all(&body).is_err() {
            return;
        }
        let _ = sock.flush();
    }
}
