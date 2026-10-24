import QtQuick 2.6
// Silica C++ enum (DeclarativePageStatus). Same as Orientation.qml: property names
// can't start upper-case, so QML `enum`.
//
// Stub needed: `running:` binding on `page.status === PageStatus.Active` IS evaluated
// on page instantiation (`onStatusChanged` handler isn't). Without it load test trips
// on "PageStatus is not defined".
QtObject {
    enum Value {
        Inactive = 0,
        Activating = 1,
        Active = 2,
        Deactivating = 3
    }
}
