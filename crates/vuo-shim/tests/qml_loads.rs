//! Loading every QML file in a real QML engine, headlessly.
//!
//! §8.1 asks for Silica stubs so QML can be linted on a CI runner without the
//! SailfishOS SDK. Qt 5's `qmllint` only checks *syntax*, so on its own it
//! would pass a file that references a type that does not exist or sets a
//! property that was never declared — which is most of the mistakes actually
//! made in QML.
//!
//! This test goes further: it builds a real `QQmlEngine`, points it at
//! `qml-stubs/` for `Sailfish.Silica`, registers Vuo's own types, and compiles
//! every page. A missing type, a typo'd property or a malformed binding is a
//! compile error here, on a machine with no phone and no SDK.
//!
//! Run under `QT_QPA_PLATFORM=offscreen`; `make check` sets it.

use qmetaobject::*;

/// Everything is done in one test on purpose: `QmlEngine::new()` builds a
/// `QApplication`, and constructing a second one in the same process aborts.
#[test]
fn every_qml_file_compiles_against_the_silica_stubs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .to_path_buf();

    vuo_shim::register_qml_types();

    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        root.join("qml-stubs").to_string_lossy().to_string(),
    ));
    // So that `import "pages"`-style relative resolution behaves as it does on
    // a device, where everything lives under one datadir.
    engine.add_import_path(QString::from(root.join("qml").to_string_lossy().to_string()));

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_qml(&root.join("qml"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no QML found -- the test is not testing anything");

    let mut failures: Vec<String> = Vec::new();
    for file in &files {
        let url = QUrl::from(QString::from(format!("file://{}", file.display())));
        let mut component = QmlComponent::new(&engine);
        component.load_url(url, CompilationMode::PreferSynchronous);

        if component.status() == ComponentStatus::Error {
            let relative = file.strip_prefix(&root).unwrap_or(file);
            failures.push(relative.display().to_string());
            // `QmlComponent` does not expose `errors()` through this binding,
            // and a bare list of filenames is not enough to fix anything. The
            // engine's own loader prints Qt's diagnostics to stderr, so a
            // failing file is re-loaded through it purely for the message.
            eprintln!("--- QML errors in {} ---", relative.display());
            engine.load_file(QString::from(file.to_string_lossy().to_string()));
        }
    }

    assert!(
        failures.is_empty(),
        "these QML files failed to compile: {failures:#?}\n\
         (Qt printed the specific errors above.) If a Silica property is\n\
         missing from the stubs, add it to qml-stubs/ rather than working\n\
         around it in the app."
    );
}

fn collect_qml(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_qml(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("qml") {
            out.push(path);
        }
    }
}
