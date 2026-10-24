import QtQuick 2.6

// Surface of nemo-qml-plugin-notifications' Notification Vuo uses, from its
// notification.h: real properties and signals, real types where QML can spell
// them. Publishes nothing; no home screen.
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
    // Static in C++, callable on instance from QML: every notification this
    // process owns, incl. ones previous run left.
    function notifications() { return [] }
}
