//! The cover: what it says, and where its texture stops.
//!
//! The QML load test compiles and instantiates this file with every property
//! at its default, which shows the heading. What it cannot see is what the
//! heading says as sync and the count move underneath it, and it does not
//! look at the texture's geometry at all.
//!
//! The texture itself is no longer computed here -- it is a mask painted
//! ahead of time by `tools/textart/` and shipped in `qml/art/`, and
//! `qml_loads.rs` checks that the masks are there and are masks. What is
//! left for this test is the one thing about the texture that is still the
//! cover's own decision: WHERE IT FADES IN. It is drawn under the whole
//! cover, heading included, and only the fade keeps the app's name off a
//! field of text.
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

use qmetaobject::*;

/// Loads the cover at a size, since the stub `CoverBackground` has none of
/// its own, and reads it back.
const PROBE_QML: &str = r"
    import QtQuick 2.0
    Item {
        Loader { id: loader }
        function load(url) {
            loader.setSource(url, { width: 240, height: 360 })
            return loader.status === Loader.Ready ? 'ok' : 'load-failed'
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
        function get(name, property) {
            var item = findIn(loader.item, name)
            if (!item) { return 'missing:' + name }
            return '' + item[property]
        }
        function count(total) { loader.item.unreadCount = total; return 'ok' }
        function syncing(on) { loader.item.syncing = on; return 'ok' }
        function failure(text, auth) {
            loader.item.syncErrorIsAuth = auth
            loader.item.syncError = text
            return 'ok'
        }
        // Where the texture starts, against where the heading ends. Both in
        // the coordinates the cover lays them out in.
        function headingBottom() {
            var item = findIn(loader.item, 'heading')
            return item ? '' + (item.y + item.height) : 'missing:heading'
        }
    }
";

fn cover_url() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .to_path_buf();
    format!("file://{}", root.join("qml/cover/CoverPage.qml").display())
}

fn stubs_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .join("qml-stubs")
}

/// One test, because `QmlEngine::new()` builds a `QApplication` and there may
/// be only one of those per process.
#[test]
fn the_cover_says_the_count_over_a_texture_that_starts_below_it() {
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(stubs_dir().to_string_lossy().into_owned()));
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
    macro_rules! get {
        ($name:expr, $property:expr) => {
            call!("get", QString::from($name), QString::from($property))
        };
    }
    macro_rules! number {
        ($text:expr) => {{
            let text = $text;
            text.parse::<f64>()
                .unwrap_or_else(|_| panic!("not a number: {text:?}"))
        }};
    }

    assert_eq!(
        call!("load", QString::from(cover_url())),
        "ok",
        "the cover did not load"
    );

    // ---------------------------------------------------------- the heading
    assert_eq!(
        get!("brand", "text"),
        "Vuo",
        "the cover does not name the app in its corner"
    );
    assert_eq!(get!("subtitle", "text"), "Unread");
    assert_eq!(
        get!("unreadTotal", "text"),
        "0",
        "the count must be there from the start; a zero says as much as a count"
    );
    assert_eq!(call!("count", 4), "ok");
    assert_eq!(get!("unreadTotal", "text"), "4");

    // ---------------------------------------------------------- the texture
    let source = get!("textArt", "source");
    assert!(
        source.ends_with("art/cover.png"),
        "the cover must draw the cover's own mask -- the page's is painted at \
         a density that is mush at this size -- got {source:?}"
    );
    // It is drawn under the WHOLE cover, so the fade is the only thing
    // keeping the app's name off a field of text.
    let fade_from = number!(get!("textArt", "fadeFrom"));
    let fade_to = number!(get!("textArt", "fadeTo"));
    let heading_bottom = number!(call!("headingBottom"));
    assert!(
        fade_from >= heading_bottom,
        "the texture reaches full strength at {fade_from}, above where the \
         heading ends at {heading_bottom}: the app's name would sit in text"
    );
    assert!(
        fade_to > fade_from,
        "the texture must fade in over a band rather than start on a hard \
         line: {fade_from} to {fade_to}"
    );

    // ------------------------------------------------------- what sync says
    assert_eq!(call!("syncing", true), "ok");
    assert_eq!(get!("subtitle", "text"), "Refreshing");
    assert_eq!(
        get!("unreadTotal", "text"),
        "4",
        "the count must survive a refresh; it is the one thing the cover is for"
    );
    assert_eq!(call!("syncing", false), "ok");
    assert_eq!(get!("subtitle", "text"), "Unread");

    // §9.3: the server's own words never reach the cover.
    assert_eq!(
        call!(
            "failure",
            QString::from("<b>500</b> from feeds.example"),
            false
        ),
        "ok"
    );
    assert_eq!(
        get!("subtitle", "text"),
        "Refresh failed",
        "the cover must say its own fixed line, never the server's text"
    );
    assert_eq!(
        call!("failure", QString::from(""), true),
        "ok",
        "a rejected key is reported without any server text"
    );
    assert_eq!(get!("subtitle", "text"), "Sign-in failed");

    // A number too wide for the corner is capped rather than pushed into the
    // app's name.
    assert_eq!(call!("count", 1234), "ok");
    assert_eq!(get!("unreadTotal", "text"), "999+");
}
