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
            let name = name.trim();
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

#[test]
fn every_role_the_delegates_use_is_exposed_by_a_model() {
    // Delegates reference roles as bare identifiers, so a typo'd role silently
    // renders as `undefined` rather than failing.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .to_path_buf();

    let roles = declared_roles(&root);
    assert!(!roles.is_empty(), "found no role_names() entries");

    // The roles the delegates actually rely on. Kept explicit so that removing
    // a role from Rust fails here rather than silently blanking the UI.
    let used = [
        "entryId",
        "feedId",
        "title",
        "author",
        "unread",
        "starred",
        "readingTime",
        "errorMessage",
        "blockKind",
        "styledText",
        "level",
        "quoteDepth",
        "ordered",
        "marker",
        "indent",
        "imageSource",
        "imageAlt",
        "needsConsent",
    ];
    let missing: Vec<&str> = used
        .iter()
        .copied()
        .filter(|r| !roles.contains(*r))
        .collect();
    assert!(
        missing.is_empty(),
        "delegates use roles no model exposes: {missing:?}"
    );
}
