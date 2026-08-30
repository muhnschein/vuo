import QtQuick 2.6
// Silica exposes this as a C++ enum (DeclarativePageStatus). Same reasoning as
// Orientation.qml: QML property names may not begin with an upper-case letter,
// so the stub uses a QML `enum`.
//
// A stub is needed because a `running:` binding on `page.status === PageStatus.Active`
// IS evaluated when a page is instantiated -- unlike an `onStatusChanged` handler,
// which is why the existing use in EntryListPage got away without one. Without
// this the QML load test trips on "PageStatus is not defined".
QtObject {
    enum Value {
        Inactive = 0,
        Activating = 1,
        Active = 2,
        Deactivating = 3
    }
}
