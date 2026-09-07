import QtQuick 2.6
Item {
    property bool busy: false
    property bool quickSelect: false
    // Whether the menu is on screen -- open, or being dragged. Declaring it
    // as a property is what gives `onActiveChanged` something to read; the
    // stub used to carry the bare signal, so a handler that looked at
    // `active` was reading undefined.
    property bool active: false
    default property alias __content: __p.data
    Item { id: __p }
}
