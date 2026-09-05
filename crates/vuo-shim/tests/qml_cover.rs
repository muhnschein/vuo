//! The cover, drawn from the feeds the model hands it.
//!
//! The QML load test compiles and instantiates this file with every property
//! at its default, which is the empty cover and nothing else. What it cannot
//! see is the part that only exists once there are feeds: the rings of
//! favicons around the centre, which are laid out by a pass of JavaScript over
//! a parsed JSON list rather than by a view over model rows, and the rules
//! that decide how bright each ring is drawn and when the count appears. All
//! of that is runtime behaviour, so it needs a running engine.
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
        // The field's cells, and the rings they are on.
        function drawn() { return '' + allIn(loader.item, 'fieldCell', []).length }
        function rings() {
            var cells = allIn(loader.item, 'fieldCell', [])
            var outermost = -1
            for (var i = 0; i < cells.length; i++) {
                if (cells[i].ring > outermost) { outermost = cells[i].ring }
            }
            return '' + (outermost + 1)
        }
        // How bright the cells on one ring are drawn: the least of them, so a
        // single wrong cell shows.
        function ringOpacity(ring) {
            var cells = allIn(loader.item, 'fieldCell', [])
            var least = 2
            var any = false
            for (var i = 0; i < cells.length; i++) {
                if (cells[i].ring !== ring) { continue }
                any = true
                if (cells[i].opacity < least) { least = cells[i].opacity }
            }
            return any ? '' + least : 'missing:ring' + ring
        }
        // How far the nearest cell's centre is from the cover's centre, in
        // icon widths: the hole the count sits in.
        function hole() {
            var cells = allIn(loader.item, 'fieldCell', [])
            var item = loader.item
            var nearest = 1e9
            for (var i = 0; i < cells.length; i++) {
                var dx = cells[i].x + cells[i].width / 2 - item.width / 2
                var dy = cells[i].y + cells[i].height / 2 - item.height / 2
                var d = Math.sqrt(dx * dx + dy * dy)
                if (d < nearest) { nearest = d }
            }
            return '' + (nearest / item.iconSize)
        }
        // Whether any cell lies wholly outside the cover.
        function stray() {
            var cells = allIn(loader.item, 'fieldCell', [])
            var item = loader.item
            for (var i = 0; i < cells.length; i++) {
                var c = cells[i]
                if (c.x + c.width <= 0 || c.x >= item.width
                        || c.y + c.height <= 0 || c.y >= item.height) {
                    return 'yes'
                }
            }
            return 'no'
        }
        // Which feed the first cells hold, in order: the innermost ring's.
        function innermostFeeds() {
            var cells = loader.item.cells
            var ids = []
            for (var i = 0; i < cells.length; i++) {
                if (cells[i].ring === 0) { ids.push(cells[i].feed.feedId) }
            }
            return ids.join(',')
        }
        function planned() { return '' + loader.item.cells.length }
        function listed() { return '' + loader.item.feedList.length }
        function monochrome() {
            var field = findIn(loader.item, 'faviconField')
            return field ? '' + field.layer.enabled : 'missing:faviconField'
        }
        // Whether the count sits over the centre of the cover.
        function countCentred() {
            var label = findIn(loader.item, 'unreadTotal')
            var item = loader.item
            var dx = Math.abs(label.x + label.width / 2 - item.width / 2)
            var dy = Math.abs(label.y + label.height / 2 - item.height / 2)
            return dx <= 1 && dy <= 1 ? 'yes' : 'no:' + dx + ',' + dy
        }
        // Letters drawn into the field. The field is favicons and nothing
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
fn the_cover_draws_rings_of_feeds_around_the_count() {
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

    // ------------------------------------------------------ I. no feeds yet
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
        "a field is drawn from feeds that are not there"
    );
    assert_eq!(
        get!("unreadTotal", "visible"),
        "false",
        "a zero says nothing the empty centre does not"
    );
    assert_eq!(
        get!("status", "visible"),
        "false",
        "nothing is on the cover at rest but the feeds and the count"
    );

    // --------------------------------------------------- III. feeds and news
    assert_eq!(call!("feeds", QString::from(FEEDS)), "ok");
    assert_eq!(call!("count", 4), "ok");

    assert_eq!(call!("listed"), "3", "all three feeds were handed over");
    assert_eq!(
        get!("emptyLabel", "visible"),
        "false",
        "the cover says there are no feeds while there are three"
    );
    assert_eq!(get!("unreadTotal", "visible"), "true");
    assert_eq!(get!("unreadTotal", "text"), "4");
    assert_eq!(
        call!("countCentred"),
        "yes",
        "the count sits in the middle of the rings, not in a corner"
    );

    let planned: usize = call!("planned").parse().unwrap_or(0);
    assert_eq!(
        call!("drawn"),
        planned.to_string(),
        "the field is not filled from the feeds there are"
    );
    assert!(
        planned > 3,
        "the field must REPEAT the feeds to fill itself, not stop at three cells"
    );
    let rings: usize = call!("rings").parse().unwrap_or(0);
    assert!(
        rings >= 3,
        "the cover's shape gives fewer than three rings: {rings}"
    );
    assert_eq!(
        call!("stray"),
        "no",
        "a cell wholly outside the cover is a feed drawn where nobody can see it"
    );
    let hole = number!(call!("hole"));
    assert!(
        hole >= 1.5,
        "the innermost ring must leave the centre clear for the count; the \
         nearest cell is only {hole} icons from it"
    );

    // OPAQUE INSIDE, TRANSPARENT OUTSIDE.
    let inner = number!(call!("ringOpacity", 0));
    let next = number!(call!("ringOpacity", 1));
    let outer = number!(call!("ringOpacity", rings as i32 - 1));
    assert!(
        (inner - 1.0).abs() < 1e-6,
        "with news, the innermost ring is drawn solid, got {inner}"
    );
    assert!(
        next < inner && outer < next,
        "each ring out must be fainter than the one inside it: {inner}, {next}, \
         ... {outer}"
    );
    assert!(
        outer > 0.0,
        "but never gone: the outermost ring is still the feeds"
    );

    // The feeds with something new come first from the model, so they take
    // the innermost ring, repeated round it in order.
    let innermost = call!("innermostFeeds");
    assert!(
        innermost.starts_with("1,2,3,1"),
        "the innermost ring must hold the feeds in the model's order, \
         repeated, got {innermost}"
    );

    // ALL MONOCHROME: the field is drawn through its layer's shader.
    assert_eq!(
        call!("monochrome"),
        "true",
        "the field must be rendered through the monochrome layer"
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

    // ---------------------------------------------- II. feeds, nothing new
    assert_eq!(call!("count", 0), "ok");
    assert_eq!(
        get!("unreadTotal", "visible"),
        "false",
        "no news, no number: the empty centre says it"
    );
    assert_eq!(
        call!("drawn"),
        planned.to_string(),
        "the feeds stay on the cover when there is nothing new"
    );
    let quiet_inner = number!(call!("ringOpacity", 0));
    let quiet_outer = number!(call!("ringOpacity", rings as i32 - 1));
    assert!(
        quiet_inner < 0.5 && (quiet_inner - quiet_outer).abs() < 1e-6,
        "with nothing new every ring is drawn faint, and equally so: \
         {quiet_inner} inside, {quiet_outer} outside"
    );
    assert_eq!(call!("count", 4), "ok");

    // ------------------------------------------------- before any icon lands
    // Icons are fetched lazily, so a first sync has feeds and no pictures of
    // them. The field draws initials then rather than nothing at all.
    assert_eq!(call!("feeds", QString::from(ICONLESS)), "ok");
    let planned_plain: usize = call!("planned").parse().unwrap_or(0);
    assert!(planned_plain > 0, "the field emptied itself");
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
    assert_eq!(get!("status", "visible"), "true");
    assert_eq!(get!("subtitle", "text"), "Refreshing");
    assert_eq!(
        get!("unreadTotal", "text"),
        "4",
        "the count must survive a refresh; it is the one thing the cover is for"
    );
    assert_eq!(get!("unreadTotal", "visible"), "true");
    assert_eq!(call!("syncing", false), "ok");
    assert_eq!(
        get!("status", "visible"),
        "false",
        "the line goes when the refresh has nothing more to say"
    );

    // §9.3: the server's own words never reach the cover.
    assert_eq!(
        call!(
            "failure",
            QString::from("<b>500</b> from feeds.example"),
            false
        ),
        "ok"
    );
    assert_eq!(get!("status", "visible"), "true");
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

    // A number too wide for the hole is capped rather than pushed into the
    // rings.
    assert_eq!(call!("count", 1234), "ok");
    assert_eq!(get!("unreadTotal", "text"), "999+");
}
