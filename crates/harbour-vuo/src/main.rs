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

use qmetaobject::*;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VUO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,vuo_core=info")),
        )
        .init();

    // The systemd user timer starts the same binary with --sync-once rather
    // than shipping a second program: §5 says background refresh shares
    // vuo-core rather than reimplementing sync in a script, and sharing the
    // binary is the strongest form of that.
    if std::env::args().any(|a| a == "--sync-once") {
        std::process::exit(sync_once());
    }

    vuo_shim::register_qml_types();
    run();
}

/// Run one sync pass headlessly, then exit.
///
/// Exit code 75 (EX_TEMPFAIL) for a transient failure, so the unit can treat
/// "the phone had no signal" as success rather than as a fault worth
/// restarting and logging about.
fn sync_once() -> i32 {
    let Some(paths) = vuo_shim::worker::AppPaths::from_env() else {
        eprintln!("vuo: not configured yet; nothing to sync");
        return 0;
    };
    match vuo_shim::worker::sync_once_blocking(&paths) {
        Ok(report) => {
            tracing::info!(
                upserted = report.pull.upserted,
                deleted = report.entries_deleted,
                "background sync finished"
            );
            0
        }
        Err(e) if e.is_transient() => {
            tracing::info!(error = %e, "background sync deferred");
            75
        }
        Err(e) => {
            tracing::warn!(error = %e, "background sync failed");
            1
        }
    }
}

#[cfg(feature = "sailfishapp")]
fn run() {
    use qmetaobject::cpp;

    cpp! {{
        #include <sailfishapp.h>
        #include <QtQuick>
        #include <QGuiApplication>
        #include <QQuickView>
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
