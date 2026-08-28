#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use qmetaobject::*;

#[test]
fn probe_entrylistpage_initial_properties() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    vuo_shim::register_qml_types();
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(
        root.join("qml-stubs").to_string_lossy().to_string(),
    ));
    engine.add_import_path(QString::from(
        root.join("qml").to_string_lossy().to_string(),
    ));

    let probe = root.join("qml").join("zz_probe.qml");
    std::fs::write(
        &probe,
        r#"
import QtQuick 2.6
import Sailfish.Silica 1.0
import "pages"
Item {
    Component { id: c; EntryListPage {} }
    Component.onCompleted: {
        var o = c.createObject(null, { title: "Feed name", feedId: 42 });
        console.warn("PROBE created =", o);
        if (o) {
            console.warn("PROBE title =", o.title);
            console.warn("PROBE model =", o.model);
            console.warn("PROBE feedId =", o.feedId);
        }
    }
}
"#,
    )
    .unwrap();
    engine.load_file(QString::from(probe.to_string_lossy().to_string()));
    let _ = std::fs::remove_file(&probe);
}
