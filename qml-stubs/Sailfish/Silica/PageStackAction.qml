import QtQuick 2.6
// Silica exposes this as a C++ enum, like PageStatus. `Push`, `Replace` and
// `Pop` are what a Dialog's `acceptDestinationAction` takes; `Animated` and
// `Immediate` are the operation type `pageStack.push`/`replace` accept.
//
// A stub is needed because `acceptDestinationAction: PageStackAction.Replace`
// is a plain property binding, evaluated the moment the dialog is
// instantiated -- so without this the QML load test trips on
// "PageStackAction is not defined".
QtObject {
    enum Value {
        Animated = 0,
        Immediate = 1,
        Push = 2,
        Pop = 3,
        Replace = 4
    }
}
