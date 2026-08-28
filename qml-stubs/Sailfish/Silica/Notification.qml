import QtQuick 2.6
QtObject {
    property string category
    property string summary
    property string body
    property string previewSummary
    property string previewBody
    property int itemCount: 1
    property int replacesId: 0
    property var remoteActions
    function publish() {}
    function close() {}
}
