//! Entry list as search page draws it.
//!
//! Load test instantiates list without model: no delegate, no section header built. Gives
//! it model shaped like searching `EntryModel` (roles and counts as Rust hands over), reads
//! back what reader sees: groups, marked title and feed name, excerpt, found feeds, no
//! pulley. Same list NOT searching draws none of it: feed title is foreign, plain outside
//! search.
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

const PROBE_QML: &str = r#"
    import QtQuick 2.0
    Item {
        // EntryModel as QML sees it: roles, plus properties and methods list
        // reads.
        ListModel {
            id: fake
            property bool ready: true
            property bool syncing: false
            property int titleMatches: 4
            property int feedMatches: 2
            property int textMatches: 3
            property int titleShown: 2
            property int feedShown: 1
            property int textShown: 1
            property int titleNextLoad: 10
            property int feedNextLoad: 10
            property int textNextLoad: 10
            property string loadedMoreIn: ""
            function loadMoreIn(kind) { loadedMoreIn = kind }
            function setSearch(text) {}
            function setScope(kind, id) {}
            function requestSync() {}
            ListElement {
                entryId: 1; feedId: 1; author: ""; unread: true; starred: false
                published: 0; readingTime: 3; url: ""; feedIcon: ""
                title: "Harbour <at> dusk"
                titleStyled: "<b>Harbour</b> &lt;at&gt; dusk"
                feedName: "Harbour Gazette"
                feedNameStyled: "<b>Harbour</b> Gazette"
                matchKind: "title"; excerpt: ""; unreadCount: 0
            }
            ListElement {
                entryId: 3; feedId: 2; author: ""; unread: false; starred: false
                published: 0; readingTime: 0; url: ""; feedIcon: ""
                title: "Harbour lights"; titleStyled: "<b>Harbour</b> lights"
                feedName: "Daily"; feedNameStyled: "Daily"
                matchKind: "title"; excerpt: ""; unreadCount: 0
            }
            // Feed found by name: see models::EntryRow::feed_found.
            ListElement {
                entryId: -4; feedId: 4; author: ""; unread: true; starred: false
                published: 0; readingTime: 0; url: ""; feedIcon: ""
                title: "Harbour <Gazette>"; titleStyled: "<b>Harbour</b> &lt;Gazette&gt;"
                feedName: "Harbour <Gazette>"; feedNameStyled: ""
                matchKind: "feed"; excerpt: ""; unreadCount: 7
            }
            ListElement {
                entryId: 2; feedId: 2; author: ""; unread: true; starred: false
                published: 0; readingTime: 0; url: ""; feedIcon: ""
                title: "entry 2"; titleStyled: "entry 2"
                feedName: "Daily"; feedNameStyled: "Daily"
                matchKind: "text"; excerpt: "A walk by the <b>harbour</b>."
                unreadCount: 0
            }
        }
        // Page list is on, as far as opening feed reads it.
        Item {
            id: host
            property int status: 0
            property string browseModel: "browse"
            property string feedModel: "feeds"
            property string noticeModel: "notices"
        }
        // Silica's pageStack, found as page finds window's: by name, up context
        // chain.
        property QtObject pageStack: QtObject {
            property string pushed: ""
            function push(url, props) {
                var page = ('' + url).split('/').pop()
                pushed = page === "EntryListPage.qml"
                    ? [page, props.scopeKind, props.scopeId, props.title, props.model,
                       props.browseModel, props.feedModel, props.noticeModel].join(' | ')
                    : [page, props.entryId].join(' | ')
            }
        }
        function tap(nth) {
            var found = findAll(loader.item, 'listItem', [])
            found.sort(function(a, b) {
                return a.mapToItem(loader.item, 0, 0).y - b.mapToItem(loader.item, 0, 0).y
            })
            if (nth >= found.length) { return 'missing' }
            pageStack.pushed = ""
            found[nth].clicked()
            return pageStack.pushed
        }
        Loader { id: loader }
        function load(url, searching) {
            loader.source = ""
            loader.setSource(url, { width: 540, height: 960, searching: searching,
                                    entryModel: fake, hostPage: host })
            if (loader.status !== Loader.Ready) { return 'load-failed' }
            loader.item.forceLayout()
            return 'ok'
        }
        function findAll(node, name, out) {
            if (!node) { return out }
            if (node.objectName === name) { out.push(node) }
            var kids = node.data !== undefined ? node.data : node.children
            for (var i = 0; kids && i < kids.length; i++) { findAll(kids[i], name, out) }
            return out
        }
        function set(property, value) { fake[property] = value; return 'ok' }
        function setOnList(property, value) { loader.item[property] = value; return 'ok' }
        function click(name, nth) {
            var found = findAll(loader.item, name, [])
            found.sort(function(a, b) {
                return a.mapToItem(loader.item, 0, 0).y - b.mapToItem(loader.item, 0, 0).y
            })
            if (nth >= found.length) { return 'missing' }
            found[nth].clicked()
            return fake.loadedMoreIn
        }
        // nth item of that name, top to bottom as reader sees.
        function get(name, nth, property) {
            var found = findAll(loader.item, name, [])
            found.sort(function(a, b) {
                return a.mapToItem(loader.item, 0, 0).y - b.mapToItem(loader.item, 0, 0).y
            })
            if (nth >= found.length) { return 'missing:' + name + '#' + nth }
            return '' + found[nth][property]
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
fn a_search_lists_its_results_grouped_and_marked_and_nothing_else_does() {
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        repo_root().join("qml-stubs").to_string_lossy().into_owned(),
    ));
    engine.load_data(QByteArray::from(PROBE_QML));
    let url = format!(
        "file://{}",
        repo_root()
            .join("qml/components/EntryListView.qml")
            .display()
    );

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
        ($name:expr, $nth:expr, $property:expr) => {
            call!("get", QString::from($name), $nth, QString::from($property))
        };
    }
    // Qt's Text.TextFormat.
    const PLAIN: &str = "0";
    const STYLED: &str = "4";

    // ---------------------------------------------------------- searching
    assert_eq!(call!("load", QString::from(url.as_str()), true), "ok");
    assert_eq!(
        get!("pulley", 0, "visible"),
        "false",
        "the search page has no pulley: nothing on it was about a search"
    );
    assert_eq!(
        get!("sectionHeader", 0, "text"),
        "In titles (2 of 4)",
        "a group showing part of itself says how much of it"
    );
    assert_eq!(
        get!("sectionHeader", 1, "text"),
        "Feeds (1 of 2)",
        "one header per group"
    );
    assert_eq!(get!("sectionHeader", 2, "text"), "In article text (1 of 3)");

    // Bold AND highlight colour: bold alone hard to see on faded read row.
    let mark = |text: &str| format!("<b><font color=\"#80c0ff\">{text}</font></b>");
    assert_eq!(get!("titleLabel", 0, "textFormat"), STYLED);
    assert_eq!(
        get!("titleLabel", 0, "text"),
        format!("{} &lt;at&gt; dusk", mark("Harbour")),
        "a result's title is the one Rust escaped and marked"
    );
    assert_eq!(get!("detailLabel", 0, "textFormat"), STYLED);
    assert_eq!(
        get!("detailLabel", 0, "text"),
        format!("{} Gazette  \u{b7}  3 min read", mark("Harbour")),
        "and so is its feed's name, with the rest of the line beside it"
    );
    assert_eq!(
        get!("excerptLabel", 0, "visible"),
        "false",
        "a title hit has no excerpt"
    );
    assert_eq!(get!("excerptLabel", 3, "visible"), "true");
    assert_eq!(get!("excerptLabel", 3, "textFormat"), STYLED);
    assert_eq!(
        get!("excerptLabel", 3, "text"),
        format!("A walk by the {}.", mark("harbour"))
    );

    // Feed found by name drawn as feed list draws it (name marked, unread
    // count), opens as there.
    assert_eq!(get!("titleLabel", 2, "textFormat"), STYLED);
    assert_eq!(
        get!("titleLabel", 2, "text"),
        format!("{} &lt;Gazette&gt;", mark("Harbour"))
    );
    assert_eq!(
        get!("detailLabel", 2, "visible"),
        "false",
        "an article's date and reading time are not a feed's"
    );
    assert_eq!(get!("detailLabel", 0, "visible"), "true");
    assert_eq!(get!("feedUnread", 2, "visible"), "true");
    assert_eq!(get!("feedUnread", 2, "text"), "7");
    assert_eq!(
        get!("feedUnread", 0, "visible"),
        "false",
        "an article has no count"
    );
    assert_eq!(
        get!("listItem", 2, "showMenuOnPressAndHold"),
        "false",
        "an article's menu does nothing for a feed"
    );
    assert_eq!(get!("listItem", 0, "showMenuOnPressAndHold"), "true");
    assert_eq!(
        call!("tap", 2),
        "EntryListPage.qml | 3 | 4 | Harbour <Gazette> | browse | browse | feeds | notices",
        "a feed opens its articles, over the browse model, as from the feed list"
    );
    assert_eq!(
        call!("tap", 0),
        "ArticlePage.qml | 1",
        "and an article opens the article"
    );

    // "Load more" under last row of group with more than shown; nowhere else.
    assert_eq!(
        get!("loadMore", 0, "visible"),
        "false",
        "not between two rows of a group"
    );
    assert_eq!(get!("loadMore", 1, "visible"), "true");
    assert_eq!(get!("loadMore", 2, "visible"), "true");
    assert_eq!(get!("loadMore", 3, "visible"), "true");
    assert_eq!(
        get!("loadMoreLabel", 1, "text"),
        "Load 2 more",
        "the next step, but no more than are left"
    );
    assert_eq!(
        call!("click", QString::from("loadMore"), 3),
        "text",
        "each asks for more of its own group"
    );
    assert_eq!(call!("click", QString::from("loadMore"), 2), "feed");
    assert_eq!(call!("click", QString::from("loadMore"), 1), "title");
    assert_eq!(call!("set", QString::from("textMatches"), 30), "ok");
    assert_eq!(get!("loadMoreLabel", 3, "text"), "Load 10 more");
    assert_eq!(call!("set", QString::from("textNextLoad"), 20), "ok");
    assert_eq!(get!("loadMoreLabel", 3, "text"), "Load 20 more");
    assert_eq!(call!("set", QString::from("titleNextLoad"), 0), "ok");
    assert_eq!(get!("loadMoreLabel", 1, "text"), "Load all results");
    assert_eq!(
        get!("loadMoreLabel", 3, "text"),
        "Load 20 more",
        "per group"
    );
    assert_eq!(call!("set", QString::from("textMatches"), 3), "ok");

    // Group showing all of itself has nothing more to load.
    assert_eq!(call!("set", QString::from("textShown"), 3), "ok");
    assert_eq!(get!("sectionHeader", 2, "text"), "In article text (3)");
    assert_eq!(get!("loadMore", 3, "visible"), "false");
    assert_eq!(call!("set", QString::from("feedShown"), 2), "ok");
    assert_eq!(get!("sectionHeader", 1, "text"), "Feeds (2)");
    assert_eq!(get!("loadMore", 2, "visible"), "false");

    // Empty search says what is searched, nothing more.
    assert_eq!(get!("placeholder", 0, "text"), "");
    assert_eq!(
        get!("placeholder", 0, "hintText"),
        "Vuo searches the title and text of every article on this device, and the \
         names of your feeds."
    );
    assert_eq!(
        call!("setOnList", QString::from("searchText"), QString::from("x")),
        "ok"
    );
    assert_eq!(get!("placeholder", 0, "text"), "No articles found");

    // -------------------------------------------------------- not searching
    assert_eq!(call!("load", QString::from(url.as_str()), false), "ok");
    assert_eq!(get!("pulley", 0, "visible"), "true");
    assert!(
        get!("sectionHeader", 0, "text").starts_with("missing:"),
        "a list that is not a search is not grouped"
    );
    assert_eq!(
        get!("titleLabel", 0, "textFormat"),
        PLAIN,
        "§9.3: outside a search, a feed's title is foreign text and drawn plain"
    );
    assert_eq!(get!("titleLabel", 0, "text"), "Harbour <at> dusk");
    assert_eq!(get!("detailLabel", 0, "textFormat"), PLAIN);
    assert_eq!(
        get!("detailLabel", 0, "text"),
        "Harbour Gazette  \u{b7}  3 min read"
    );
    assert_eq!(get!("excerptLabel", 3, "visible"), "false");
    assert_eq!(get!("loadMore", 1, "visible"), "false");
    assert_eq!(
        get!("feedUnread", 2, "visible"),
        "false",
        "outside a search no row is a feed"
    );
    assert_eq!(get!("detailLabel", 2, "visible"), "true");
}
