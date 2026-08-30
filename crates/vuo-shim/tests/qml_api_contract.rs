//! Every member the QML calls must exist on a Rust `QObject`.
//!
//! # Why this test exists
//!
//! The QML load test compiles every page, which catches unknown *types* and
//! undeclared *properties*. It does not catch a call to a method that does not
//! exist: QML resolves method calls at **runtime**, so
//! `model.setRedd(index, true)` compiles perfectly and fails on the device the
//! moment a user taps.
//!
//! That is not a hypothetical. The first version of this UI called nine
//! methods -- `setRead`, `setStarred`, `markAllRead`, `markFeedRead`,
//! `subscribe`, `unsubscribe`, `load`, `fetchOriginal`, `allowImagesFrom` --
//! that had never been implemented, and every check in the build passed.
//!
//! # How it works
//!
//! `qmetaobject` emits names to the meta-object verbatim, so the Rust source
//! *is* the QML API. This scans the `qt_property!`/`qt_method!`/`qt_signal!`
//! declarations for the set of names QML may use, then scans the QML for
//! members accessed on a Vuo model, and reports anything not declared.
//!
//! It deliberately checks the union of members across all Vuo types rather
//! than per-type: QML passes models around through `property var`, so the
//! receiver's type is not always recoverable from the source. The union still
//! catches every typo and every unimplemented method, which is the failure
//! mode that matters.

// Test code: the panic denials guard foreign-input paths in production, not
// assertions in tests. See the note in vuo-core's lib.rs.
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
    for file in ["models.rs", "article.rs", "settings.rs"] {
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
            // Strip a visibility modifier: `pub blockedImages: qt_property!(...)`
            // declares the member `blockedImages`, not `pub blockedImages`.
            // Without this the member silently fails to register and the test
            // reports the QML that uses it as calling something undeclared --
            // pointing at the wrong file entirely.
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
            // A `qt_signal!` also gives QML an `onName` handler.
            let mut chars = name.chars();
            if let Some(first) = chars.next() {
                names.insert(format!("on{}{}", first.to_uppercase(), chars.as_str()));
            }
            // A NOTIFY signal named in a property declaration is usable too.
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

/// The tab strip's titles and its scope kinds must stay the same length.
///
/// They are two parallel arrays in EntryListPage.qml -- `titles` on the strip
/// and `scopeTabKinds` on the page -- and the strip maps one to the other by
/// index. Adding a fourth title without a fourth kind gives a tab whose tap
/// resolves `scopeTabKinds[3]` to `undefined`, which `setScope` would then
/// receive as 0: the tab would silently show Unread. Nothing else in the build
/// would notice, because it is a runtime array lookup.
#[test]
fn every_scope_tab_has_a_scope_kind() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root");
    let source = std::fs::read_to_string(root.join("qml/pages/EntryListPage.qml"))
        .expect("EntryListPage.qml");

    let array_len = |needle: &str| -> usize {
        let line = source
            .lines()
            .find(|l| l.trim_start().starts_with(needle))
            .unwrap_or_else(|| panic!("no line starting `{needle}` in EntryListPage.qml"));
        let open = line.find('[').expect("an array literal");
        let close = line[open..].find(']').expect("a closed array literal") + open;
        let inner = line[open + 1..close].trim();
        if inner.is_empty() {
            0
        } else {
            inner.split(',').count()
        }
    };

    let kinds = array_len("property var scopeTabKinds:");
    let titles = array_len("titles:");
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

/// Role names exposed through `role_names()`, which delegates see as
/// context properties rather than as members of the model.
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
        } else if path.extension().and_then(|e| e.to_str()) == Some("qml") {
            out.push(path);
        }
    }
}

#[test]
fn every_member_the_qml_uses_is_implemented_in_rust() {
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

    // Identifiers that hold a Vuo model in the QML. `model` and `feedModel`
    // are the `property var` names pages receive them through.
    const RECEIVERS: &[&str] = &[
        "entries",
        "feeds",
        "article",
        "settings",
        "model",
        "feedModel",
    ];

    let mut files = Vec::new();
    qml_files(&root.join("qml"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no QML found");

    let mut missing: Vec<String> = Vec::new();

    for file in &files {
        let source = std::fs::read_to_string(file).expect("read qml");
        for (lineno, line) in source.lines().enumerate() {
            // Skip comments so a mention in prose is not treated as a call.
            let code = line.split("//").next().unwrap_or("");
            for receiver in RECEIVERS {
                let needle = format!("{receiver}.");
                let mut from = 0usize;
                while let Some(idx) = code.get(from..).and_then(|s| s.find(&needle)) {
                    let at = from + idx;
                    // Require a non-identifier character before the receiver,
                    // so `page.model.` matches on `model` but `xmodel.` does not.
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
                    // Qt built-ins available on every QObject.
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

/// Roles the Rust models expose that no QML page reads.
///
/// The check below is that every *other* declared role is referenced, so
/// renaming or dropping a role the UI depends on fails here. Adding a name to
/// this list is therefore a deliberate statement that the UI does not use it,
/// not a way to quiet the test.
const ROLES_QML_DOES_NOT_USE: &[&str] = &[
    // The article's plain-text rendering, kept for a share/copy action that
    // does not exist yet.
    "plainText",
    // The transform records a code block's language; the UI renders code
    // blocks unhighlighted, so nothing reads it.
    "codeLanguage",
    // List numbering comes from the pre-rendered `marker`.
    "ordered",
    // Feed browsing is by feed; categories are stored but not yet a UI axis.
    "categoryId",
    // The list shows `readingTime` and `unread`, not a timestamp; and the
    // article URL reaches the browser through `article.openInBrowser()`,
    // which returns it, rather than through the role.
    "published",
    "url",
];

/// QML and Silica names that legitimately appear as bare camelCase words in a
/// value position. Everything else bare and camelCase inside a page has to be
/// a model role.
///
/// This list is short on purpose. An earlier version of this test carried
/// around sixty names, and because it had swallowed real role and method
/// vocabulary (`entryId`, `unreadCount`, `setRead`, ...) it could no longer
/// see a typo in any of them.
const QML_VALUE_NAMES: &[&str] = &[
    "qsTr",
    "pageStack",
    "remorseAction",
    "currentIndex",
    "defaultAllowedOrientations",
    "implicitHeight",
    // The pair of it. Both are Item's own, and the article view reads them off
    // a loaded Image to get its true aspect ratio.
    "implicitWidth",
    "sourceSize",
    "textFormat",
];

/// Does this word have the shape of one of our role names?
fn looks_like_a_role(word: &str) -> bool {
    word.len() >= 4
        && word.chars().next().is_some_and(char::is_lowercase)
        && word.chars().any(char::is_uppercase)
}

/// The bare identifiers on one line of QML.
///
/// Comments and string literals are removed; a member access (`model.title`)
/// and a property being set (`textFormat:`) are both excluded. Member accesses
/// are the other test's job, and a name on the left of a colon is a property
/// this QML declares rather than a value it reads.
fn bare_identifiers(line: &str) -> Vec<String> {
    let code = line.split("//").next().unwrap_or("");

    // A role name mentioned inside a translated string is not a reference.
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
        let is_binding = chars.get(j) == Some(&':');
        if !preceded_by_dot && !is_binding {
            out.push(chars[start..i].iter().collect());
        }
    }
    out
}

/// Names the QML itself introduces: `property var x`, `id: x`, `signal x`,
/// `function x()`. These are legitimately used bare and are not roles.
fn qml_declared_identifiers(files: &[PathBuf]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in files {
        let source = std::fs::read_to_string(file).expect("read qml");
        for line in source.lines() {
            let code = line.split("//").next().unwrap_or("").trim();
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
    // Delegates reference roles as bare identifiers, so a typo'd role renders
    // as `undefined` rather than failing — silently blanking part of the UI.
    // Neither qmllint nor the QML load test can see it: a role is a runtime
    // lookup.
    //
    // The check runs in BOTH directions, because each catches a different
    // mistake and neither catches the other's:
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

    let mut files = Vec::new();
    qml_files(&root.join("qml"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no QML found");

    let qml_declared = qml_declared_identifiers(&files);

    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut unknown: Vec<String> = Vec::new();

    for file in &files {
        let source = std::fs::read_to_string(file).expect("read qml");
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
