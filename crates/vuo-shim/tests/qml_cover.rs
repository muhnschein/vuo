//! Cover: what it says, how.
//!
//! QML load test instantiates file with defaults; can't see cover behaviour as count
//! and sync move.
//!
//! Count = NEGATIVE SPACE in texture: lines flow around digits; every count has own
//! mask in `qml/art/cover/`; cover's one job is naming right one. Nothing unread = no
//! number: `none.png`, pattern alone, no outline. `qml_loads.rs` checks masks QML names
//! literally; cover's name is computed, so set and naming checked here.
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

/// Loads cover at a size (stub `CoverBackground` has none), reads it back.
const PROBE_QML: &str = r"
    import QtQuick 2.0
    Item {
        Loader { id: loader }
        property int refreshes: 0
        function load(url) {
            loader.setSource(url, { width: 240, height: 360 })
            if (loader.status !== Loader.Ready) { return 'load-failed' }
            loader.item.refresh.connect(function() { refreshes++ })
            return 'ok'
        }
        /// Tap cover action, report what cover asked app for.
        function tap(name) {
            var action = findIn(loader.item, name)
            if (!action) { return 'missing:' + name }
            action.triggered()
            return 'refresh ' + refreshes
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

/// Mask cover names when nothing unread: no digits, so no outline beside it.
const NONE: &str = "none";

/// Every mask cover can name: counts, cap, nothing unread.
fn cover_keys() -> Vec<String> {
    let mut keys: Vec<String> = (1..=99).map(|n| n.to_string()).collect();
    keys.push("99+".to_owned());
    keys.push(NONE.to_owned());
    keys
}

/// One test: `QmlEngine::new()` builds `QApplication`; one per process.
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
            // Outline, from same count. Separate bindings over separate files:
            // one could follow count while other doesn't, drawing this count's
            // pattern with another count's line (looks like painter bug, is
            // binding bug).
            let edge = get!("textArt", "edgeSource");
            if $key == NONE {
                // No number, no outline, no file to name.
                assert_eq!(
                    edge, "",
                    "with nothing unread the cover must draw no outline"
                );
            } else {
                let expected_edge = format!("art/cover/{}-edge.png", $key);
                assert!(
                    edge.ends_with(&expected_edge),
                    "the cover's outline must come from the same count as its \
                     pattern: expected an edgeSource ending in {expected_edge:?}, \
                     got {edge:?}"
                );
            }
        }};
    }

    assert_eq!(
        call!("load", QString::from(cover_url())),
        "ok",
        "the cover did not load"
    );

    // ------------------------------------------------------------ the count
    // From start: nothing unread draws no number (not a 0 lines flow around):
    // pattern alone. Zero still in data for anything reading cover rather than
    // looking.
    mask!(NONE);
    assert_eq!(
        get!("unreadTotal", "text"),
        "0",
        "the count must be there as data from the start"
    );
    assert_eq!(call!("count", 1), "ok");
    mask!("1");
    assert_eq!(call!("count", 0), "ok");
    mask!(NONE);
    assert_eq!(get!("unreadTotal", "text"), "0");
    assert_eq!(call!("count", 4), "ok");
    mask!("4");
    assert_eq!(get!("unreadTotal", "text"), "4");
    assert_eq!(call!("count", 99), "ok");
    mask!("99");
    assert_eq!(get!("unreadTotal", "text"), "99");

    // Past cap every count shares one mask; data says cap.
    assert_eq!(call!("count", 100), "ok");
    mask!("99+");
    assert_eq!(get!("unreadTotal", "text"), "99+");
    assert_eq!(call!("count", 1234), "ok");
    mask!("99+");
    assert_eq!(get!("unreadTotal", "text"), "99+");

    // Negative count can't happen; if it did, names no missing mask, draws no
    // number.
    assert_eq!(call!("count", -3), "ok");
    mask!(NONE);
    assert_eq!(get!("unreadTotal", "text"), "0");
    assert_eq!(call!("count", 4), "ok");
    mask!("4");

    // ---------------------------------------------------------- the texture
    // Whole cover, nothing cut out: room for number is in mask; fade band would
    // take strip off pattern that is cover's whole face.
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
    // Room for status line made by TEXTURE: runs out towards cover foot while
    // something to read there, whole again after. Nothing laid over pattern:
    // checks mask's own band, not layer.
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

    // Line never runs off cover edges.
    //
    // `Row` centred on cover gives `Label` whatever width it asks; `Label` asks
    // for whole string on one line. German "Refresh failed" ≈ 1.5× cover width:
    // clipped at BOTH ends. Test can't load that catalogue, so cover squeezed
    // until English can't fit either: rule geometric, check too.
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

    // §9.3: server's own words never reach cover.
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

    // ------------------------------------------------------------ actions
    // Refresh action asks app for refresh.
    assert_eq!(
        call!("tap", QString::from("refreshAction")),
        "refresh 1",
        "the refresh action must ask for a refresh"
    );
}

/// Every mask cover can name exists, and is coverage mask.
///
/// TWO per count: `<count>.png` (pattern) and `<count>-edge.png` (outline laid
/// over in ambience colour). Second easier to lose, harder to notice (missing
/// pattern = bare cover; missing outline = cover looking like every other
/// count's minus line): both checked. `none.png` alone has no outline: no
/// number to trace.
///
/// Cover computes both sources: `qml_loads.rs` (reads literal sources) can't
/// see them. Qt reports missing image only as warning; mask re-exported as RGBA
/// tints cover solid (reason there).
#[test]
fn every_mask_the_cover_can_name_exists_and_is_a_mask() {
    let dir = repo_root().join("qml/art/cover");
    let mut problems: Vec<String> = Vec::new();
    let mut expected: std::collections::HashSet<String> = std::collections::HashSet::new();
    for key in cover_keys() {
        let mut names = vec![format!("{key}.png")];
        if key != NONE {
            names.push(format!("{key}-edge.png"));
        }
        for name in names {
            expected.insert(name.clone());
            let Ok(bytes) = std::fs::read(dir.join(&name)) else {
                problems.push(format!("{name} is missing"));
                continue;
            };
            if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
                problems.push(format!("{name} is not a PNG"));
                continue;
            }
            // IHDR always first: 8 bytes signature, 8 chunk header, then width,
            // height, bit depth, colour type.
            let depth = bytes.get(24).copied().unwrap_or(0);
            let colour = bytes.get(25).copied().unwrap_or(255);
            if (depth, colour) != (8, 0) {
                problems.push(format!(
                    "{name} is bit depth {depth} colour type {colour}; the shader \
                     reads coverage from the red channel of an 8-bit grayscale mask"
                ));
            }
        }
    }

    // Nothing else: mask for unnameable count = dead weight in every package.
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

/// 8-bit grayscale PNG as `scripts/png-mask.py` writes, read back to rows.
/// Nothing else accepted: test above holds masks to that format.
fn read_mask(name: &str) -> (usize, usize, Vec<u8>) {
    use std::io::Read as _;
    let bytes = std::fs::read(repo_root().join("qml/art/cover").join(name)).unwrap();
    let be32 = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let (w, h) = (be32(16), be32(20));
    assert_eq!(
        (bytes[24], bytes[25]),
        (8, 0),
        "{name} is not 8-bit grayscale"
    );
    let mut zipped = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let len = be32(at);
        if &bytes[at + 4..at + 8] == b"IDAT" {
            zipped.extend_from_slice(&bytes[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&zipped[..])
        .read_to_end(&mut raw)
        .unwrap();
    let mut px = vec![0u8; w * h];
    for y in 0..h {
        let filter = raw[y * (w + 1)];
        for x in 0..w {
            let left = if x > 0 { px[y * w + x - 1] } else { 0 };
            let up = if y > 0 { px[(y - 1) * w + x] } else { 0 };
            let up_left = if x > 0 && y > 0 {
                px[(y - 1) * w + x - 1]
            } else {
                0
            };
            let add = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => u8::midpoint(left, up),
                4 => paeth(left, up, up_left),
                other => panic!("{name}: PNG filter {other}"),
            };
            px[y * w + x] = raw[y * (w + 1) + 1 + x].wrapping_add(add);
        }
    }
    (w, h, px)
}

/// PNG Paeth predictor: neighbour nearest their linear guess.
fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let guess = i16::from(left) + i16::from(up) - i16::from(up_left);
    let off = |v: u8| (guess - i16::from(v)).abs();
    if off(left) <= off(up) && off(left) <= off(up_left) {
        left
    } else if off(up) <= off(up_left) {
        up
    } else {
        up_left
    }
}

/// Where count's outline sits across master: middle of outline ink box, px
/// right of master's middle.
fn outline_offset(key: &str) -> f64 {
    let (w, h, px) = read_mask(&format!("{key}-edge.png"));
    let inked: Vec<usize> = (0..w)
        .filter(|&x| (0..h).any(|y| px[y * w + x] > 0))
        .collect();
    let (first, last) = (inked[0], inked[inked.len() - 1]);
    let px_f = |n: usize| f64::from(u32::try_from(n).unwrap());
    px_f(first + last) / 2.0 - px_f(w - 1) / 2.0
}

/// 1, 3, 4 sit where they LOOK centred, not where painter's weight rule alone
/// puts them: see `coverNudge` in tools/textart/render.qml. Settled on device:
/// all three right of rule.
///
/// In px of 512-wide master, on outline ink box, each within a pixel of settled
/// position. Repaint dropping nudge, or masks not repainted after nudge
/// changed, fail here. 2 is control: never asked to move, hasn't.
#[test]
fn the_one_three_and_four_are_centred_by_eye() {
    for (key, want) in [("1", -37.5), ("3", -4.0), ("4", -9.5), ("2", -5.0)] {
        let got = outline_offset(key);
        assert!(
            (got - want).abs() < 1.0,
            "the {key}'s box is {got:+} px from centre; want {want:+}"
        );
    }
}
