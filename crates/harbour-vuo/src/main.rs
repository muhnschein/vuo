//! The Vuo application binary.
//!
//! Two entry paths, selected by the `sailfishapp` feature:
//!
//! - **Device (`--features sailfishapp`).** Uses `SailfishApp::application()`
//!   and `SailfishApp::createView()`. This matters: `QmlEngine::new()` builds a
//!   **QtWidgets** `QApplication`, and a Silica app needs Sailfish's own
//!   application object for correct Wayland surfaces, the booster, and theme
//!   setup.
//!
//! - **Desktop (default).** A plain `QmlEngine` pointed at the Silica stubs.
//!   For bring-up and debugging on a laptop, so that a QML change can be seen
//!   without a phone. Not shippable and not meant to be: the stubs are
//!   geometry-free placeholders.
//!
//! Either way the binary is thin on purpose. It registers types, opens the
//! mirror, loads QML, and runs the event loop. Everything else is in
//! `vuo-core`, where it can be tested without Qt.

// Only the desktop harness needs these (`QmlEngine`, `QString`). The device
// build's `run` is the cpp! block, which reaches Qt through C++ headers rather
// than through qmetaobject's Rust types -- so under `sailfishapp` this import
// is unused and warns.
#[cfg(not(feature = "sailfishapp"))]
use qmetaobject::*;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VUO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,vuo_core=info")),
        )
        .init();

    vuo_shim::register_qml_types();

    // Install the shared context before any QML loads: QML constructs the
    // models itself, so they resolve their database and worker through the
    // Qt-thread global rather than through a constructor.
    if let Err(e) = install_context() {
        // Not fatal. With no account configured yet there is nothing to open,
        // and the UI must still start so the user can reach Settings.
        tracing::info!(error = %e, "starting without a configured account");
    }

    run();
}

/// Open the mirror, start the worker, and install the app context.
///
/// The whole body of this used to live here, in a crate with no tests that
/// most of `make check` does not even build. It now lives in `vuo-shim`, where
/// it is tested — and, more importantly, where the settings screen can call it
/// again. Building the context exactly once, here, meant that on a first run
/// (no account file, so this fails) nothing ever retried it, and every
/// worker-backed action in the running app silently did nothing until Vuo was
/// restarted. See `vuo_shim::context::refresh`.
fn install_context() -> vuo_core::Result<()> {
    let paths = vuo_shim::worker::AppPaths::resolve().ok_or_else(|| {
        vuo_core::Error::Config("could not resolve the data directory".to_owned())
    })?;
    vuo_shim::context::refresh(&paths)?;
    Ok(())
}

#[cfg(feature = "sailfishapp")]
fn run() {
    // From `cpp`, not `qmetaobject`: qmetaobject depends on the crate but
    // its `use cpp::{cpp, cpp_class}` is private, so `qmetaobject::cpp` does
    // not resolve. This line was `use qmetaobject::cpp;` and could never have
    // compiled -- the first thing a real device build finds once it gets past
    // the toolchain.
    use cpp::cpp;

    cpp! {{
        #include <sailfishapp.h>
        #include <QtQuick>
        #include <QGuiApplication>
        #include <QQuickView>
        #include <QLocale>
        #include <QTranslator>
    }}

    // SailfishApp::main() does the whole dance -- application object, view,
    // source path resolution against /usr/share/<pkg>/ -- and is what every
    // Silica app uses.
    unsafe {
        cpp!([] {
            int argc = 1;
            char name[] = "harbour-vuo";
            char *argv[] = { name, nullptr };
            QGuiApplication *app = SailfishApp::application(argc, argv);

            // Load the UI translation for the device's locale.
            //
            // Without this, `qsTr` returns its source string -- English --
            // while Silica's own Format/Formatter follow the system locale.
            // Reported from a German device as a metadata line reading
            // "Tagesschau | vor 6 Stunden | 2 min read": half translated by
            // Silica, half not translated at all, in one sentence.
            //
            // Leaked on purpose: it must outlive `run()`, which never returns
            // until the app is quitting, and a QTranslator removed while the
            // UI is live would blank every translated string. `-` is the
            // separator in `harbour-vuo-de.qm`.
            QTranslator *translator = new QTranslator(app);
            if (translator->load(QLocale(), QStringLiteral("harbour-vuo"),
                                 QStringLiteral("-"),
                                 SailfishApp::pathTo(QStringLiteral("translations"))
                                     .toLocalFile())) {
                app->installTranslator(translator);
            }

            QQuickView *view = SailfishApp::createView();
            view->setSource(SailfishApp::pathTo("qml/harbour-vuo.qml"));
            view->show();
            app->exec();
        });
    }
}

#[cfg(not(feature = "sailfishapp"))]
fn run() {
    let root = std::env::var("VUO_QML_ROOT").unwrap_or_else(|_| "qml".to_owned());
    let stubs = std::env::var("VUO_QML_STUBS").unwrap_or_else(|_| "qml-stubs".to_owned());

    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(stubs));
    engine.add_import_path(QString::from(root.clone()));
    engine.load_file(QString::from(format!("{root}/harbour-vuo.qml")));
    engine.exec();
}
