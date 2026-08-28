//! (Some fixtures are used by only one of the integration test binaries.)
#![allow(dead_code)]

//! A mock Miniflux, and the fixtures the sync tests are written against.
//!
//! §8.3: the mock HTTP server *is* the main new infrastructure this project
//! needs, and it is what makes milestone 1 possible without a phone or a
//! server. Everything here runs on a plain host toolchain with no network.

use vuo_core::api::{MinifluxClient, Transport, TransportConfig};
use vuo_core::db::Database;
use vuo_core::redact::ApiToken;
use wiremock::MockServer;

/// Build a client pointed at a mock server.
pub fn client_for(server: &MockServer) -> MinifluxClient {
    let origin = url::Url::parse(&server.uri()).expect("mock server URI");
    let transport = Transport::new(origin, ApiToken::new("test-token"), &TransportConfig::default())
        .expect("transport");
    MinifluxClient::new(transport)
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
