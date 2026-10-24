import QtQuick 2.6
Item {
    property bool busy: false
    property bool quickSelect: false
    // Menu on screen: open or being dragged. Declared property so
    // `onActiveChanged` handler reading `active` gets value, not undefined.
    property bool active: false
    default property alias __content: __p.data
    Item { id: __p }
}
