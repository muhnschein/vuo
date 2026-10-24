//! Search field keeps keyboard while results change under it.
//!
//! `ListView` makes current item the focused one inside it, and gives itself
//! current item when rows arrive and it has none; search field is in same
//! view's header, so new results would take focus (keyboard closes on typing
//! pause).
//!
//! Model RESET does same, hence search model never resets
//! (`models::Publish::Search`, tested there). New results reach view as here:
//! rows out, rows in.
//!
//! Focus real only in window: unlike other QML tests, shows list in
//! `QQuickView` (offscreen). View root registers with JS library engine's root
//! (reached by `invoke_method`) forwards to.
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

use qmetaobject::{QMetaType, QQuickView, QString, QVariant};

const PROBE_JS: &str = ".pragma library\nvar target = null\n";

const VIEW_QML: &str = r#"
    import QtQuick 2.0
    import "probe.js" as Probe
    Item {
        id: root
        width: 540
        height: 960
        // EntryModel searching, as far as list reads one.
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
            property var searches: []
            function loadMoreIn(kind) {}
            function setSearch(text) { searches.push(text) }
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
        // Results arriving like search's: rows into empty list.
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
        // What Return and Enter do, short of key event (can't synthesise here):
        // separate test checks both keys call it.
        function submit(text) {
            var field = findIn(loader.item, 'searchField')
            field.text = text
            fake.searches = []
            field.submit()
            return fake.searches.join('/') + ' ' + field.activeFocus
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
        function submit(text) { return Probe.target.submit(text) }
    }
"#;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repository root")
        .to_path_buf()
}

/// One test: `QQuickView` builds process's one application.
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
    // Again, as each typing pause brings new set.
    assert_eq!(call!("results"), "ok");
    assert_eq!(call!("fieldHasFocus"), "true");

    // Return = "done typing": search goes out now for field text; keyboard goes
    // away so results get screen.
    assert_eq!(
        call!("submit", QString::from("harbour")),
        "harbour false",
        "submitting must search for the field's text at once, and only once, \
         and drop the focus so the keyboard closes"
    );
}

/// Both keyboard "done" keys submit: Return from virtual keyboard, Enter from
/// hardware keypad. Read from source (QML probe can't press key); `submit`
/// checked above.
#[test]
fn return_and_enter_submit_the_search() {
    let source =
        std::fs::read_to_string(repo_root().join("qml/components/EntryListView.qml")).unwrap();
    let field = source
        .split_once("SearchField {")
        .map(|(_, rest)| rest)
        .expect("EntryListView has a SearchField");
    for key in ["Keys.onReturnPressed", "Keys.onEnterPressed"] {
        assert!(
            field.contains(&format!("{key}: searchField.submit()")),
            "the search field's {key} must call submit()"
        );
    }
}
