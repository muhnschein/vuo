//! The search field keeps the keyboard while the results change under it.
//!
//! Reported from a device: pausing while typing a search -- long enough for
//! the results to load -- closed the keyboard. A ListView makes its current
//! item the focused one inside it, and gives itself a current item whenever
//! rows arrive and it has none; the search field is in the same view's
//! header, so every new set of results took the focus off it.
//!
//! A model RESET does the same, which is why a search model never resets --
//! see `models::Publish::Search`, and its test there. New results reach this
//! view the way they do here: rows out, rows in.
//!
//! Focus is only real in a window, so unlike the other QML tests this one
//! shows the list in a `QQuickView` (offscreen). The view's root registers
//! itself with a JavaScript library that the engine's own root -- the one
//! `invoke_method` reaches -- forwards to.
//!
//! Run under `QT_QPA_PLATFORM=offscreen`; `make check` sets it.

// Test code: the panic denials guard foreign-input paths in production, not
// assertions in tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use qmetaobject::{QMetaType, QQuickView, QString, QVariant};

const PROBE_JS: &str = ".pragma library\nvar target = null\n";

const VIEW_QML: &str = r#"
    import QtQuick 2.0
    import "probe.js" as Probe
    Item {
        id: root
        width: 540
        height: 960
        // An EntryModel searching, as far as the list reads one.
        ListModel {
            id: fake
            property bool ready: true
            property bool syncing: false
            property int titleMatches: 0
            property int feedMatches: 0
            property int textMatches: 0
            property int titleShown: 0
            property int feedShown: 0
            property int textShown: 0
            property int titleNextLoad: 10
            property int feedNextLoad: 10
            property int textNextLoad: 10
            function loadMoreIn(kind) {}
            function setSearch(text) {}
            function setScope(kind, id) {}
            function requestSync() {}
        }
        Loader { id: loader; anchors.fill: parent; focus: true }
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
        function load(url, searching) {
            fake.clear()
            loader.setSource(url, { searching: searching, entryModel: fake })
            return loader.status === Loader.Ready ? 'ok' : 'load-failed'
        }
        function type() {
            var field = findIn(loader.item, 'searchField')
            if (!field) { return 'missing' }
            field.forceActiveFocus()
            return '' + field.activeFocus
        }
        // Results arriving, as a search's do: rows into an empty list.
        function results() {
            fake.clear()
            for (var i = 0; i < 3; i++) {
                fake.append({ entryId: i + 1, feedId: 1, author: "", unread: true,
                              starred: false, published: 0, readingTime: 0, url: "",
                              feedIcon: "", title: "t", titleStyled: "t", feedName: "f",
                              feedNameStyled: "f", matchKind: "title", excerpt: "" })
            }
            loader.item.forceLayout()
            return 'ok'
        }
        function fieldHasFocus() {
            return '' + findIn(loader.item, 'searchField').activeFocus
        }
        Component.onCompleted: Probe.target = root
    }
"#;

const ROOT_QML: &str = r#"
    import QtQuick 2.0
    import "probe.js" as Probe
    QtObject {
        function load(url, searching) { return Probe.target.load(url, searching) }
        function type() { return Probe.target.type() }
        function results() { return Probe.target.results() }
        function fieldHasFocus() { return Probe.target.fieldHasFocus() }
    }
"#;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repository root")
        .to_path_buf()
}

/// One test, because a `QQuickView` builds the process's one application.
#[test]
fn new_results_leave_the_keyboard_with_the_search_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("probe.js"), PROBE_JS).unwrap();
    std::fs::write(dir.path().join("View.qml"), VIEW_QML).unwrap();
    std::fs::write(dir.path().join("Root.qml"), ROOT_QML).unwrap();

    let mut view = QQuickView::new();
    view.engine().add_import_path(QString::from(
        repo_root().join("qml-stubs").to_string_lossy().into_owned(),
    ));
    view.set_source(QString::from(format!(
        "file://{}",
        dir.path().join("View.qml").display()
    )));
    view.show();
    view.engine().load_file(QString::from(
        dir.path().join("Root.qml").to_string_lossy().into_owned(),
    ));

    let url = format!(
        "file://{}",
        repo_root()
            .join("qml/components/EntryListView.qml")
            .display()
    );
    macro_rules! call {
        ($name:expr $(, $arg:expr)*) => {{
            let result = view.engine().invoke_method(
                $name.into(),
                &[$(QVariant::from($arg)),*],
            );
            QString::from_qvariant(result)
                .map(|value| value.to_string())
                .unwrap_or_default()
        }};
    }

    assert_eq!(call!("load", QString::from(url.as_str()), true), "ok");
    assert_eq!(
        call!("type"),
        "true",
        "the field must take the focus to begin with"
    );
    assert_eq!(call!("results"), "ok");
    assert_eq!(
        call!("fieldHasFocus"),
        "true",
        "results arriving must not take the focus -- and the keyboard -- off \
         the search field"
    );
    // And again, as each pause in typing brings a new set.
    assert_eq!(call!("results"), "ok");
    assert_eq!(call!("fieldHasFocus"), "true");
}
