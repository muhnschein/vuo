import QtQuick 2.6
// Silica exposes this as a C++ enum (DeclarativeCover), the same way
// PageStatus.qml's is exposed -- and for the same reason it needs a stub: a
// `_forceAnimation:` binding on `cover.status === Cover.Active` IS evaluated
// when the cover is instantiated, so without this the QML load test trips on
// "Cover is not defined".
//
// The ORDER is Silica's; the numbers are this stub's own and are never seen on
// a device, where the real enum is in scope. Nothing in Vuo compares a cover
// status to an integer.
QtObject {
    enum Value {
        Inactive = 0,
        Activating = 1,
        Active = 2,
        Deactivating = 3
    }
}
