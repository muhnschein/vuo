//! The cover: the heading, and the texture under it.
//!
//! The QML load test compiles and instantiates this file with every property
//! at its default and can see the heading. What it cannot see is the texture:
//! lines of filler text set along curves that are traced by a pass of
//! JavaScript over the cover's size. The painting itself needs a window and
//! a font, which a headless engine has neither of, so the pass is split in
//! two -- `layout` returns the curves, and `onPaint` sets text along them --
//! and this reads the curves back.
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

/// Loads the cover at a size, since the stub `CoverBackground` has none of its
/// own, and reads it back.
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
        // The curves the texture is set along, traced once and summarised:
        // how many, how many close on themselves, how many points in all,
        // and whether any point lies off the cover by more than a line.
        property var curves: null
        function trace() {
            var art = findIn(loader.item, 'textArt')
            if (!art) { return 'missing:textArt' }
            curves = art.layout()
            return '' + curves.length
        }
        function closed() {
            var n = 0
            for (var i = 0; i < curves.length; i++) { if (curves[i].closed) { n++ } }
            return '' + n
        }
        function points() {
            var n = 0
            for (var i = 0; i < curves.length; i++) { n += curves[i].points.length }
            return '' + n
        }
        function stray() {
            var art = findIn(loader.item, 'textArt')
            var margin = art.spacing * 2
            for (var i = 0; i < curves.length; i++) {
                var pts = curves[i].points
                for (var j = 0; j < pts.length; j++) {
                    if (pts[j].x < -margin || pts[j].x > art.width + margin
                            || pts[j].y < -margin || pts[j].y > art.height + margin) {
                        return 'yes:' + pts[j].x + ',' + pts[j].y
                    }
                }
            }
            return 'no'
        }
        // The greatest gap between two consecutive points of any curve: a
        // curve that jumped would set text across the gap.
        function longestStep() {
            var worst = 0
            for (var i = 0; i < curves.length; i++) {
                var pts = curves[i].points
                for (var j = 1; j < pts.length; j++) {
                    var dx = pts[j].x - pts[j-1].x, dy = pts[j].y - pts[j-1].y
                    worst = Math.max(worst, Math.sqrt(dx * dx + dy * dy))
                }
            }
            return '' + worst
        }
        function filler() { return findIn(loader.item, 'textArt').filler }
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
#[allow(clippy::too_many_lines)]
fn the_cover_says_the_count_over_a_texture_of_text_along_curves() {
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
    let curves: usize = call!("trace").parse().unwrap_or(0);
    assert!(
        curves >= 12,
        "the cover's size gives only {curves} curves; the texture would be bare"
    );
    let closed: usize = call!("closed").parse().unwrap_or(0);
    assert!(
        closed >= 3,
        "the innermost curves close around their strokes -- the 'eyes' of \
         the pattern -- and only {closed} of {curves} did"
    );
    assert!(
        closed < curves,
        "every curve closed, so none of them reach the cover's edge and the \
         corners are bare"
    );
    let points: usize = call!("points").parse().unwrap_or(0);
    assert!(
        points >= 2000,
        "{points} points across {curves} curves is not enough line to set \
         text along"
    );
    assert_eq!(
        call!("stray"),
        "no",
        "a curve ran off the cover: text would be set where nobody can see it"
    );
    let step = number!(call!("longestStep"));
    assert!(
        step <= 4.0,
        "a curve jumped {step} pixels between points; the text set across \
         that gap would not follow it"
    );
    // The filler is the cover's own fixed text. Nothing foreign is anywhere
    // near the canvas, and this is the assertion that keeps it so.
    let filler = call!("filler");
    assert!(
        filler.starts_with("Lorem ipsum") && !filler.contains('<'),
        "the texture's text must be the fixed filler, got {filler:?}"
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
