//! The cover: what it says, and how it says it.
//!
//! The QML load test compiles and instantiates this file with every property
//! at its default. What it cannot see is what the cover does as the count
//! and sync move underneath it.
//!
//! The count is NEGATIVE SPACE in the texture: the lines flow around the
//! digits, so every count has a mask of its own in `qml/art/cover/` and the
//! cover's one job is to name the right one. `qml_loads.rs` checks the masks
//! the QML names literally; the cover's is computed, so the set is checked
//! here instead, and so is the naming.
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
        function narrow(width) { loader.item.width = width; return 'ok' }
        function fits() {
            var row = findIn(loader.item, 'syncStatus')
            if (!row) { return 'missing:syncStatus' }
            return row.width <= loader.item.width ? 'fits' : 'overflows'
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

fn cover_url() -> String {
    format!(
        "file://{}",
        repo_root().join("qml/cover/CoverPage.qml").display()
    )
}

fn stubs_dir() -> std::path::PathBuf {
    repo_root().join("qml-stubs")
}

/// Every mask the cover can name: the counts, and the cap.
fn cover_keys() -> Vec<String> {
    let mut keys: Vec<String> = (0..=99).map(|n| n.to_string()).collect();
    keys.push("99+".to_owned());
    keys
}

/// One test, because `QmlEngine::new()` builds a `QApplication` and there may
/// be only one of those per process.
#[test]
fn the_cover_names_the_mask_for_its_count_and_says_how_sync_is() {
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
    macro_rules! mask {
        ($key:expr) => {{
            let source = get!("textArt", "source");
            let expected = format!("art/cover/{}.png", $key);
            assert!(
                source.ends_with(&expected),
                "the cover must draw the mask painted around its count: \
                 expected a source ending in {expected:?}, got {source:?}"
            );
        }};
    }

    assert_eq!(
        call!("load", QString::from(cover_url())),
        "ok",
        "the cover did not load"
    );

    // ------------------------------------------------------------ the count
    // From the start: a zero says as much as a count, and has a mask of its
    // own like any other.
    mask!("0");
    assert_eq!(
        get!("unreadTotal", "text"),
        "0",
        "the count must be there as data from the start"
    );
    assert_eq!(call!("count", 4), "ok");
    mask!("4");
    assert_eq!(get!("unreadTotal", "text"), "4");
    assert_eq!(call!("count", 99), "ok");
    mask!("99");
    assert_eq!(get!("unreadTotal", "text"), "99");

    // Past the cap every count shares one mask, and the data says the cap.
    assert_eq!(call!("count", 100), "ok");
    mask!("99+");
    assert_eq!(get!("unreadTotal", "text"), "99+");
    assert_eq!(call!("count", 1234), "ok");
    mask!("99+");
    assert_eq!(get!("unreadTotal", "text"), "99+");

    // A count below zero cannot happen; if it did, it would not name a mask
    // that is not there.
    assert_eq!(call!("count", -3), "ok");
    mask!("0");
    assert_eq!(call!("count", 4), "ok");
    mask!("4");

    // ---------------------------------------------------------- the texture
    // The whole cover, with nothing cut out of it: the room for the number
    // is in the mask, and a fade band would take a strip off the top of a
    // pattern that is the cover's whole face.
    let fade_from = number!(get!("textArt", "fadeFrom"));
    let fade_to = number!(get!("textArt", "fadeTo"));
    assert!(
        fade_to <= fade_from,
        "the cover's texture must not fade in over a band: {fade_from} to {fade_to}"
    );
    assert_eq!(
        number!(get!("textArt", "clearRadius")),
        0.0,
        "the cover's texture must not have a disc cleared out of it"
    );

    // ------------------------------------------------------- what sync says
    // The room for the status line is made by the TEXTURE, which runs out
    // towards the foot of the cover while there is something to read there
    // and is whole again afterwards. Nothing is laid over the pattern, so
    // what this checks is the mask's own band, not a layer.
    macro_rules! sinks {
        ($expected:expr, $why:expr) => {{
            let from = number!(get!("textArt", "fadeOutFrom"));
            let to = number!(get!("textArt", "fadeOutTo"));
            assert_eq!(to > from, $expected, "{}: {from} to {to}", $why);
        }};
    }

    assert_eq!(
        get!("syncStatus", "visible"),
        "false",
        "nothing to say while sync is idle and well"
    );
    sinks!(
        false,
        "the texture must be whole while there is no status to make room for"
    );
    assert_eq!(get!("syncStatusLabel", "text"), "");
    assert_eq!(call!("syncing", true), "ok");
    assert_eq!(get!("syncStatus", "visible"), "true");
    sinks!(
        true,
        "the texture must give the status line room while it is shown"
    );
    assert_eq!(get!("syncStatusLabel", "text"), "Refreshing");

    // The line never runs off the cover's edges.
    //
    // A `Row` centred on the cover hands a `Label` whatever width it asks
    // for, and a `Label` asks for its whole string on one line; the German
    // for "Refresh failed" is half again as wide as the cover, so the row
    // grew past both edges and the phrase was clipped at BOTH ends. The
    // catalogue that broke it is not the one this test can load, so the
    // cover is squeezed instead until English cannot fit either: the rule
    // is geometric, and so is the check.
    assert_eq!(
        call!("fits"),
        "fits",
        "the status row must fit at full width"
    );
    assert_eq!(call!("narrow", 120), "ok");
    assert_eq!(
        call!("fits"),
        "fits",
        "the status row must stay inside a cover too narrow for its line"
    );
    assert!(
        number!(get!("syncStatusLabel", "lineCount")) > 1.0,
        "a line that cannot fit must wrap rather than overflow"
    );
    assert_eq!(call!("narrow", 240), "ok");

    assert_eq!(
        get!("unreadTotal", "text"),
        "4",
        "the count must survive a refresh; it is the one thing the cover is for"
    );
    mask!("4");
    assert_eq!(call!("syncing", false), "ok");
    assert_eq!(get!("syncStatus", "visible"), "false");
    sinks!(
        false,
        "the texture must come back when the status it made room for goes"
    );
    assert_eq!(get!("syncStatusLabel", "text"), "");

    // §9.3: the server's own words never reach the cover.
    assert_eq!(
        call!(
            "failure",
            QString::from("<b>500</b> from feeds.example"),
            false
        ),
        "ok"
    );
    assert_eq!(get!("syncStatus", "visible"), "true");
    sinks!(true, "a warning needs the same room as a spinner");
    assert_eq!(
        get!("syncStatusLabel", "text"),
        "Refresh failed",
        "the cover must say its own fixed line, never the server's text"
    );
    assert_eq!(
        call!("failure", QString::from(""), true),
        "ok",
        "a rejected key is reported without any server text"
    );
    assert_eq!(get!("syncStatusLabel", "text"), "Sign-in failed");
    mask!("4");
}

/// Every mask the cover can name is there, and is a coverage mask.
///
/// The cover computes its `source`, so `qml_loads.rs`, which reads literal
/// sources out of the QML, cannot see these. A missing one would be a bare
/// cover for exactly that count -- Qt reports a missing image only as a
/// warning -- and a mask re-exported as RGBA would tint the cover solid,
/// for the reason given over there.
#[test]
fn every_mask_the_cover_can_name_exists_and_is_a_mask() {
    let dir = repo_root().join("qml/art/cover");
    let mut problems: Vec<String> = Vec::new();
    for key in cover_keys() {
        let path = dir.join(format!("{key}.png"));
        let Ok(bytes) = std::fs::read(&path) else {
            problems.push(format!("{key}.png is missing"));
            continue;
        };
        if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
            problems.push(format!("{key}.png is not a PNG"));
            continue;
        }
        // IHDR is always first: 8 bytes of signature, 8 of chunk header,
        // then width, height, bit depth, colour type.
        let depth = bytes.get(24).copied().unwrap_or(0);
        let colour = bytes.get(25).copied().unwrap_or(255);
        if (depth, colour) != (8, 0) {
            problems.push(format!(
                "{key}.png is bit depth {depth} colour type {colour}; the shader \
                 reads coverage from the red channel of an 8-bit grayscale mask"
            ));
        }
    }

    // And nothing else: a mask for a count the cover cannot name is dead
    // weight in every package.
    let expected: std::collections::HashSet<String> = cover_keys()
        .into_iter()
        .map(|k| format!("{k}.png"))
        .collect();
    for entry in std::fs::read_dir(&dir)
        .expect("qml/art/cover exists")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !expected.contains(&name) {
            problems.push(format!("{name} is in qml/art/cover/ but no count names it"));
        }
    }

    assert!(
        problems.is_empty(),
        "the cover's masks are not what `make textart` makes:\n{}",
        problems.join("\n")
    );
}
