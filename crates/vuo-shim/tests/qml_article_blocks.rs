//! Article body: each block builds its own kind only.
//!
//! Load test instantiates page with defaults: model empty, no block ever built.
//! Here article's own delegate drives a fake block list, one row per kind, and
//! each row's items are read back: right text in right item, nothing of other
//! kinds built. Built-and-hidden kinds cost every block six layouts: long
//! article opened late on phone.
//!
//! Run under `QT_QPA_PLATFORM=offscreen`; `make check` sets it.

// Test code: panic denials guard production foreign-input paths, not test
// assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use qmetaobject::{QByteArray, QMetaType, QString, QVariant, QmlEngine};

const PROBE_QML: &str = r#"
    import QtQuick 2.6
    Item {
        width: 540; height: 4000
        Loader { id: loader }
        ListModel {
            id: fake
            function row(kind, text) {
                return { blockKind: kind, styledText: text, level: 2, quoteDepth: 0,
                         indent: 0, marker: kind === "list_item" ? "•" : "",
                         needsConsent: false, imageHost: "", imageSource: "",
                         imageRatio: 0.5, imageLink: "",
                         imageAlt: kind === "image" ? "alt words" : "" }
            }
        }
        ListView {
            id: view
            width: 540; height: 4000
            model: fake
        }
        function load(url) {
            loader.setSource(url, { width: 540, height: 960 })
            if (loader.status !== Loader.Ready) { return 'load-failed' }
            var blocks = findIn(loader.item, 'blocks')
            if (!blocks) { return 'no-blocks' }
            var kinds = ["heading", "paragraph", "list_item", "code", "table", "image", "rule"]
            for (var i = 0; i < kinds.length; i++) {
                fake.append(fake.row(kinds[i], kinds[i] + " words"))
            }
            // Article's own delegate, its scope (page, article) intact.
            view.delegate = blocks.delegate
            view.forceLayout()
            return 'ok'
        }
        function findIn(node, name) {
            if (!node) { return null }
            if (node.objectName === name) { return node }
            var kids = node.data !== undefined ? node.data : node.children
            for (var i = 0; kids && i < kids.length; i++) {
                var hit = findIn(kids[i], name)
                if (hit) { return hit }
            }
            return null
        }
        function walk(node, out) {
            var kids = node.children
            for (var i = 0; kids && i < kids.length; i++) {
                out.push(kids[i])
                walk(kids[i], out)
            }
            return out
        }
        function delegateAt(row) {
            var kids = view.contentItem.children
            for (var i = 0; i < kids.length; i++) {
                if (kids[i].y === undefined) { continue }
                var at = view.indexAt(1, kids[i].y + 1)
                if (at === row && kids[i].height > 0) { return kids[i] }
            }
            return null
        }
        // "items|texts|images|height": everything one block built.
        function block(row) {
            var d = delegateAt(row)
            if (!d) { return 'missing' }
            var all = walk(d, [])
            var texts = [], images = 0
            for (var i = 0; i < all.length; i++) {
                if (all[i].text !== undefined && ('' + all[i].text).length > 0
                        && all[i].wrapMode !== undefined) {
                    texts.push('' + all[i].text)
                }
                if (all[i].sourceSize !== undefined && all[i].fillMode !== undefined) {
                    images++
                }
            }
            return all.length + '|' + texts.join('+') + '|' + images + '|' + (d.height > 0)
        }
    }
"#;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repository root")
        .to_path_buf()
}

/// One test: `QmlEngine::new()` builds `QApplication`; one per process.
#[test]
fn every_block_builds_its_own_kind_only() {
    vuo_shim::register_qml_types();
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        repo_root().join("qml-stubs").to_string_lossy().into_owned(),
    ));
    engine.add_import_path(QString::from(
        repo_root().join("qml").to_string_lossy().into_owned(),
    ));
    engine.load_data(QByteArray::from(PROBE_QML));

    macro_rules! call {
        ($name:expr $(, $arg:expr)*) => {{
            let result = engine.invoke_method(
                $name.into(),
                &[$(QVariant::from($arg)),*],
            );
            QString::from_qvariant(result)
                .map(|value| value.to_string())
                .unwrap_or_default()
        }};
    }

    let url = format!(
        "file://{}",
        repo_root().join("qml/pages/ArticlePage.qml").display()
    );
    assert_eq!(call!("load", QString::from(url)), "ok");

    // (row, text items it shows, images it holds). Text = what reader reads in
    // that block, nothing else laid out.
    let expected: [(i32, &str, &str); 7] = [
        (0, "heading words", "0"),
        (1, "paragraph words", "0"),
        (2, "\u{2022}+list_item words", "0"),
        (3, "code words", "0"),
        (4, "table words", "0"),
        (5, "alt words", "1"),
        (6, "", "0"),
    ];
    for (row, texts, images) in expected {
        let got = call!("block", row);
        let parts: Vec<&str> = got.split('|').collect();
        assert_eq!(parts.len(), 4, "row {row}: {got}");
        assert_eq!(parts[1], texts, "row {row} shows its own text only: {got}");
        assert_eq!(
            parts[2], images,
            "row {row} builds an image only if image: {got}"
        );
        assert_eq!(parts[3], "true", "row {row} takes room: {got}");
        let items: usize = parts[0].parse().unwrap();
        // Text block: loader, its item, list marker. Image block: column,
        // consent button (and its parts), image, mouse area, alt label. All
        // kinds built = 16 per block.
        let most = if images == "1" { 10 } else { 4 };
        assert!(
            items <= most,
            "row {row} built {items} items; other kinds built and hidden? {got}"
        );
    }
}
