//! Loading every QML file in a real QML engine, headlessly.
//!
//! §8.1 asks for Silica stubs so QML can be linted on a CI runner without the
//! SailfishOS SDK. Qt 5's `qmllint` only checks *syntax*, so on its own it
//! would pass a file that references a type that does not exist or sets a
//! property that was never declared — which is most of the mistakes actually
//! made in QML.
//!
//! This test goes further: it builds a real `QQmlEngine`, points it at
//! `qml-stubs/` for `Sailfish.Silica`, registers Vuo's own types, compiles
//! every page AND INSTANTIATES IT.
//!
//! Instantiating is the half that was missing. QML resolves type names and
//! property *names* at compile time, but binding *values* only when an object
//! is actually created — so compiling alone passed
//!
//!     textFormat: Text.Plaintext        // PlainText, note the capital T
//!
//! which is a §9.3 defence silently turning itself off: the binding evaluates
//! to `undefined`, Qt falls back to `Text.AutoText`, and a feed title chosen by
//! a remote operator is then interpreted as rich text.
//!
//! Qt reports that as a runtime warning rather than an error, and the
//! `qmetaobject` binding exposes no message handler, so each file is loaded in
//! a CHILD PROCESS (this same test binary, re-executed) whose stderr the parent
//! reads. That also sidesteps `QmlEngine::new()` building a `QApplication`,
//! which may exist only once per process.
//!
//! What this still cannot see: a delegate's bindings. A delegate is
//! instantiated per row, and these pages are created with empty models, so a
//! nonexistent role inside one is never evaluated here. That case belongs to
//! `qml_api_contract.rs`, which reads the QML statically and checks every role
//! against the Rust models.
//!
//! Run under `QT_QPA_PLATFORM=offscreen`; `make check` sets it.

// Test code: see the note in vuo-core's lib.rs. The unwrap/panic denials
// guard foreign-input paths in production, not assertions in tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
use qmetaobject::*;

/// Set on the re-executed child to name the one file it should load.
const CHILD_VAR: &str = "VUO_QML_LOAD_ONE";

/// Qt diagnostics that mean a binding did not do what the source says.
///
/// Qt logs these at warning level and carries on, so nothing fails without
/// this list. They are matched as substrings of stderr.
const RUNTIME_FAILURES: &[&str] = &[
    "Unable to assign",
    "Cannot assign",
    "is not a function",
    "is not defined",
    "ReferenceError",
    "TypeError",
    "Unable to determine callable",
    "Binding loop detected",
];

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .to_path_buf()
}

/// Load and INSTANTIATE one file, then exit. Runs only in the child.
fn load_one_and_exit(path: &str) -> ! {
    let root = repo_root();
    vuo_shim::register_qml_types();

    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        root.join("qml-stubs").to_string_lossy().to_string(),
    ));
    // So that `import "pages"`-style relative resolution behaves as it does on
    // a device, where everything lives under one datadir.
    engine.add_import_path(QString::from(
        root.join("qml").to_string_lossy().to_string(),
    ));

    let mut component = QmlComponent::new(&engine);
    component.load_url(
        QUrl::from(QString::from(format!("file://{path}"))),
        CompilationMode::PreferSynchronous,
    );
    if component.status() == ComponentStatus::Error {
        // Re-load through the engine purely so Qt prints its diagnostics; the
        // binding does not expose `QQmlComponent::errors()`.
        eprintln!("COMPILE-ERROR");
        engine.load_file(QString::from(path));
        std::process::exit(2);
    }
    // The point of the child: evaluate every binding.
    let _ = component.create();
    std::process::exit(0);
}

#[test]
fn every_qml_file_compiles_and_instantiates_against_the_silica_stubs() {
    if let Ok(path) = std::env::var(CHILD_VAR) {
        load_one_and_exit(&path);
    }

    let root = repo_root();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_qml(&root.join("qml"), &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no QML found -- the test is not testing anything"
    );

    let exe = std::env::current_exe().expect("test binary path");
    let mut failures: Vec<String> = Vec::new();

    for file in &files {
        let relative = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        let output = std::process::Command::new(&exe)
            .args([
                "--exact",
                "every_qml_file_compiles_and_instantiates_against_the_silica_stubs",
                "--nocapture",
            ])
            .env(CHILD_VAR, file.to_string_lossy().to_string())
            .env("QT_QPA_PLATFORM", "offscreen")
            .output()
            .expect("re-exec the test binary");

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            eprintln!("--- QML errors in {relative} ---\n{stderr}");
            failures.push(format!("{relative}: failed to compile"));
            continue;
        }
        for line in stderr.lines() {
            if RUNTIME_FAILURES.iter().any(|needle| line.contains(needle)) {
                failures.push(format!("{relative}: {}", line.trim()));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "QML problems:\n{}\n\n\
         A compile failure usually means a missing Silica property: add it to\n\
         qml-stubs/ rather than working around it in the app. A runtime line\n\
         means a binding evaluated to something Qt could not use -- a typo'd\n\
         enum value, a missing method, an undefined identifier.",
        failures.join("\n")
    );
}

/// Every `Qt.resolvedUrl("literal.qml")` must name a file that exists.
///
/// These sit inside `onClicked` handlers, so they are arbitrary JS: not
/// resolved at compile time, and not reached by instantiating a page either.
/// A typo here is a dead button on the device. There are only a handful of
/// them in the whole app, so a literal check costs nothing.
#[test]
fn every_page_reference_resolves_to_a_file() {
    let root = repo_root();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_qml(&root.join("qml"), &mut files);
    assert!(!files.is_empty(), "no QML found");

    let mut checked = 0usize;
    let mut missing: Vec<String> = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).expect("read qml");
        let dir = file.parent().expect("qml file has a parent");
        for (lineno, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(idx) = rest.find("Qt.resolvedUrl(\"") {
                let after = &rest[idx + "Qt.resolvedUrl(\"".len()..];
                let Some(end) = after.find('"') else { break };
                let target = &after[..end];
                rest = &after[end..];
                if !target.ends_with(".qml") {
                    continue;
                }
                checked += 1;
                if !dir.join(target).exists() {
                    missing.push(format!(
                        "{}:{} — Qt.resolvedUrl(\"{target}\") names no such file",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        lineno + 1
                    ));
                }
            }
        }
    }

    assert!(checked > 0, "found no page references to check");
    assert!(
        missing.is_empty(),
        "these page references would be a dead button on the device:\n{}",
        missing.join("\n")
    );
}

fn collect_qml(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_qml(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("qml") {
            out.push(path);
        }
    }
}
