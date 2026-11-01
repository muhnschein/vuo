//! Vuo application binary.
//!
//! Two entry paths, by `sailfishapp` feature:
//!
//! - **Device (`--features sailfishapp`).** `SailfishApp::application()` and
//!   `SailfishApp::createView()`. `QmlEngine::new()` builds a **`QtWidgets`**
//!   `QApplication`; Silica app needs Sailfish's application object for Wayland
//!   surfaces, booster, theme.
//!
//! - **Desktop (default).** Plain `QmlEngine` on Silica stubs. Bring-up and
//!   debugging on laptop. Not shippable: stubs are geometry-free placeholders.
//!
//! Binary thin on purpose: register types, open mirror, load QML, run event
//! loop. Rest in `vuo-core`, testable without Qt.

// Shipped code held to more than tests; vuo-core's lib.rs says why.
#![cfg_attr(
    not(test),
    deny(
        clippy::string_slice,
        clippy::arithmetic_side_effects,
        clippy::let_underscore_must_use,
        clippy::panic_in_result_fn,
        clippy::unwrap_in_result,
        clippy::get_unwrap
    )
)]

// Desktop harness only. Device `run` is cpp! block (C++ headers, not
// qmetaobject's Rust types): import unused under `sailfishapp`.
#[cfg(not(feature = "sailfishapp"))]
use qmetaobject::{QString, QmlEngine};

fn main() {
    // `vuo_shim=info` in default on purpose, not only under `VUO_LOG`.
    //
    // Shim logs a handful of lines per session: mirror open, sync worker start,
    // context install. Device process dying on signal prints nothing (stripped
    // binary), so last line reached is all evidence; user reproducing crash
    // shouldn't need an env var.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VUO_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "warn,vuo_core=info,vuo_shim=info,harbour_vuo=info",
                )
            }),
        )
        .init();

    vuo_shim::register_qml_types();

    // Sync worker thread first, account or not.
    //
    // Not optimisation: creating it later (from QML tap saving first account)
    // kills process on Jolla Phone 2026, no panic, no diagnostic. Started here,
    // while process is few threads and Wayland/GPU not loaded: parked thread,
    // account sent by channel later. See `vuo_shim::context::start_worker`.
    vuo_shim::context::start_worker();

    // Install shared context before QML loads: QML constructs models itself;
    // they find database and worker via Qt-thread global, not constructor.
    if let Err(e) = install_context() {
        // Not fatal: no account yet = nothing to open; UI must still start for
        // Settings.
        tracing::info!(error = %e, "starting without a configured account");
    }

    run();
}

/// Open mirror, start worker, install app context.
///
/// Body lives in `vuo-shim` (tested; settings screen calls it again after first
/// account save). See `vuo_shim::context::refresh`.
fn install_context() -> vuo_core::Result<()> {
    let paths = vuo_shim::worker::AppPaths::resolve().ok_or_else(|| {
        vuo_core::Error::Config("could not resolve the data directory".to_owned())
    })?;
    vuo_shim::context::refresh(&paths)?;
    Ok(())
}

#[cfg(feature = "sailfishapp")]
fn run() {
    // From `cpp`, not `qmetaobject`: qmetaobject's `use cpp::{cpp, cpp_class}`
    // is private, `qmetaobject::cpp` doesn't resolve.
    use cpp::cpp;

    cpp! {{
        #include <sailfishapp.h>
        #include <QtQuick>
        #include <QGuiApplication>
        #include <QQuickView>
        #include <QLocale>
        #include <QTranslator>
    }}

    // SailfishApp::main() does it all (app object, view, source path against
    // /usr/share/<pkg>/); every Silica app uses it.
    unsafe {
        cpp!([] {
            int argc = 1;
            char name[] = "harbour-vuo";
            char *argv[] = { name, nullptr };
            QGuiApplication *app = SailfishApp::application(argc, argv);

            // Load UI translation for device locale.
            //
            // Without it `qsTr` returns English source while Silica's
            // Format/Formatter follow system locale: half-translated lines.
            //
            // Leaked on purpose: must outlive `run()` (returns only at quit);
            // removing QTranslator while UI live blanks every translated
            // string. `-` = separator in `harbour-vuo-de.qm`.
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
