//! The cover, drawn from the feeds the model hands it.
//!
//! The QML load test compiles and instantiates this file with every property
//! at its default, which is the empty cover and nothing else. What it cannot
//! see is the part that only exists once there are feeds: the staggered grid,
//! which is laid out by a pass of JavaScript over a parsed JSON list rather
//! than by a view over model rows, and the rule that decides which cells are
//! drawn bright. Both are runtime behaviour, so they need a running engine.
//!
//! No mirror and no worker here on purpose. The cover takes its feeds as a
//! string, so the interesting half can be driven directly -- including shapes
//! a real mirror is awkward to produce, like a feed with no icon at all.
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
        function allIn(node, name, found) {
            if (!node) { return found }
            if (node.objectName === name) { found.push(node) }
            var kids = node.data !== undefined ? node.data : node.children
            for (var i = 0; kids && i < kids.length; i++) {
                allIn(kids[i], name, found)
            }
            return found
        }
        function get(name, property) {
            var item = findIn(loader.item, name)
            if (!item) { return 'missing:' + name }
            return '' + item[property]
        }
        function at(name, which, property) {
            var items = allIn(loader.item, name, [])
            if (which >= items.length) { return 'missing:' + name + '[' + which + ']' }
            return '' + items[which][property]
        }
        function feeds(json) { loader.item.feedsJson = json; return 'ok' }
        function count(total) { loader.item.unreadCount = total; return 'ok' }
        function syncing(on) { loader.item.syncing = on; return 'ok' }
        function failure(text, auth) {
            loader.item.syncErrorIsAuth = auth
            loader.item.syncError = text
            return 'ok'
        }
        // The grid's cells, and how many of them are drawn bright.
        function drawn() { return '' + allIn(loader.item, 'gridCell', []).length }
        function lit() {
            var cells = allIn(loader.item, 'gridCell', [])
            var total = 0
            for (var i = 0; i < cells.length; i++) {
                if (cells[i].loud) { total += 1 }
            }
            return '' + total
        }
        // The leftmost cell: a shifted row starts half a cell off the edge.
        function leftmost() {
            var cells = allIn(loader.item, 'gridCell', [])
            var least = 0
            for (var i = 0; i < cells.length; i++) {
                if (cells[i].x < least) { least = cells[i].x }
            }
            return '' + least
        }
        function planned() { return '' + loader.item.cells.length }
        function listed() { return '' + loader.item.feedList.length }
        // Letters drawn into the field. The grid is favicons and nothing
        // else, so this is zero unless the feeds arrived without icons.
        function letters() {
            var all = allIn(loader.item, 'cellInitial', [])
            var shown = 0
            for (var i = 0; i < all.length; i++) {
                if (all[i].visible) { shown += 1 }
            }
            return '' + shown
        }
    }
";

/// Two feeds with something new and one quiet one, in the order the model
/// sends them: whatever is unread first, and every one of them with an icon,
/// since a feed the mirror has no favicon for does not reach the cover while
/// any other one does.
const FEEDS: &str = r#"[
    {"feedId":1,"title":"Tagesschau","unread":3,"icon":"data:image/png;base64,iVBORw0KGgo="},
    {"feedId":2,"title":"lwn","unread":1,"icon":"data:image/png;base64,iVBORw0KGgo="},
    {"feedId":3,"title":"Zeit","unread":0,"icon":"data:image/png;base64,iVBORw0KGgo="}
]"#;

/// What a first sync looks like: feeds, and no icons fetched yet.
const ICONLESS: &str = r#"[
    {"feedId":1,"title":"Tagesschau","unread":3,"icon":""},
    {"feedId":2,"title":"lwn","unread":1,"icon":""}
]"#;

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
fn the_cover_draws_a_field_of_feeds_and_lights_whichever_has_something_new() {
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

    assert_eq!(
        call!("load", QString::from(cover_url())),
        "ok",
        "the cover did not load"
    );

    // ---------------------------------------------------------- nothing yet
    assert_eq!(
        get!("emptyLabel", "visible"),
        "true",
        "a cover with no feeds must say so"
    );
    assert_eq!(get!("emptyLabel", "text"), "No feeds");
    assert_ne!(
        get!("emptyLabel", "wrapMode"),
        "0",
        "the line cannot wrap, so a longer language runs off the cover"
    );
    assert_eq!(
        call!("drawn"),
        "0",
        "a grid is drawn from feeds that are not there"
    );
    assert_eq!(
        get!("unreadTotal", "text"),
        "0",
        "the count must be there from the start; a zero says as much as a count"
    );
    assert_eq!(
        get!("brand", "text"),
        "Vuo",
        "the cover does not name the app in its corner"
    );
    assert_eq!(get!("subtitle", "text"), "Unread");

    // -------------------------------------------------------- with feeds in
    assert_eq!(call!("feeds", QString::from(FEEDS)), "ok");
    assert_eq!(call!("count", 4), "ok");

    assert_eq!(call!("listed"), "3", "all three feeds were handed over");
    assert_eq!(
        get!("emptyLabel", "visible"),
        "false",
        "the cover says there are no feeds while there are three"
    );
    assert_eq!(get!("unreadTotal", "text"), "4");

    let planned: usize = call!("planned").parse().unwrap_or(0);
    assert!(
        planned >= 8,
        "the cover's shape gives the grid fewer than two rows: {planned} cells"
    );
    assert_eq!(
        call!("drawn"),
        planned.to_string(),
        "the grid is not filled from the feeds there are"
    );
    assert!(
        planned > 3,
        "the grid must REPEAT the feeds to fill itself, not stop at three cells"
    );
    assert_eq!(
        call!("lit"),
        "2",
        "the two feeds with something new are not the two cells drawn bright, \
         once each"
    );
    let leftmost: f64 = call!("leftmost").parse().unwrap_or(0.0);
    assert!(
        leftmost < 0.0,
        "no row is shifted off the edge, so the rows do not stagger"
    );

    // The first cell is the first feed: its icon reaches the Image as the
    // `data:` URI it arrived as, and nothing in the cover fetches anything.
    let source = call!(
        "at",
        QString::from("cellFavicon"),
        0,
        QString::from("source")
    );
    assert!(
        source.starts_with("data:image/png;base64,"),
        "the first cell must draw the first feed's icon, got {source:?}"
    );
    // The field is favicons and nothing else. A letter among them is the bug
    // this asserts against: the model sends feeds without an icon only when NO
    // feed has one.
    assert_eq!(
        call!("letters"),
        "0",
        "a letter was drawn into a field of favicons"
    );

    // ------------------------------------------------- before any icon lands
    // Icons are fetched lazily, so a first sync has feeds and no pictures of
    // them. The grid draws initials then rather than nothing at all.
    assert_eq!(call!("feeds", QString::from(ICONLESS)), "ok");
    let planned_plain: usize = call!("planned").parse().unwrap_or(0);
    assert!(planned_plain > 0, "the grid emptied itself");
    assert_eq!(
        call!("letters"),
        planned_plain.to_string(),
        "with no icons anywhere every cell must fall back to a letter, or the \
         cover is blank under a real unread count"
    );
    assert_eq!(
        call!("at", QString::from("cellInitial"), 1, QString::from("text")),
        "L",
        "and the letter is the feed's own initial"
    );
    assert_eq!(call!("feeds", QString::from(FEEDS)), "ok");

    // ------------------------------------------------------- what sync says
    assert_eq!(call!("syncing", true), "ok");
    assert_eq!(get!("subtitle", "text"), "Refreshing");
    assert_eq!(
        get!("unreadTotal", "text"),
        "4",
        "the count must survive a refresh; it is the one thing the cover is for"
    );
    assert_eq!(call!("syncing", false), "ok");

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
