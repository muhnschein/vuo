//! The entry list as a search page draws it.
//!
//! The QML load test instantiates the list with no model, so no delegate and
//! no section header is ever built. This gives it a model shaped like an
//! `EntryModel` searching -- roles and counts as Rust hands them over -- and
//! reads back what the reader would see: the groups, the marked title and
//! feed name, the excerpt, and no pulley. And the same list NOT searching,
//! which must draw none of it: a feed's title is foreign text, and outside a
//! search it is rendered plain.
//!
//! Run under `QT_QPA_PLATFORM=offscreen`; `make check` sets it.

// Test code: the panic denials guard foreign-input paths in production, not
// assertions in tests. `borrow_as_ptr` is the Qt harness's engine pointer.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::borrow_as_ptr
)]

use qmetaobject::{QByteArray, QMetaType, QString, QVariant, QmlEngine};

const PROBE_QML: &str = r#"
    import QtQuick 2.0
    Item {
        // An EntryModel as QML sees one: its roles, and the properties and
        // methods the list reads off it.
        ListModel {
            id: fake
            property bool ready: true
            property bool syncing: false
            property int titleMatches: 4
            property int feedMatches: 0
            property int textMatches: 7
            function setSearch(text) {}
            function setScope(kind, id) {}
            function requestSync() {}
            ListElement {
                entryId: 1; feedId: 1; author: ""; unread: true; starred: false
                published: 0; readingTime: 3; url: ""; feedIcon: ""
                title: "Harbour <at> dusk"
                titleStyled: "<b>Harbour</b> &lt;at&gt; dusk"
                feedName: "Harbour Gazette"
                feedNameStyled: "<b>Harbour</b> Gazette"
                matchKind: "title"; excerpt: ""
            }
            ListElement {
                entryId: 2; feedId: 2; author: ""; unread: true; starred: false
                published: 0; readingTime: 0; url: ""; feedIcon: ""
                title: "entry 2"; titleStyled: "entry 2"
                feedName: "Daily"; feedNameStyled: "Daily"
                matchKind: "text"; excerpt: "A walk by the <b>harbour</b>."
            }
        }
        Loader { id: loader }
        function load(url, searching) {
            loader.source = ""
            loader.setSource(url, { width: 540, height: 960, searching: searching,
                                    entryModel: fake })
            if (loader.status !== Loader.Ready) { return 'load-failed' }
            loader.item.forceLayout()
            return 'ok'
        }
        function findAll(node, name, out) {
            if (!node) { return out }
            if (node.objectName === name) { out.push(node) }
            var kids = node.data !== undefined ? node.data : node.children
            for (var i = 0; kids && i < kids.length; i++) { findAll(kids[i], name, out) }
            return out
        }
        // The nth item of that name, top to bottom as the reader sees them.
        function get(name, nth, property) {
            var found = findAll(loader.item, name, [])
            found.sort(function(a, b) {
                return a.mapToItem(loader.item, 0, 0).y - b.mapToItem(loader.item, 0, 0).y
            })
            if (nth >= found.length) { return 'missing:' + name + '#' + nth }
            return '' + found[nth][property]
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

/// One test, because `QmlEngine::new()` builds a `QApplication` and there may
/// be only one of those per process.
#[test]
fn a_search_lists_its_results_grouped_and_marked_and_nothing_else_does() {
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        repo_root().join("qml-stubs").to_string_lossy().into_owned(),
    ));
    engine.load_data(QByteArray::from(PROBE_QML));
    let url = format!(
        "file://{}",
        repo_root()
            .join("qml/components/EntryListView.qml")
            .display()
    );

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
    macro_rules! get {
        ($name:expr, $nth:expr, $property:expr) => {
            call!("get", QString::from($name), $nth, QString::from($property))
        };
    }
    // Qt's Text.TextFormat.
    const PLAIN: &str = "0";
    const STYLED: &str = "4";

    // ---------------------------------------------------------- searching
    assert_eq!(call!("load", QString::from(url.as_str()), true), "ok");
    assert_eq!(
        get!("pulley", 0, "visible"),
        "false",
        "the search page has no pulley: nothing on it was about a search"
    );
    assert_eq!(get!("sectionHeader", 0, "text"), "In titles (4)");
    assert_eq!(
        get!("sectionHeader", 1, "text"),
        "In article text (7)",
        "one header per group, counting the whole of it"
    );

    assert_eq!(get!("titleLabel", 0, "textFormat"), STYLED);
    assert_eq!(
        get!("titleLabel", 0, "text"),
        "<b>Harbour</b> &lt;at&gt; dusk",
        "a result's title is the one Rust escaped and marked"
    );
    assert_eq!(get!("detailLabel", 0, "textFormat"), STYLED);
    assert_eq!(
        get!("detailLabel", 0, "text"),
        "<b>Harbour</b> Gazette  \u{b7}  3 min read",
        "and so is its feed's name, with the rest of the line beside it"
    );
    assert_eq!(
        get!("excerptLabel", 0, "visible"),
        "false",
        "a title hit has no excerpt"
    );
    assert_eq!(get!("excerptLabel", 1, "visible"), "true");
    assert_eq!(get!("excerptLabel", 1, "textFormat"), STYLED);
    assert_eq!(
        get!("excerptLabel", 1, "text"),
        "A walk by the <b>harbour</b>."
    );

    // -------------------------------------------------------- not searching
    assert_eq!(call!("load", QString::from(url.as_str()), false), "ok");
    assert_eq!(get!("pulley", 0, "visible"), "true");
    assert!(
        get!("sectionHeader", 0, "text").starts_with("missing:"),
        "a list that is not a search is not grouped"
    );
    assert_eq!(
        get!("titleLabel", 0, "textFormat"),
        PLAIN,
        "§9.3: outside a search, a feed's title is foreign text and drawn plain"
    );
    assert_eq!(get!("titleLabel", 0, "text"), "Harbour <at> dusk");
    assert_eq!(get!("detailLabel", 0, "textFormat"), PLAIN);
    assert_eq!(
        get!("detailLabel", 0, "text"),
        "Harbour Gazette  \u{b7}  3 min read"
    );
    assert_eq!(get!("excerptLabel", 1, "visible"), "false");
}
