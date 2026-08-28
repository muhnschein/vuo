#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use qmetaobject::*;
use vuo_core::model::*;

#[test]
fn probe_articlepage_renders_body() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().and_then(|p| p.parent()).unwrap().to_path_buf();
    let dir = tempfile::tempdir().unwrap();
    let dbpath = dir.path().join("m.sqlite");
    {
        let mut db = vuo_core::db::Database::open(&dbpath).unwrap();
        db.with_tx(|tx| {
            vuo_core::db::store::upsert_feed(tx, &Feed {
                id: FeedId(1), category_id: None, title: "F".into(),
                site_url: None, feed_url: None, icon_id: None,
                checked_at: None,
                parsing_error_message: String::new(), parsing_error_count: 0,
                disabled: false, hide_globally: false,
            }, 1)?;
            vuo_core::db::store::upsert_entry(tx, &Entry {
                id: EntryId(7), feed_id: FeedId(1), status: EntryStatus::Unread,
                starred: false, title: "T".into(), url: None, comments_url: None,
                author: String::new(),
                content: "<h2>Head</h2><p>Hello <b>world</b></p><ul><li>one</li></ul><pre>code</pre>".into(),
                published_at: None, created_at: None, changed_at: None,
                reading_time: 1, tags: vec![], enclosures: vec![],
            }, 1)
        }).unwrap();
    }

    vuo_shim::register_qml_types();
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(root.join("qml-stubs").to_string_lossy().to_string()));
    engine.add_import_path(QString::from(root.join("qml").to_string_lossy().to_string()));

    let db = vuo_core::db::Database::open(&dbpath).unwrap();
    let server = url::Url::parse("https://miniflux.example/").unwrap();
    let worker = vuo_shim::worker::Worker::spawn(
        dbpath.clone(), server.clone(),
        vuo_core::redact::ApiToken::new("t".to_owned()),
        vuo_core::api::TransportConfig::default(),
        |_e| {},
    );
    let ctx = vuo_shim::context::AppContext::new(db, worker, server);
    vuo_shim::context::install(ctx);

    let probe = root.join("qml").join("zz_probe_article.qml");
    std::fs::write(&probe, r#"
import QtQuick 2.6
import Sailfish.Silica 1.0
import "pages"
Item {
    width: 540; height: 960
    ArticlePage { id: p; width: 540; height: 960; entryId: 7; entryTitle: "T" }
    Component.onCompleted: {
        console.warn("PROBE ok, page =", p)
    }
}
"#).unwrap();
    engine.load_file(QString::from(probe.to_string_lossy().to_string()));
    // let the view lay out
    let _ = std::fs::remove_file(&probe);
}
