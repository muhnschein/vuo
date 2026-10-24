import QtQuick 2.6
// qtmozembed QuickMozView properties Vuo reads, slots it calls. `url` is QUrl
// on device; assigning string drives real one too.
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
