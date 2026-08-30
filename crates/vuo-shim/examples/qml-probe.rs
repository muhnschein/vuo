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
    // Build the app context from $HOME before loading, so the models can
    // actually read a mirror.
    //
    // Without this a probe sees three empty lists and cannot tell "the roles
    // are wrong" from "there is no data" -- which is exactly the confusion
    // that let an entry row ship with no feed name on it. Opt-in, because a
    // probe of a page's first-appearance geometry wants no worker thread.
    if std::env::var_os("VUO_PROBE_CONTEXT").is_some() {
        match vuo_shim::context::refresh_current() {
            Some(_) => eprintln!("qml-probe: context built from $HOME"),
            None => eprintln!("qml-probe: NO context -- is there an account.json?"),
        }
    }
    vuo_shim::register_qml_types();
    let mut engine = qmetaobject::QmlEngine::new();
    engine.load_file(qmetaobject::QString::from(path));

    // `Component.onCompleted` runs during the load, so a probe that only wants
    // to inspect a page as it first appears needs nothing more. Anything
    // driven by a Timer or by the page stack -- a navigation sequence, say --
    // needs the event loop, and the QML is then responsible for calling
    // Qt.quit(). Opt in, so the common case still exits on its own.
    if std::env::var_os("VUO_PROBE_EXEC").is_some() {
        engine.exec();
    }
}
