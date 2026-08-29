//! Load a QML file in a real engine and let it print what it finds.
//!
//! Exists so the UI can be inspected against the SailfishOS target's OWN
//! Qt 5.6 and OWN Silica, under qemu, with no phone. `docs/status.md` lists
//! "the shim is compiled against Qt 5.15, not Qt 5.6" and "the Silica stubs
//! are an approximation" as standing gaps; this closes enough of them to
//! answer questions the stubs cannot, such as what a control's `checked`
//! actually is when the page first appears.
//!
//! See docs/sdk-build.md for the invocation. `QT_LOGGING_TO_CONSOLE=1` is
//! required: Sailfish's Qt sends qDebug to journald otherwise, and the
//! console.log output simply vanishes.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: qml-probe <file.qml>");
        std::process::exit(2);
    };
    vuo_shim::register_qml_types();
    let mut engine = qmetaobject::QmlEngine::new();
    engine.load_file(qmetaobject::QString::from(path));
}
