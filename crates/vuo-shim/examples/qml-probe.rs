//! Load QML file in real engine; let it print what it finds.
//!
//! Inspects UI against `SailfishOS` target's OWN Qt 5.6 and OWN Silica under
//! qemu, no phone. Answers what stubs can't (e.g. control's `checked` when page
//! first appears); see `docs/status.md` for standing gaps.
//!
//! Invocation: docs/sdk-build.md. `QT_LOGGING_TO_CONSOLE=1` required:
//! Sailfish's Qt sends qDebug to journald otherwise; console.log vanishes.

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: qml-probe <file.qml>");
        std::process::exit(2);
    };
    // Build app context from $HOME before loading so models read a mirror.
    //
    // Without: three empty lists, can't tell "roles wrong" from "no data".
    // Opt-in: probe of first-appearance geometry wants no worker thread.
    if std::env::var_os("VUO_PROBE_CONTEXT").is_some() {
        match vuo_shim::context::refresh_current() {
            Some(_) => eprintln!("qml-probe: context built from $HOME"),
            None => eprintln!("qml-probe: NO context -- is there an account.json?"),
        }
    }
    vuo_shim::register_qml_types();
    let mut engine = qmetaobject::QmlEngine::new();
    engine.load_file(qmetaobject::QString::from(path));

    // `Component.onCompleted` runs during load: first-appearance probe needs
    // nothing more. Timer- or page-stack-driven (navigation sequence) needs
    // event loop; QML then calls Qt.quit(). Opt-in: common case exits on own.
    if std::env::var_os("VUO_PROBE_EXEC").is_some() {
        engine.exec();
    }
}
