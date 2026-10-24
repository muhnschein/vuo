//! (Some fixtures used by only one integration test binary.)
// Test code: see note in vuo-core's lib.rs. unwrap/panic denials guard
// production foreign-input paths, not test assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
// Compiled separately into each integration-test binary: items one uses look
// unreachable from others.
#![allow(unreachable_pub)]
#![allow(dead_code)]

//! Mock Miniflux, and fixtures sync tests are written against.
//!
//! §8.3: mock HTTP server *is* main new infrastructure; makes milestone 1
//! possible without phone or server. Plain host toolchain, no network.

use vuo_core::api::{MinifluxClient, Transport, TransportConfig};
use vuo_core::db::Database;
use vuo_core::redact::ApiToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a client pointed at a mock server.
pub fn client_for(server: &MockServer) -> MinifluxClient {
    let origin = url::Url::parse(&server.uri()).expect("mock server URI");
    let transport = Transport::new(
        origin,
        ApiToken::new("test-token"),
        &TransportConfig::default(),
    )
    .expect("transport");
    MinifluxClient::new(transport)
}

/// Answer `GET /v1/version`, asked before any entry-state write: Vuo refuses
/// those below 2.3.2 (`ServerVersion::supports_entry_state_writes`), so every
/// test mounting write endpoint says what server is.
pub async fn mount_version(server: &MockServer, version: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": version
        })))
        .mount(server)
        .await;
}

pub fn memory_db() -> Database {
    Database::open_in_memory().expect("in-memory mirror")
}

/// One entry as the server would serialise it.
pub fn entry_json(id: i64, feed_id: i64, status: &str, starred: bool) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "user_id": 1,
        "feed_id": feed_id,
        "status": status,
        "hash": format!("hash-{id}"),
        "title": format!("Entry {id}"),
        "url": format!("https://blog.example/{id}"),
        "comments_url": "",
        "published_at": "2026-01-02T03:04:05Z",
        "created_at": "2026-01-02T03:04:05Z",
        "changed_at": "2026-01-02T03:04:05Z",
        "content": format!("<p>Body of entry {id}</p>"),
        "author": "A. Writer",
        "share_code": "",
        "starred": starred,
        "reading_time": 3,
        "enclosures": null,
        "tags": null
    })
}

// By value: call builds page inline with `vec![...]`.
#[allow(clippy::needless_pass_by_value)]
pub fn entries_response(entries: Vec<serde_json::Value>, total: i64) -> serde_json::Value {
    serde_json::json!({ "total": total, "entries": entries })
}

pub fn feed_json(id: i64, title: &str, category_id: i64) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "user_id": 1,
        "title": title,
        "site_url": "https://blog.example/",
        "feed_url": "https://blog.example/feed.xml",
        "category": { "id": category_id, "user_id": 1, "title": "News" },
        "icon": null,
        "disabled": false,
        "hide_globally": false,
        "parsing_error_count": 0,
        "parsing_error_message": ""
    })
}

pub fn category_json(id: i64, title: &str) -> serde_json::Value {
    serde_json::json!({ "id": id, "user_id": 1, "title": title, "hide_globally": false })
}
