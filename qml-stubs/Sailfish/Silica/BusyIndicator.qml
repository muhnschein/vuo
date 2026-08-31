import QtQuick 2.6
// `_forceAnimation` is Silica's own escape hatch for a BusyIndicator drawn
// while the app is not the active window -- see BusyIndicator.qml:80.
Item { property bool running: false; property int size: 0; property bool _forceAnimation: false }
