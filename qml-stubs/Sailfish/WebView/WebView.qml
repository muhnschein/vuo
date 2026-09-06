import QtQuick 2.6
// The properties of qtmozembed's QuickMozView that Vuo reads, and the slots
// it calls. `url` is a QUrl on the device, and assigning a string to it is how
// the real one is driven too.
Item {
    property url url
    property string title
    property bool loading: false
    property int loadProgress: 0
    property bool canGoBack: false
    property bool canGoForward: false
    property bool active: true
    property bool privateMode: false
    function goBack() {}
    function goForward() {}
    function reload() {}
    function stop() {}
}
