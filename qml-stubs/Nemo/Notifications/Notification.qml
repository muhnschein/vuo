import QtQuick 2.6

// The surface of nemo-qml-plugin-notifications' Notification that Vuo uses,
// transcribed from its notification.h: the properties and signals below are
// the real ones, with the real types where QML can spell them. It publishes
// nothing -- there is no home screen to publish to.
QtObject {
    property string category
    property string appName
    property string appIcon
    property string summary
    property string body
    property string previewSummary
    property string previewBody
    property int itemCount: 1
    property int replacesId: 0
    property int expireTimeout: -1
    property bool isTransient: false
    property var remoteActions: []

    signal clicked()
    signal closed(int reason)

    function publish() {}
    function close() {}
    // Static in C++, callable on an instance from QML: every notification
    // this process owns, including ones a previous run left behind.
    function notifications() { return [] }
}
