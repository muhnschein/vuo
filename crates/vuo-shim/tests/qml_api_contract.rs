//! Every member QML calls must exist on a Rust `QObject`.
//!
//! # Why
//!
//! QML load test compiles every page: catches unknown *types* and undeclared
//! *properties*, not calls to missing methods. QML resolves method calls at
//! **runtime**: `model.setRedd(index, true)` compiles fine, fails on device at
//! first tap. Unimplemented methods otherwise pass every check in the build.
//!
//! # How
//!
//! `qmetaobject` emits names verbatim: Rust source *is* QML API. Scans
//! `qt_property!`/`qt_method!`/`qt_signal!` declarations for usable names,
//! scans QML for members accessed on a Vuo model, reports anything undeclared.
//!
//! Checks union of members across all Vuo types, not per type: QML passes
//! models via `property var`, receiver type not always recoverable. Union still
//! catches every typo and unimplemented method (failure mode that matters).

// Test code: panic denials guard production foreign-input paths, not test
// assertions. See note in vuo-core's lib.rs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Names declared with `qt_property!`, `qt_method!` or `qt_signal!`.
fn declared_members(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in ["models.rs", "article.rs", "settings.rs", "arrivals.rs"] {
        let path = root.join("crates/vuo-shim/src").join(file);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        for line in source.lines() {
            let trimmed = line.trim();
            let Some((name, rest)) = trimmed.split_once(':') else {
                continue;
            };
            if !rest.trim_start().starts_with("qt_property!")
                && !rest.trim_start().starts_with("qt_method!")
                && !rest.trim_start().starts_with("qt_signal!")
            {
                continue;
            }
            // Strip visibility modifier: `pub blockedImages: qt_property!(...)`
            // declares `blockedImages`, not `pub blockedImages`. Else member
            // silently unregistered; test blames QML using it, pointing at wrong
            // file.
            let name = name
                .trim()
                .strip_prefix("pub(crate) ")
                .or_else(|| name.trim().strip_prefix("pub "))
                .unwrap_or_else(|| name.trim())
                .trim();
            if name.is_empty() || name == "base" {
                continue;
            }
            names.insert(name.to_owned());
            // `qt_signal!` also gives QML `onName` handler.
            let mut chars = name.chars();
            if let Some(first) = chars.next() {
                names.insert(format!("on{}{}", first.to_uppercase(), chars.as_str()));
            }
            // NOTIFY signal named in property declaration usable too.
            if let Some(idx) = rest.find("NOTIFY ") {
                if let Some(sig) = rest.get(idx + 7..) {
                    let sig: String = sig
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !sig.is_empty() {
                        names.insert(sig);
                    }
                }
            }
        }
    }
    names
}

/// Tab strip's titles and scope kinds stay same length.
///
/// Two parallel arrays in EntryListPage.qml (`titles` on strip, `scopeTabKinds`
/// on page), mapped by index. Fourth title without fourth kind → tap resolves
/// `scopeTabKinds[3]` to `undefined`, `setScope` gets 0: tab silently shows
/// Unread. Runtime array lookup; nothing else notices.
#[test]
fn every_scope_tab_has_a_scope_kind() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root");
    // Kinds live with page owning pager. Strip location (page vs list header)
    // has moved; titles looked for in both, not pinned to today's file.
    let page = std::fs::read_to_string(root.join("qml/pages/EntryListPage.qml"))
        .expect("EntryListPage.qml");
    let view = std::fs::read_to_string(root.join("qml/components/EntryListView.qml"))
        .expect("EntryListView.qml");
    let both = format!("{page}\n{view}");

    let array_len = |source: &str, needle: &str| -> usize {
        let line = source
            .lines()
            .find(|l| l.trim_start().starts_with(needle))
            .unwrap_or_else(|| panic!("no line starting `{needle}`"));
        let open = line.find('[').expect("an array literal");
        let close = line[open..].find(']').expect("a closed array literal") + open;
        let inner = line[open + 1..close].trim();
        if inner.is_empty() {
            0
        } else {
            inner.split(',').count()
        }
    };

    let kinds = array_len(&page, "property var scopeTabKinds:");
    let titles = array_len(&both, "titles:");
    assert_eq!(
        kinds, titles,
        "the strip shows {titles} tabs but the page maps {kinds} scope kinds; \
         a tab with no kind resolves to undefined and silently shows Unread"
    );
    assert!(
        kinds >= 3,
        "Unread, Starred and All are all meant to be reachable"
    );
}

/// §a pushed feed view never re-scopes a tab's model.
///
/// `setScope` = plain overwrite, nothing restores: feed view's model shows that
/// feed until rescoped. Handing it a TAB's model is permanent ("open feed,
/// swipe back twice, list still shows only that feed, no way back but killing
/// app").
///
/// qmllint and load test blind: both are `property var` holding valid
/// `EntryModel`; wrong only in where else bound. Wiring asserted here.
#[test]
fn a_feed_view_is_never_given_a_tabs_model() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root");
    let window =
        std::fs::read_to_string(root.join("qml/harbour-vuo.qml")).expect("harbour-vuo.qml");
    let view = std::fs::read_to_string(root.join("qml/components/EntryListView.qml"))
        .expect("EntryListView.qml");
    let feeds =
        std::fs::read_to_string(root.join("qml/pages/FeedListPage.qml")).expect("FeedListPage.qml");

    // Models bound to three tabs, by window's ids.
    let tab_models: Vec<String> = {
        let line = window
            .lines()
            .find(|l| l.trim_start().starts_with("scopeModels:"))
            .expect("the window binds scopeModels");
        let open = line.find('[').expect("an array literal");
        let close = line[open..].find(']').expect("a closed array literal") + open;
        line[open + 1..close]
            .split(',')
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect()
    };
    assert!(tab_models.len() >= 3, "three tabs, three models");

    let browse = window
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("browseModel:"))
        .expect("the window names the model feed views scope")
        .trim()
        .to_owned();
    assert!(
        !tab_models.contains(&browse),
        "the browse model is `{browse}`, which is also a tab's ({tab_models:?}); \
         opening a feed would leave that tab showing the feed"
    );

    // Pulley must reach for it, not list it's on.
    let push = view
        .split_once("FeedListPage.qml")
        .map(|(_, rest)| rest.split_once("})").map_or(rest, |(args, _)| args))
        .expect("the pulley opens the feeds page");
    assert!(
        push.contains("browseModel"),
        "the Feeds pulley item must hand over the browse model, not this \
         tab's; it passes: {push}"
    );

    // Feeds page hands same model to page it pushes, and on again: feed opened
    // from THAT page has one too.
    let opened = feeds
        .split_once("EntryListPage.qml")
        .map(|(_, rest)| rest.split_once("})").map_or(rest, |(args, _)| args))
        .expect("the feeds page opens a feed");
    assert!(
        opened.contains("model: page.entryModel"),
        "a feed opens onto the browse model it was given: {opened}"
    );
    assert!(
        opened.contains("browseModel: page.entryModel"),
        "and passes it on, so the feed view's own Feeds pulley works: {opened}"
    );
}

/// §the search page has a model of its own.
///
/// Same trap one page over: search page scopes handed model to scope 5;
/// `setScope` overwrites. Tab's model → tab lists search results; browse model
/// → re-scopes open feed view underneath.
#[test]
fn a_search_is_never_given_another_lists_model() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root");
    let window =
        std::fs::read_to_string(root.join("qml/harbour-vuo.qml")).expect("harbour-vuo.qml");
    let view = std::fs::read_to_string(root.join("qml/components/EntryListView.qml"))
        .expect("EntryListView.qml");

    let bound = |name: &str| -> String {
        window
            .lines()
            .find_map(|l| l.trim_start().strip_prefix(name))
            .unwrap_or_else(|| panic!("the window binds `{name}`"))
            .trim()
            .to_owned()
    };
    let tabs = bound("scopeModels:");
    let browse = bound("browseModel:");
    let search = bound("searchModel:");
    assert!(
        !tabs.contains(&search),
        "the search model `{search}` is also a tab's ({tabs}); a search would leave \
         that tab listing its results"
    );
    assert_ne!(
        search, browse,
        "the search model is the browse model; a search would re-scope a feed view"
    );

    // Pulley's Search item hands that model over as page's own.
    let push = view
        .split_once("searching: true")
        .and_then(|(before, _)| before.rsplit_once("pageStack.push("))
        .map(|(_, args)| args)
        .expect("the pulley opens a search");
    assert!(
        push.contains("model: listView.hostPage ? listView.hostPage.searchModel"),
        "the Search pulley item must hand over the search model, not this tab's: {push}"
    );
    assert!(
        push.contains("scopeKind: 5"),
        "and scope it to models::Scope::Search: {push}"
    );
}

/// §no property is bound to itself.
///
/// `account: account` inside `OnboardingPage { }` ≠ "the id called account".
/// Binding resolves against object's OWN properties first; `OnboardingPage`
/// declares `property var account`, so right name = left name: Qt reports
/// binding loop, leaves default; everything reading it silently does nothing.
///
/// Device log showed `QML OnboardingPage: Binding loop detected for property
/// "account"`. `qml_loads.rs` fails on that warning only for bindings it
/// evaluates; `Component { }` inside file isn't instantiated by creating root.
/// Reading source catches it regardless.
///
/// `key: value` inside call arguments (`pageStack.push(url, { hideGlobally:
/// hideGlobally })`) is opposite: left = pushed page's property, right = role in
/// scope; correct. Those sit inside parentheses; object-declaration binding
/// doesn't. Depth count below is for that.
#[test]
fn no_property_is_bound_to_itself() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root");

    let mut files = Vec::new();
    qml_files(&root.join("qml"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no QML found");

    let mut found: Vec<String> = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).expect("read qml");
        // Parens inside string or comment aren't nesting.
        let source = strip_strings_and_comments(&source);
        let mut depth: i32 = 0;
        for (lineno, line) in source.lines().enumerate() {
            if depth == 0 {
                if let Some((left, right)) = line.split_once(':') {
                    let left = left.trim();
                    let right = right.trim().trim_end_matches(',');
                    if !left.is_empty()
                        && left == right
                        && left.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && left.chars().next().is_some_and(char::is_alphabetic)
                    {
                        found.push(format!(
                            "{}:{} — `{left}: {right}` binds a property to itself",
                            file.strip_prefix(root).unwrap_or(file).display(),
                            lineno + 1
                        ));
                    }
                }
            }
            depth += i32::try_from(line.chars().filter(|c| *c == '(').count()).unwrap();
            depth -= i32::try_from(line.chars().filter(|c| *c == ')').count()).unwrap();
            depth = depth.max(0);
        }
    }

    assert!(
        found.is_empty(),
        "a binding whose value is its own name is a loop, not a reference to \
         an id of that name; rename one of the two:\n{}",
        found.join("\n")
    );
}

/// Role names exposed via `role_names()`; delegates see them as context
/// properties, not model members.
fn declared_roles(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in ["models.rs", "article.rs"] {
        let source = std::fs::read_to_string(root.join("crates/vuo-shim/src").join(file))
            .unwrap_or_default();
        for line in source.lines() {
            if let Some(rest) = line.trim().strip_prefix("names.insert(") {
                if let Some(start) = rest.find('"') {
                    let after = rest.get(start + 1..).unwrap_or_default();
                    if let Some(end) = after.find('"') {
                        if let Some(role) = after.get(..end) {
                            names.insert(role.to_owned());
                        }
                    }
                }
            }
        }
    }
    names
}

fn qml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            qml_files(&path, out);
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("qml") {
            out.push(path);
        }
    }
}

#[test]
fn every_member_the_qml_uses_is_implemented_in_rust() {
    // Identifiers holding a Vuo model in QML. `model` and `feedModel` =
    // `property var` names pages receive them through.
    const RECEIVERS: &[&str] = &[
        "entries",
        "feeds",
        "article",
        "settings",
        "account",
        "model",
        "feedModel",
        // Root window's own ids. Window reads `accountSettings` every sync;
        // `arrivals` only called, never bound: typo in either fails only on
        // device, only on cover.
        "accountSettings",
        "arrivals",
        // Every other name EntryModel travels under. `entryModel` = what
        // EntryListView calls every list action through (mark read, star, load
        // more, sync, search); typo like `entryModel.setSerch(...)` must fail
        // here.
        "entryModel",
        "noticeModel",
        "browseModel",
        "searchModel",
        "starredEntries",
        "allEntries",
        "browseEntries",
        "searchEntries",
    ];

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .to_path_buf();

    let declared = declared_members(&root);
    assert!(
        !declared.is_empty(),
        "found no qt_property/qt_method declarations to check against"
    );

    let mut files = Vec::new();
    qml_files(&root.join("qml"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no QML found");

    let mut missing: Vec<String> = Vec::new();

    for file in &files {
        let source = std::fs::read_to_string(file).expect("read qml");
        for (lineno, line) in source.lines().enumerate() {
            // Skip comments: mention in prose isn't call.
            let code = line.split("//").next().unwrap_or("");
            for receiver in RECEIVERS {
                let needle = format!("{receiver}.");
                let mut from = 0usize;
                while let Some(idx) = code.get(from..).and_then(|s| s.find(&needle)) {
                    let at = from + idx;
                    // Require non-identifier char before receiver: `page.model.`
                    // matches `model`, `xmodel.` doesn't.
                    let preceded_ok = at == 0
                        || !code
                            .get(..at)
                            .and_then(|s| s.chars().last())
                            .is_some_and(|c| c.is_alphanumeric() || c == '_');
                    from = at + needle.len();
                    if !preceded_ok {
                        continue;
                    }
                    let member: String = code
                        .get(from..)
                        .unwrap_or_default()
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if member.is_empty() || declared.contains(&member) {
                        continue;
                    }
                    // Qt built-ins on every QObject.
                    if matches!(member.as_str(), "objectName" | "destroy" | "count") {
                        continue;
                    }
                    missing.push(format!(
                        "{}:{} — `{receiver}.{member}` is not declared on any Vuo QObject",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        lineno + 1
                    ));
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "QML calls members that do not exist. QML resolves these at RUNTIME, so \
         nothing else in the build catches them — they fail on the device when a \
         user taps.\n\n{}\n\nDeclared members are:\n{:?}",
        missing.join("\n"),
        declared
    );
}

/// Roles Rust models expose that no QML page reads.
///
/// Check below: every *other* declared role referenced, so renaming or dropping
/// role UI depends on fails. Adding name here = deliberate statement UI doesn't
/// use it, not way to quiet test.
const ROLES_QML_DOES_NOT_USE: &[&str] = &[
    // Article's plain-text rendering, kept for share/copy action not built yet.
    "plainText",
    // Transform records code block language; UI renders code unhighlighted,
    // nothing reads it.
    "codeLanguage",
    // List numbering comes from the pre-rendered `marker`.
    "ordered",
    // Entry list detail line = feed | age | reading time | star. Author left
    // out on request: duplicated feed name for most feeds, pushed identifying
    // line off row. Role stays: mirror has it; future article header is obvious
    // home.
    "author",
];

/// QML and Silica names legitimately bare camelCase in value position.
/// Everything else bare and camelCase in page must be model role.
///
/// Short on purpose: a list swallowing role and method vocabulary (`entryId`,
/// `unreadCount`, `setRead`, ...) can't see typos in them.
const QML_VALUE_NAMES: &[&str] = &[
    "qsTr",
    "pageStack",
    "remorseAction",
    "currentIndex",
    "defaultAllowedOrientations",
    "implicitHeight",
    // Pair of it. Both Item's own; article view reads them off loaded Image for
    // true aspect ratio.
    "implicitWidth",
    "sourceSize",
    "textFormat",
    // What Repeater or view calls entry of plain JS-array model. Cover lays
    // grid over one: repeats given feeds until grid full, which view over rows
    // can't.
    "modelData",
];

/// Does this word have the shape of one of our role names?
fn looks_like_a_role(word: &str) -> bool {
    word.len() >= 4
        && word.chars().next().is_some_and(char::is_lowercase)
        && word.chars().any(char::is_uppercase)
}

/// Bare identifiers on one QML line (`bare_identifiers`): comments and strings
/// removed; member accesses (`model.title`) and property sets (`textFormat:`)
/// excluded. Member accesses = other test's job; name left of colon = property
/// this QML declares, not value read.
///
/// This function: blank insides of `"..."` and `'...'` literals and every
/// comment, keeping newlines so line numbers hold.
///
/// One pass over whole source, strings and comments together: each can contain
/// other's delimiter (`//` comment holding apostrophe like `the feed's name`
/// read as opening quote swallows rest of file; `/*` in string is text). Per
/// line not enough: cover's `ShaderEffect` GLSL is ONE literal over a dozen
/// lines, inner lines read as code full of `qt_TexCoord0` and `gl_FragColor`.
/// Name inside string or comment never a role lookup, however many lines.
fn strip_strings_and_comments(source: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum In {
        Code,
        Line,
        Block,
        Str(char),
    }
    let mut out = String::with_capacity(source.len());
    let mut state = In::Code;
    let mut escaped = false;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            out.push('\n');
            if state == In::Line {
                state = In::Code;
            }
            continue;
        }
        match state {
            In::Code => {
                if c == '/' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = In::Line;
                } else if c == '/' && chars.peek() == Some(&'*') {
                    chars.next();
                    state = In::Block;
                } else if c == '"' || c == '\'' {
                    state = In::Str(c);
                    out.push(c);
                } else {
                    out.push(c);
                }
            }
            In::Line => {}
            In::Block => {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = In::Code;
                }
            }
            In::Str(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    state = In::Code;
                    out.push(c);
                }
            }
        }
    }
    out
}

fn bare_identifiers(line: &str) -> Vec<String> {
    let code = line.split("//").next().unwrap_or("");

    // Role name inside translated string isn't reference.
    let mut cleaned = String::with_capacity(code.len());
    let mut quote: Option<char> = None;
    for c in code.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    cleaned.push(' ');
                } else {
                    cleaned.push(c);
                }
            }
        }
    }

    let chars: Vec<char> = cleaned.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if !(chars[i].is_alphabetic() || chars[i] == '_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        let preceded_by_dot = start > 0 && chars[start - 1] == '.';
        let mut j = i;
        while chars.get(j) == Some(&' ') {
            j += 1;
        }
        // Colon after identifier = property binding, UNLESS `?` came first on
        // line: then it closes ternary, identifier is value read.
        //
        // Else `text: visible ? styledText : ""` scans as binding named
        // `styledText`; role counts unreferenced, test loses that binding
        // shape.
        let ternary_before = chars[..start].contains(&'?');
        let is_binding = chars.get(j) == Some(&':') && !ternary_before;
        if !preceded_by_dot && !is_binding {
            out.push(chars[start..i].iter().collect());
        }
    }
    out
}

/// Names QML introduces itself: `property var x`, `id: x`, `signal x`,
/// `function x()`. Legitimately bare, not roles.
fn qml_declared_identifiers(files: &[PathBuf]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in files {
        let source = std::fs::read_to_string(file).expect("read qml");
        for line in source.lines() {
            let code = line.split("//").next().unwrap_or("").trim();

            // `id:` anywhere on line, not just start: usual form `EntryModel {
            // id: entries }`. Else every role-shaped id reported as undefined
            // role.
            let mut scan = code.split_whitespace().peekable();
            while let Some(word) = scan.next() {
                if word == "id:" {
                    if let Some(name) = scan.peek() {
                        names.insert((*name).trim_end_matches('}').trim().to_owned());
                    }
                }
            }

            // JS locals inside handler or function. `var shortAge = ...` isn't
            // model role; reporting it = noise training reader to ignore test.
            //
            // EVERY declarator of comma-separated `var a = 1, b = 2`, not just
            // first (text art declares run and run width on one line).
            for keyword in ["var ", "let ", "const "] {
                let mut rest = code;
                while let Some(idx) = rest.find(keyword) {
                    // `myvar x` declares nothing: keyword must start word.
                    let starts_word = idx == 0
                        || !rest[..idx].ends_with(|c: char| c.is_alphanumeric() || c == '_');
                    rest = rest.get(idx + keyword.len()..).unwrap_or("");
                    if !starts_word {
                        continue;
                    }
                    for part in rest.split(',') {
                        let part = part.trim_start();
                        let name: String = part
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        // Declarator = `name`, `name =` or `name;`. Else comma
                        // was inside initialiser (argument list, array);
                        // declaration ended before it.
                        let after = part.get(name.len()..).unwrap_or("").trim_start();
                        if name.is_empty()
                            || !(after.is_empty()
                                || after.starts_with('=')
                                || after.starts_with(';'))
                        {
                            break;
                        }
                        names.insert(name);
                    }
                }
            }

            let mut words = code.split_whitespace();
            match words.next() {
                Some("property") => {
                    // `property <type> <name>` and `property alias <name>`.
                    let _ty = words.next();
                    if let Some(name) = words.next() {
                        names.insert(name.trim_end_matches(':').to_owned());
                    }
                }
                Some("signal" | "function") => {
                    if let Some(name) = words.next() {
                        let name: String = name
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty() {
                            names.insert(name);
                        }
                    }
                    // Function parameters = JS locals too: `function
                    // zoomAt(target, viewX, viewY)`.
                    let params = code
                        .split_once('(')
                        .and_then(|(_, rest)| rest.split_once(')'))
                        .map(|(params, _)| params);
                    for param in params.unwrap_or_default().split(',') {
                        let param = param.trim();
                        if !param.is_empty() {
                            names.insert(param.to_owned());
                        }
                    }
                }
                Some("id:") => {
                    if let Some(name) = words.next() {
                        names.insert(name.to_owned());
                    }
                }
                _ => {}
            }
        }
    }
    names
}

#[test]
fn every_role_the_delegates_use_is_exposed_by_a_model() {
    // Delegates reference roles as bare identifiers: typo'd role renders
    // `undefined`, no failure, silently blanks part of UI. qmllint and load
    // test blind: role = runtime lookup.
    //
    // Check runs BOTH directions; each catches what other can't:
    //
    //   Rust side — every declared role must be referenced by some page, so
    //   renaming `needsConsent` in article.rs fails here even though the QML
    //   is untouched and still compiles.
    //
    //   QML side  — every bare camelCase word in a page must be a declared
    //   role, a name the QML itself declares, or one of a short list of Qt
    //   names, so mistyping `quoteDepth` at ONE of its four call sites fails
    //   here even though the role is still referenced elsewhere.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .to_path_buf();

    let roles = declared_roles(&root);
    assert!(!roles.is_empty(), "found no role_names() entries");

    // Scanner must see role read inside ternary, and not mistake property name
    // for one. Both directions: scanner reporting nothing would pass by doing
    // nothing.
    assert!(
        bare_identifiers(r#"text: visible ? styledText : """#).contains(&"styledText".to_owned()),
        "the scanner cannot see a role used in a ternary, so it guarantees nothing for one"
    );
    assert!(
        !bare_identifiers("    styledText: 1").contains(&"styledText".to_owned()),
        "the scanner counts a property NAME as a role read"
    );

    let mut files = Vec::new();
    qml_files(&root.join("qml"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no QML found");

    let qml_declared = qml_declared_identifiers(&files);

    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut unknown: Vec<String> = Vec::new();

    for file in &files {
        let source = std::fs::read_to_string(file).expect("read qml");
        // One place role named in STRING: view's section property,
        // `section.property: searching ? "matchKind" : ""`. Typo = list
        // silently stops grouping.
        for (lineno, line) in source.lines().enumerate() {
            let Some((_, value)) = line.trim_start().split_once("section.property:") else {
                continue;
            };
            for name in value.split('"').skip(1).step_by(2) {
                if name.is_empty() {
                    continue;
                }
                if roles.contains(name) {
                    used.insert(name.to_owned());
                } else {
                    unknown.push(format!(
                        "{}:{} — `{name}` is a section property but no model exposes it",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        lineno + 1
                    ));
                }
            }
        }
        // Comments prose, strings text: naming role while explaining code isn't
        // reference. Blanked, not removed: reported line numbers stay real.
        let source = strip_strings_and_comments(&source);
        for (lineno, line) in source.lines().enumerate() {
            for word in bare_identifiers(line) {
                if roles.contains(&word) {
                    used.insert(word);
                    continue;
                }
                if !looks_like_a_role(&word)
                    || qml_declared.contains(&word)
                    || QML_VALUE_NAMES.contains(&word.as_str())
                {
                    continue;
                }
                unknown.push(format!(
                    "{}:{} — `{word}` is used as a bare value but no model exposes it",
                    file.strip_prefix(&root).unwrap_or(file).display(),
                    lineno + 1
                ));
            }
        }
    }

    assert!(
        unknown.is_empty(),
        "QML reads names no model exposes. A role that does not exist evaluates \
         to `undefined` at runtime, so the binding silently renders nothing.\n\n{}\n\n\
         If one of these is a Qt or Silica name rather than a typo, add it to \
         QML_VALUE_NAMES with a reason.",
        unknown.join("\n")
    );

    let unused: Vec<&String> = roles
        .iter()
        .filter(|r| !used.contains(*r) && !ROLES_QML_DOES_NOT_USE.contains(&r.as_str()))
        .collect();
    assert!(
        unused.is_empty(),
        "these roles are declared in Rust but no QML page reads them: {unused:?}\n\n\
         Either the role was renamed and the QML still asks for the old name — \
         which renders as `undefined` on the device — or the role is genuinely \
         unused and belongs in ROLES_QML_DOES_NOT_USE with a reason."
    );
}
