//! Icon fetching, and the starvation §11's "thundering herd" note hints at.
//!
//! Icons are fetched a few per sync rather than all at once, which is the
//! answer to §11's question about avoiding a herd on first sync. That batching
//! introduces a failure mode of its own: the batch is ordered, so a handful of
//! permanently-undecodable icons at the front are re-fetched on every sync
//! forever and every feed behind them never gets one. Measured before the fix:
//! 40 requests across five passes, zero icons stored, and two perfectly good
//! icons never reached.

// Test code: see the note in vuo-core's lib.rs.
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

    // Ten feeds. The first eight serve garbage that can never decode; the last
    // two serve real PNGs. With a batch of eight per pass and no failure
    // tracking, the good ones are never reached.
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

    // And the broken ones stopped being asked for.
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
}
