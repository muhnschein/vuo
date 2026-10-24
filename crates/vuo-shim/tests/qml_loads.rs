//! Load every QML file in real QML engine, headless.
//!
//! §8.1 asks for Silica stubs so QML lints on CI without `SailfishOS` SDK. Qt 5
//! `qmllint` checks *syntax* only: passes nonexistent types and undeclared
//! properties (most real QML mistakes).
//!
//! This builds real `QQmlEngine`, points at `qml-stubs/` for `Sailfish.Silica`,
//! registers Vuo types, compiles every page AND INSTANTIATES IT.
//!
//! Instantiating matters: QML resolves type and property *names* at compile
//! time, binding *values* only on creation. Compiling alone passes
//!
//!     textFormat: Text.Plaintext        // PlainText, note the capital T
//!
//! = §9.3 defence silently off: binding → `undefined`, Qt falls back to
//! `Text.AutoText`, remote operator's feed title interpreted as rich text.
//!
//! Qt reports that as runtime warning, not error; `qmetaobject` exposes no
//! message handler. So each file loads in CHILD PROCESS (this test binary
//! re-executed), parent reads its stderr. Also sidesteps
//! one-`QApplication`-per-process.
//!
//! Still blind to delegate bindings: delegates instantiate per row, pages
//! created with empty models, so bad role inside one never evaluated here.
//! `qml_api_contract.rs` covers that statically against Rust models.
//!
//! Run under `QT_QPA_PLATFORM=offscreen`; `make check` sets it.

// Test code: see note in vuo-core's lib.rs. unwrap/panic denials guard
// production foreign-input paths, not test assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
use qmetaobject::{CompilationMode, ComponentStatus, QString, QUrl, QmlComponent, QmlEngine};

/// Set on re-executed child, naming the one file to load.
const CHILD_VAR: &str = "VUO_QML_LOAD_ONE";

/// What child prints once file instantiated; parent reads this INSTEAD OF exit
/// code.
///
/// Different claims. Qt reads images on own thread; `exit` mid-read runs Qt
/// static destructors under it: "`QMutex`: destroying locked mutex", then
/// crash, after real work finished. That crash says nothing about QML, but as
/// exit code looks like failed load (seen on CI runners with second `Image` in
/// TextArt.qml, never locally). Child announces success when it has it;
/// teardown not test's business. Crash BEFORE this line still fails (case worth
/// catching).
const INSTANTIATED: &str = "VUO-QML-INSTANTIATED";

/// Qt diagnostics meaning binding didn't do what source says.
///
/// Qt logs at warning level and continues; nothing fails without this list.
/// Matched as stderr substrings.
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
        .and_then(std::path::Path::parent)
        .expect("repository root")
        .to_path_buf()
}

/// Load and INSTANTIATE one file, exit. Child only.
fn load_one_and_exit(path: &str) -> ! {
    let root = repo_root();
    vuo_shim::register_qml_types();

    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        root.join("qml-stubs").to_string_lossy().to_string(),
    ));
    // So `import "pages"`-style relative resolution behaves as on device
    // (everything under one datadir).
    engine.add_import_path(QString::from(
        root.join("qml").to_string_lossy().to_string(),
    ));

    let mut component = QmlComponent::new(&engine);
    component.load_url(
        QUrl::from(QString::from(format!("file://{path}"))),
        CompilationMode::PreferSynchronous,
    );
    if component.status() == ComponentStatus::Error {
        // Re-load through engine only so Qt prints diagnostics; binding doesn't
        // expose `QQmlComponent::errors()`.
        eprintln!("COMPILE-ERROR");
        engine.load_file(QString::from(path));
        std::process::exit(2);
    }
    // Child's point: evaluate every binding.
    let _ = component.create();
    // Newline flushes (Rust stdout line-buffered): reaches pipe before
    // teardown. Stdout on purpose: parent reads child's stdout for it; Qt
    // writes stderr.
    #[allow(clippy::print_stdout)]
    {
        println!("{INSTANTIATED}");
    }
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
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains(INSTANTIATED) {
            eprintln!("--- QML errors in {relative} ---\n{stderr}");
            failures.push(format!("{relative}: failed to compile or instantiate"));
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

/// Every `Qt.resolvedUrl("literal.qml")` names existing file.
///
/// Inside `onClicked` handlers: arbitrary JS, not resolved at compile time nor
/// by instantiating page. Typo = dead button on device. Handful in app: literal
/// check costs nothing.
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
                // Exact case: QML resolves imports on case-sensitive filesystem.
                if std::path::Path::new(target)
                    .extension()
                    .is_none_or(|e| e != "qml")
                {
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

/// No size or text in article body gated on `visible`.
///
/// `visible` = EFFECTIVE visibility, false for every item on page stack hid;
/// article hidden whenever image viewer pushed over it. Block `height` reading
/// it collapses, list loses everything below header, reader returns to article
/// top. Block kind says same and doesn't change while page hidden.
#[test]
fn the_article_body_keeps_its_shape_while_the_page_is_hidden() {
    let path = repo_root().join("qml/pages/ArticlePage.qml");
    let source = std::fs::read_to_string(&path).expect("read ArticlePage.qml");
    let offending: Vec<String> = source
        .lines()
        .enumerate()
        .filter_map(|(lineno, line)| {
            let code = line.split("//").next().unwrap_or("").trim();
            let gated = (code.starts_with("height:") || code.starts_with("text:"))
                && code.contains("visible ?");
            gated.then(|| format!("ArticlePage.qml:{} — {code}", lineno + 1))
        })
        .collect();
    assert!(
        offending.is_empty(),
        "these collapse while another page is over the article:\n{}",
        offending.join("\n")
    );
}

/// Every texture QML names exists and is coverage mask.
///
/// Two ways texture goes wrong on device, invisible elsewhere:
///
///   - A `source:` that names no file. Qt reports a missing image as a warning
///     on a channel this test's parent does not read, and the page simply comes
///     up bare.
///   - A mask that is not GRAYSCALE. `components/TextArt.qml` samples the RED
///     channel for coverage, because that is where `scripts/png-mask.py` puts
///     it. Re-export one of these as RGBA -- which any image editor will do by
///     default, and which looks identical in a viewer, since the art is white
///     with an alpha channel -- and every pixel's red is 1.0: the shader would
///     tint the whole surface solid.
#[test]
fn every_texture_the_qml_names_is_a_mask_that_exists() {
    let root = repo_root();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_qml(&root.join("qml"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no QML found");

    let mut checked = 0usize;
    let mut problems: Vec<String> = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file).expect("read qml");
        let dir = file.parent().expect("qml file has a parent");
        for (lineno, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let Some(idx) = code.find("source: \"") else {
                continue;
            };
            let after = &code[idx + "source: \"".len()..];
            let Some(end) = after.find('"') else { continue };
            let target = &after[..end];
            // `image://theme/...` = Silica's provider, not file.
            if std::path::Path::new(target)
                .extension()
                .is_none_or(|e| e != "png")
            {
                continue;
            }
            checked += 1;
            let path = dir.join(target);
            let where_ = format!(
                "{}:{}",
                file.strip_prefix(&root).unwrap_or(file).display(),
                lineno + 1
            );
            let Ok(bytes) = std::fs::read(&path) else {
                problems.push(format!("{where_} — `{target}` names no such file"));
                continue;
            };
            if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
                problems.push(format!("{where_} — `{target}` is not a PNG"));
                continue;
            }
            // IHDR always first: 8 bytes signature, 8 chunk header, then width,
            // height, bit depth, colour type.
            let depth = bytes.get(24).copied().unwrap_or(0);
            let colour = bytes.get(25).copied().unwrap_or(255);
            if (depth, colour) != (8, 0) {
                problems.push(format!(
                    "{where_} — `{target}` is bit depth {depth} colour type {colour}; \
                     the shader reads coverage from the red channel of an 8-bit \
                     grayscale mask, so regenerate it with `make textart`"
                ));
            }
        }
    }

    assert!(checked > 0, "found no textures to check");
    assert!(
        problems.is_empty(),
        "the texture would not draw on a device:\n{}",
        problems.join("\n")
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
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("qml") {
            out.push(path);
        }
    }
}
