#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use qmetaobject::*;

#[test]
fn probe_loader_delegate_sees_model_roles() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().and_then(|p| p.parent()).unwrap().to_path_buf();
    let mut engine = QmlEngine::new();
    engine.add_import_path(QString::from(root.join("qml-stubs").to_string_lossy().to_string()));

    let probe = root.join("zz_probe_loader.qml");
    std::fs::write(&probe, r#"
import QtQuick 2.6
Item {
    ListModel {
        id: lm
        ListElement { styledText: "HELLO"; blockKind: "paragraph" }
    }
    Component {
        id: paragraphBlock
        Text {
            text: styledText
            Component.onCompleted: console.warn("PROBE outer-component text =", text)
        }
    }
    ListView {
        model: lm
        width: 100; height: 100
        delegate: Loader {
            sourceComponent: {
                switch (blockKind) {
                case "paragraph": return paragraphBlock
                default: return null
                }
            }
        }
    }
    // Control: component declared INSIDE the delegate
    ListView {
        model: lm
        width: 100; height: 100
        delegate: Loader {
            sourceComponent: Component {
                Text {
                    text: styledText
                    Component.onCompleted: console.warn("PROBE inline-component text =", text)
                }
            }
        }
    }
}
"#).unwrap();
    engine.load_file(QString::from(probe.to_string_lossy().to_string()));
    let _ = std::fs::remove_file(&probe);
}
