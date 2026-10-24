//! Icon fetching, and starvation hinted by §11's "thundering herd".
//!
//! Few icons per sync, not all: §11's answer to first-sync herd. Batching
//! brings own failure: batch ordered, so permanently undecodable icons at front
//! re-fetched every sync, feeds behind never get one. Without failure tracking:
//! 40 requests over five passes, zero icons stored, two good icons never
//! reached.

// Test code: see note in vuo-core's lib.rs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use base64::Engine as _;
use common::*;
use vuo_core::sync::{self, SyncOptions};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A minimal valid PNG header of the given size.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0]);
    v
}

fn icon_body(id: i64, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "mime_type": "image/png",
        "data": format!(
            "image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    })
}

#[tokio::test]
async fn undecodable_icons_do_not_starve_the_feeds_behind_them() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![category_json(1, "News")]))
        .mount(&server)
        .await;

    // Ten feeds. First eight serve undecodable garbage; last two real PNGs.
    // Batch of eight per pass without failure tracking never reaches good ones.
    let feeds: Vec<serde_json::Value> = (1..=10)
        .map(|i| {
            let mut f = feed_json(i, &format!("Feed {i}"), 1);
            f["icon"] = serde_json::json!({ "feed_id": i, "icon_id": i });
            f
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/v1/feeds"))
        .respond_with(ResponseTemplate::new(200).set_body_json(feeds))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/entries"))
        .respond_with(ResponseTemplate::new(200).set_body_json(entries_response(vec![], 0)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": "2.2.0"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/feeds/counters"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "reads": {}, "unreads": {}
        })))
        .mount(&server)
        .await;

    for i in 1..=8i64 {
        Mock::given(method("GET"))
            .and(path(format!("/v1/feeds/{i}/icon")))
            .respond_with(ResponseTemplate::new(200).set_body_json(icon_body(i, b"not an image")))
            .mount(&server)
            .await;
    }
    for i in 9..=10i64 {
        Mock::given(method("GET"))
            .and(path(format!("/v1/feeds/{i}/icon")))
            .respond_with(ResponseTemplate::new(200).set_body_json(icon_body(i, &png(32, 32))))
            .mount(&server)
            .await;
    }

    let client = client_for(&server);
    let mut db = memory_db();

    for _ in 0..5 {
        sync::sync(&mut db, &client, SyncOptions::default())
            .await
            .unwrap();
    }

    let stored: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM icons", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        stored, 2,
        "the two decodable icons must eventually be fetched; broken ones must not \
         monopolise the batch forever"
    );

    // Broken ones no longer asked for.
    let attempts: i64 = db
        .conn()
        .query_row("SELECT MAX(icon_failures) FROM feeds", [], |r| r.get(0))
        .unwrap();
    assert!(
        attempts >= 3,
        "failures should be recorded so the retry stops"
    );
    assert!(
        attempts <= 4,
        "and should stop, rather than counting up forever: {attempts}"
    );

    // Now REQUESTS, not just rows. §8.3: mock is contract; what was sent is
    // half. Assertions above are database state; many request patterns satisfy
    // them (e.g. dropping "skip icons we already have" from
    // `store::feeds_missing_icons`, re-fetching every icon every sync).
    let requests = server.received_requests().await.expect("recorded requests");
    let icon_requests: Vec<&str> = requests
        .iter()
        .map(|r| r.url.path())
        .filter(|p| p.starts_with("/v1/feeds/") && p.ends_with("/icon"))
        .collect();

    // Two good icons stored on first success, never asked again.
    for feed in [9i64, 10] {
        let asked = icon_requests
            .iter()
            .filter(|p| **p == format!("/v1/feeds/{feed}/icon"))
            .count();
        assert_eq!(
            asked, 1,
            "feed {feed}'s icon decoded on the first try, so it must never be \
             re-fetched; it was requested {asked} times across five syncs"
        );
    }

    // Broken ones stop too: total bounded, doesn't grow with passes.
    assert!(
        icon_requests.len() <= 8 * 4 + 2,
        "icon fetching is unbounded: {} requests across five syncs",
        icon_requests.len()
    );
}
