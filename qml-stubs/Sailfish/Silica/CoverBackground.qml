import QtQuick 2.6
// A cover's children go into a content item that FILLS the cover, as they do
// in Silica's own. Without the fill they land in a zero-sized item at the
// origin, and anything anchored to `parent` collapses -- silently, since a
// cover with no height still compiles and instantiates.
Item {
    property real dimmedOpacity: 0.4
    // Whether the cover is the one being shown. `Cover.Active` on a device;
    // see Cover.qml. Stubbed as Active so a cover instantiated by the load
    // test exercises the same branch a visible cover would.
    property int status: Cover.Active
    default property alias __content: __p.data
    Item { id: __p; anchors.fill: parent }
}
