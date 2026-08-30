import QtQuick 2.6
// The enum companion to the Format singleton. A QML `enum` for the same reason
// as PageStatus.qml: property names may not begin with an upper-case letter.
// The values are placeholders -- nothing in Vuo depends on them numerically,
// only on the names resolving.
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
