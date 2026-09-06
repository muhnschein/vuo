import QtQuick 2.6
import Sailfish.Silica 1.0
// A SilicaFlickable holding one WebView under an optional header, which is
// what lets a pulley menu sit over a web page. `header` takes a Component,
// as SilicaListView's does.
SilicaFlickable {
    property alias webView: webView
    property alias header: headerLoader.sourceComponent
    property alias headerItem: headerLoader.item
    WebView { id: webView }
    Loader { id: headerLoader }
}
