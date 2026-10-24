import QtQuick 2.6
// Silica C++ enum, like PageStatus. `Push`, `Replace`, `Pop` = Dialog's
// `acceptDestinationAction`; `Animated`, `Immediate` = operation type for
// `pageStack.push`/`replace`.
//
// Stub needed: `acceptDestinationAction: PageStackAction.Replace` is plain
// binding, evaluated on dialog instantiation; without it load test trips on
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
