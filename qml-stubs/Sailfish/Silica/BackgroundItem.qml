import QtQuick 2.6
Item { property bool down: false; property bool highlighted: false; default property alias __content: __p.data; Item { id: __p } signal clicked() }
