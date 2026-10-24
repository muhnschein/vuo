import QtQuick 2.6
// `_forceAnimation` = Silica's escape hatch for BusyIndicator drawn while app
// not active window; see BusyIndicator.qml:80.
Item { property bool running: false; property int size: 0; property bool _forceAnimation: false }
