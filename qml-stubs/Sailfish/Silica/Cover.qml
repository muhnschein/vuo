import QtQuick 2.6
// Silica C++ enum (DeclarativeCover), exposed like PageStatus.qml's; stub
// needed same reason: `_forceAnimation:` binding on `cover.status ===
// Cover.Active` IS evaluated on cover instantiation. Without it load test trips
// on "Cover is not defined".
//
// ORDER is Silica's; numbers are stub's own, never seen on device (real enum in
// scope there). Nothing in Vuo compares cover status to integer.
QtObject {
    enum Value {
        Inactive = 0,
        Activating = 1,
        Active = 2,
        Deactivating = 3
    }
}
