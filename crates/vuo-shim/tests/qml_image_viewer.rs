//! Image viewer: where zoom leaves image.
//!
//! Load test instantiates page with defaults; can't see zoom geometry. Zoom not
//! keeping tapped spot under finger = "jolt" (image jumps on double tap). Loads
//! page at phone size, gives image shape, zooms, reads back what's under same
//! screen point.
//!
//! Run under `QT_QPA_PLATFORM=offscreen`; `make check` sets it.

// Test code: panic denials guard production foreign-input paths, not test
// assertions. `borrow_as_ptr` = Qt harness's engine pointer.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::borrow_as_ptr
)]

use qmetaobject::{QByteArray, QMetaType, QString, QVariant, QmlEngine};

const PROBE_QML: &str = r"
    import QtQuick 2.0
    Item {
        Loader { id: loader }
        function load(url) {
            loader.setSource(url, { width: 540, height: 960 })
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
            var item = name === 'page' ? loader.item : findIn(loader.item, name)
            if (!item) { return 'missing:' + name }
            return '' + item[property]
        }
        function shape(width, height) {
            loader.item.naturalWidth = width
            loader.item.naturalHeight = height
            return 'ok'
        }
        function zoomAt(target, x, y) { loader.item.zoomAt(target, x, y); return 'ok' }
        function toggle(x, y) { loader.item.toggleZoom(x, y); return 'ok' }
        // Where in image (0..1 per axis) screen point (x, y) is: read from
        // frame's actual position, not page's own arithmetic.
        function under(x, y) {
            var flick = findIn(loader.item, 'imageFlick')
            var frame = findIn(loader.item, 'imageFrame')
            if (!flick || !frame) { return 'missing' }
            var p = frame.mapFromItem(flick, x, y)
            return (p.x / frame.width) + ' ' + (p.y / frame.height)
        }
    }
";

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repository root")
        .to_path_buf()
}

fn viewer_url() -> String {
    format!(
        "file://{}",
        repo_root().join("qml/pages/ImageViewerPage.qml").display()
    )
}

/// One test: `QmlEngine::new()` builds `QApplication`; one per process.
#[test]
fn a_zoom_keeps_the_spot_it_zooms_into_where_it_was() {
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        repo_root().join("qml-stubs").to_string_lossy().into_owned(),
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
    macro_rules! under {
        ($x:expr, $y:expr) => {{
            let text = call!("under", f64::from($x), f64::from($y));
            let mut parts = text.split(' ').map(|part| number!(part));
            match (parts.next(), parts.next()) {
                (Some(across), Some(down)) => (across, down),
                _ => panic!("not a point: {text:?}"),
            }
        }};
    }
    macro_rules! assert_near {
        ($left:expr, $right:expr, $what:expr) => {{
            let (left, right) = ($left, $right);
            assert!(
                (left.0 - right.0).abs() < 1e-6 && (left.1 - right.1).abs() < 1e-6,
                "{}: {left:?} then {right:?}",
                $what
            );
        }};
    }

    assert_eq!(call!("load", QString::from(viewer_url())), "ok");

    // Non-interactive Flickable doesn't pass pinch's second finger to inner
    // PinchArea: viewer panning only when zoomed could never be pinched at all.
    assert_eq!(
        get!("imageFlick", "interactive"),
        "true",
        "the viewer must take a pinch at the fitted size"
    );

    // Square image on 540x960 portrait page: fitted 540x540, centred, 210 above
    // and below.
    assert_eq!(call!("shape", 1000.0, 1000.0), "ok");
    assert_eq!(number!(get!("imageFrame", "width")), 540.0);
    assert_eq!(number!(get!("imageFrame", "y")), 210.0);
    assert_near!(under!(270, 480), (0.5, 0.5), "fitted, the image is centred");

    // Double tap below-right of centre: in, tapped spot still under finger.
    // Forgetting the 210 centring offset moves spot by exactly that (the jump
    // reader sees).
    let before = under!(400, 600);
    assert_eq!(call!("toggle", 400.0, 600.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 3.0);
    assert_near!(before, under!(400, 600), "a double tap in");

    // Pinch from there about another spot keeps that one.
    let before = under!(150, 700);
    assert_eq!(call!("zoomAt", 4.0, 150.0, 700.0), "ok");
    assert_near!(before, under!(150, 700), "a pinch while zoomed in");

    // Out again: whole image, centred, whatever was panned to.
    assert_eq!(call!("toggle", 150.0, 700.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 1.0);
    assert_near!(
        under!(270, 480),
        (0.5, 0.5),
        "back out, the image is centred"
    );

    // Pinch stops at limit either way.
    assert_eq!(call!("zoomAt", 50.0, 270.0, 480.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 5.0);
    assert_eq!(call!("zoomAt", 0.2, 270.0, 480.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 1.0);

    // Panorama: fitted to width, 540x135, zoomed about spot. Sideways spot
    // stays; image still shorter than screen vertically, stays centred, doesn't
    // follow.
    assert_eq!(call!("shape", 4000.0, 1000.0), "ok");
    assert_eq!(number!(get!("imageFrame", "height")), 135.0);
    let before = under!(100, 480);
    assert_eq!(call!("zoomAt", 2.0, 100.0, 480.0), "ok");
    let after = under!(100, 480);
    assert!(
        (before.0 - after.0).abs() < 1e-6,
        "a zoom into a panorama keeps the spot across: {before:?} then {after:?}"
    );
    assert_eq!(
        number!(get!("imageFrame", "y")),
        (960.0 - 270.0) / 2.0,
        "an image shorter than the screen stays centred"
    );
}
