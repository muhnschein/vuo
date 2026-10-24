import QtQuick 2.6
// Enum companion of Format singleton. QML `enum`, same reason as
// PageStatus.qml: property names can't start upper-case. Values placeholders:
// Vuo only needs names to resolve.
QtObject {
    enum Value {
        Timepoint = 0,
        TimepointRelative = 1,
        TimeValue = 2,
        TimeValueTwentyFourHours = 3,
        DateMedium = 4,
        DateLong = 5,
        DateFull = 6,
        DurationElapsed = 7,
        DurationElapsedShort = 8
    }
}
