import QtQuick 2.6
// Silica C++ enum. Property names can't start upper-case, so QML `enum` (Qt
// 5.10+). Fine: stubs host-only, never shipped to Qt 5.6 device.
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
