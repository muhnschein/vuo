import QtQuick 2.6
// Paints the current ambience's background -- the same wallpaper the app
// window is transparent to -- aligned to the screen rather than to itself,
// via `transformItem`. On a device this is a shader-backed item from the
// Silica background plugin; here it only has to carry the properties Vuo
// reads, so that `qmllint` and `make qml-load` resolve them.
Item {
    property string backgroundMaterial: ""
    property color color: "transparent"
    property color highlightColor: "transparent"
    property var material: null
    property Item transformItem: null
}
