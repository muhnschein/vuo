import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The app cover: one number, and an honest account of what sync is doing.
 *
 * A cover is drawn while the app is NOT the active window, which is the source
 * of most of the care below -- see the BusyIndicator note.
 */
CoverBackground {
    id: cover

    property int unreadCount: 0
    property bool syncing: false
    /// The last refresh's error text, or empty. FOREIGN TEXT -- never rendered
    /// here, only used as a flag; the cover has no room to say anything a user
    /// could act on, and the entry list already shows the words.
    property string syncError: ""
    property bool syncErrorIsAuth: false

    /// True for a few seconds after a refresh ends badly.
    ///
    /// The error itself is sticky -- the entry list keeps showing it until the
    /// next refresh -- but a cover that sat on a warning triangle for ever
    /// would be a worse lie than the never-ending spinner it replaces: the
    /// count is what the cover is for.
    property bool _showFailure: false

    /// One expression, so the failure trigger below cannot get out of step
    /// with what counts as a failure.
    property string _errorToken: cover.syncErrorIsAuth ? "auth" : cover.syncError

    on_ErrorTokenChanged: {
        if (cover._errorToken.length > 0) {
            cover._showFailure = true
            failureTimer.restart()
        } else {
            cover._showFailure = false
            failureTimer.stop()
        }
    }

    // Clear the moment a new refresh starts, so an old failure cannot sit
    // under a fresh spinner.
    onSyncingChanged: if (cover.syncing) {
        cover._showFailure = false
        failureTimer.stop()
    }

    Timer {
        id: failureTimer
        interval: 5000
        onTriggered: cover._showFailure = false
    }

    Column {
        anchors.centerIn: parent
        width: parent.width - Theme.paddingLarge * 2
        spacing: Theme.paddingMedium

        // The count's slot. Exactly one of the three states occupies it, so
        // the spinner REPLACES the number instead of being drawn across it --
        // which is what a centred BusyIndicator over a centred Label did.
        Item {
            id: figure

            anchors.horizontalCenter: parent.horizontalCenter
            width: parent.width
            height: countLabel.implicitHeight

            Label {
                id: countLabel

                anchors.centerIn: parent
                textFormat: Text.PlainText
                text: cover.unreadCount > 0 ? cover.unreadCount : "—"
                font.pixelSize: Theme.fontSizeHuge
                color: Theme.primaryColor
                visible: !cover.syncing && !cover._showFailure
            }

            BusyIndicator {
                anchors.centerIn: parent
                running: cover.syncing && !cover._showFailure
                size: BusyIndicatorSize.Medium
                // THE COVER IS NOT THE ACTIVE WINDOW, and Silica's indicator
                // gates its RotationAnimator on
                // `_forceAnimation || (visible && Qt.application.active)`
                // (BusyIndicator.qml:80). On a cover the second half is
                // always false, so the spinner appeared, sat perfectly still,
                // and read as a frozen app. `_forceAnimation` is the escape
                // hatch that predicate is written around.
                _forceAnimation: true
            }

            Image {
                anchors.centerIn: parent
                source: "image://theme/icon-m-warning"
                visible: cover._showFailure
            }
        }

        Label {
            anchors.horizontalCenter: parent.horizontalCenter
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            // Fixed, translated strings only -- never the server's error text.
            text: cover._showFailure
                  ? (cover.syncErrorIsAuth ? qsTr("Sign-in failed") : qsTr("Refresh failed"))
                  : (cover.syncing ? qsTr("Refreshing") : qsTr("unread"))
            font.pixelSize: Theme.fontSizeSmall
            color: cover._showFailure ? Theme.errorColor : Theme.secondaryColor
            truncationMode: TruncationMode.Fade
        }
    }

    CoverActionList {
        CoverAction {
            iconSource: "image://theme/icon-cover-refresh"
            onTriggered: cover.refresh()
        }
    }

    signal refresh()
}
