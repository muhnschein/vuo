//! The image viewer: where a zoom leaves the image.
//!
//! The QML load test instantiates the page with every property at its
//! default. What it cannot see is the geometry of a zoom -- and a zoom that
//! does not keep the tapped spot under the finger is the "jolt" a reader
//! sees as the image jumping somewhere else on a double tap. So this loads
//! the page at a phone's size, gives it an image's shape, zooms, and reads
//! back what is under the same point of the screen.
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
        // Where in the image, 0..1 on each axis, the point (x, y) of the
        // screen is -- read from where the frame actually is, not from the
        // page's own arithmetic.
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

/// One test, because `QmlEngine::new()` builds a `QApplication` and there may
/// be only one of those per process.
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

    // A Flickable that is not interactive does not hand the second finger
    // of a pinch on to the PinchArea inside it, so a viewer that only pans
    // once zoomed in can never be pinched in the first place.
    assert_eq!(
        get!("imageFlick", "interactive"),
        "true",
        "the viewer must take a pinch at the fitted size"
    );

    // A square image on a 540x960 portrait page: fitted at 540x540, and
    // centred, with 210 above and below it.
    assert_eq!(call!("shape", 1000.0, 1000.0), "ok");
    assert_eq!(number!(get!("imageFrame", "width")), 540.0);
    assert_eq!(number!(get!("imageFrame", "y")), 210.0);
    assert_near!(under!(270, 480), (0.5, 0.5), "fitted, the image is centred");

    // A double tap below and right of the centre: in, with the tapped spot
    // still under the finger. Forgetting the 210 the image was centred by
    // moves the spot by exactly that, which is the jump a reader sees.
    let before = under!(400, 600);
    assert_eq!(call!("toggle", 400.0, 600.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 3.0);
    assert_near!(before, under!(400, 600), "a double tap in");

    // And a pinch on from there, about another spot, keeps that one.
    let before = under!(150, 700);
    assert_eq!(call!("zoomAt", 4.0, 150.0, 700.0), "ok");
    assert_near!(before, under!(150, 700), "a pinch while zoomed in");

    // Out again: the whole image, centred, whatever was panned to.
    assert_eq!(call!("toggle", 150.0, 700.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 1.0);
    assert_near!(
        under!(270, 480),
        (0.5, 0.5),
        "back out, the image is centred"
    );

    // A pinch goes no further than the limit either way.
    assert_eq!(call!("zoomAt", 50.0, 270.0, 480.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 5.0);
    assert_eq!(call!("zoomAt", 0.2, 270.0, 480.0), "ok");
    assert_eq!(number!(get!("page", "zoom")), 1.0);

    // A panorama: fitted to the width, 540x135, and zoomed in about a spot
    // on it. Sideways the spot stays put; up and down the image is still
    // shorter than the screen, so it stays centred rather than following.
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
