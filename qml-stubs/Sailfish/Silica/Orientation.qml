import QtQuick 2.6
// Silica exposes this as a C++ enum. QML property names may not begin with an
// upper-case letter, so the stub uses a QML `enum` declaration instead --
// available since Qt 5.10, which is fine because stubs are host-only and are
// never shipped to a device running Qt 5.6.
QtObject {
    enum Value {
        None = 0,
        Portrait = 1,
        Landscape = 2,
        PortraitInverted = 4,
        LandscapeInverted = 8,
        PortraitMask = 5,
        LandscapeMask = 10,
        All = 15
    }
}
