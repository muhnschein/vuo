import QtQuick 2.6
import Sailfish.Silica 1.0
// SilicaFlickable holding one WebView under optional header: lets pulley menu
// sit over web page. `header` takes Component, like SilicaListView's.
SilicaFlickable {
    property alias webView: webView
    property alias header: headerLoader.sourceComponent
    property alias headerItem: headerLoader.item
    WebView { id: webView }
    Loader { id: headerLoader }
}
