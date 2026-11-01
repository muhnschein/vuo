//! List rows: long-press menu built on long press, not with each row.
//!
//! Entry list and feed list hand Silica `ListItem.menu` a Component. Inline
//! `ContextMenu` = menu plus items built, bound, hidden on every row a fling
//! brings in. Here each list's own delegate drives a fake row: row holds no
//! menu item; Component, built as Silica builds it (parent = list item),
//! shows row's own actions.
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
        width: 540; height: 960
        Loader { id: loader }
        ListModel { id: fake }
        ListView { id: view; width: 540; height: 960; model: fake }
        // Labels a built menu shows; none may exist before long press.
        property var labels: ["Mark as read", "Mark as unread", "Add favourite",
                              "Remove favourite", "Open in browser", "Feed settings",
                              "Mark feed as read", "Unsubscribe"]
        function load(url) {
            view.delegate = null
            fake.clear()
            loader.source = ""
            loader.setSource(url, { width: 540, height: 960 })
            if (loader.status !== Loader.Ready) { return 'load-failed' }
            var lists = findAll(loader.item, function(n) {
                return n.delegate !== undefined && n.contentItem !== undefined
            }, [])
            if (lists.length === 0) { return 'no-list' }
            fake.append({ entryId: 1, title: "Row title", titleStyled: "Row title",
                          feedName: "Gazette", feedNameStyled: "Gazette", excerpt: "",
                          published: 0, readingTime: 3, starred: false, unread: true,
                          url: "https://example.org/a", feedIcon: "", matchKind: "",
                          feedId: 4, unreadCount: 2, categoryId: 1, crawler: false,
                          feedDisabled: false, hideGlobally: false, errorMessage: "" })
            // List's own delegate, its scope (page, list) intact.
            view.delegate = lists[0].delegate
            view.forceLayout()
            return 'ok'
        }
        function findAll(node, test, out) {
            if (!node) { return out }
            if (test(node)) { out.push(node) }
            var kids = node.data !== undefined ? node.data : node.children
            for (var i = 0; kids && i < kids.length; i++) { findAll(kids[i], test, out) }
            return out
        }
        function listItem() {
            var found = findAll(view.contentItem, function(n) {
                return n.showMenuOnPressAndHold !== undefined
            }, [])
            return found.length > 0 ? found[0] : null
        }
        function texts(node) {
            return findAll(node, function(n) {
                return n.text !== undefined && labels.indexOf('' + n.text) >= 0
                       && n.visible !== false
            }, []).map(function(n) { return '' + n.text })
        }
        // "built|menu kind|texts once built".
        function menu() {
            var item = listItem()
            if (!item) { return 'no-row' }
            var built = texts(item).length
            var menu = item.menu
            if (!menu || typeof menu.createObject !== 'function') {
                return built + '|not-a-component|'
            }
            var made = menu.createObject(item)
            var shown = texts(made).join('+')
            made.destroy()
            return built + '|component|' + shown
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
fn row_menus_are_built_on_long_press_only() {
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
    let url = |path: &str| QString::from(format!("file://{}", repo_root().join(path).display()));

    assert_eq!(call!("load", url("qml/components/EntryListView.qml")), "ok");
    assert_eq!(
        call!("menu"),
        "0|component|Mark as read+Add favourite+Open in browser",
        "article row: no menu built with row; built one acts on that row"
    );

    assert_eq!(call!("load", url("qml/pages/FeedListPage.qml")), "ok");
    assert_eq!(
        call!("menu"),
        "0|component|Feed settings+Mark feed as read+Unsubscribe",
        "feed row: same"
    );
}
