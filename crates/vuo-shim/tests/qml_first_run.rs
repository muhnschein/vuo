//! First run: welcome page, then setup, then list; built from URL.
//!
//! Root window builds both first-run pages only when no account stored:
//! named inline, they (and Silica text fields) compiled every start. Here
//! real root window starts with no account; welcome page is initial page,
//! Continue replaces it with setup, accepting setup lands on entry list.
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
        Loader { id: loader }
        // Silica's pageStack: records replace, hands back setup page it made.
        Item {
            id: stack
            property string replaced: ""
            property var page: null
            function replace(url, props) {
                var made = Qt.createComponent(url).createObject(stack, props)
                stack.replaced = ('' + url).split('/').pop()
                stack.page = made
                return made
            }
        }
        function load(url) {
            loader.setSource(url)
            if (loader.status !== Loader.Ready) { return 'load-failed' }
            loader.item.pageStack = stack
            var first = loader.item.initialPage
            if (!first || first.continued === undefined) { return 'not-welcome:' + first }
            return 'welcome'
        }
        // "page replaced to|accept goes to entry list".
        function proceed() {
            loader.item.initialPage.continued()
            var setup = stack.page
            if (!setup) { return stack.replaced + '|no-page' }
            var dest = setup.acceptDestination
            var list = dest && typeof dest.createObject === 'function'
                       ? ('' + dest.createObject(stack)).split('_')[0] : '' + dest
            return stack.replaced + '|' + list + '|' + (setup.configured !== undefined)
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
fn a_new_install_goes_welcome_setup_list() {
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
        repo_root().join("qml/harbour-vuo.qml").display()
    );
    assert_eq!(
        call!("load", QString::from(url)),
        "welcome",
        "no account stored: window opens on welcome page"
    );
    assert_eq!(
        call!("proceed"),
        "SetupDialog.qml|EntryListPage|true",
        "Continue replaces welcome with setup; accepting setup opens entry list"
    );
}
