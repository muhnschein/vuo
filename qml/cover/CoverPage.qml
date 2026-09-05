import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * What the cover has to say while the app is minimised: how much is unread,
 * and whether sync is in trouble.
 *
 * The heading is laid out as the platform's own covers lay theirs out, and
 * with postivene's measures exactly: the name top left with a line under it,
 * the two set close, and the number top right, large. The rest of the cover
 * is texture, after Jolla's own packaging -- see components/TextArt.qml. The
 * text is filler and means nothing; the count is the message, and the
 * texture is what makes the cover Vuo's rather than a number on a plain
 * ground.
 *
 * A cover is drawn while the app is NOT the active window, which is the source
 * of most of the care below -- see the BusyIndicator note.
 */
CoverBackground {
    id: cover

    /// Unread across the whole mirror, not just one scope.
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

    // The texture, under everything else. It begins where postivene's field
    // of faces begins under the same heading -- a large padding below it --
    // and fades in over the next tenth of the cover rather than starting on
    // a hard line.
    TextArt {
        id: art
        objectName: "textArt"
        anchors.fill: parent
        fadeFrom: heading.y + heading.height + Theme.paddingLarge
        fadeTo: fadeFrom + cover.height * 0.1
    }

    // The name and what the number means, top left; the number top right,
    // always -- a zero says as much as a count.
    Column {
        id: heading
        anchors {
            top: parent.top
            left: parent.left
            right: unreadLabel.left
            margins: Theme.paddingLarge
            rightMargin: Theme.paddingMedium
        }
        // Postivene's: the two lines set closer than their line boxes would
        // put them, so they read as one heading.
        spacing: -Theme.paddingSmall

        Label {
            objectName: "brand"
            width: parent.width
            textFormat: Text.PlainText
            text: "Vuo"
            color: Theme.highlightColor
            font.pixelSize: Theme.fontSizeMedium
            truncationMode: TruncationMode.Fade
        }

        // The line under the name is where sync speaks. There is no room on a
        // cover for anything longer, and the count above it must not be
        // replaced by a spinner: it is the one thing the cover is for.
        Row {
            width: parent.width
            spacing: Theme.paddingSmall
            // No taller than the line it holds: the icon's slot used to set
            // the row's height, which pushed this line down from the name by
            // more than postivene's sits from its own.
            height: subtitleLabel.height

            // Exactly one of the two states occupies this, so the spinner
            // cannot be drawn across the warning.
            Item {
                id: statusSlot
                width: cover._showFailure || cover.syncing ? Theme.iconSizeSmall : 0
                height: subtitleLabel.height
                anchors.verticalCenter: parent.verticalCenter

                BusyIndicator {
                    anchors.centerIn: parent
                    running: cover.syncing && !cover._showFailure
                    size: BusyIndicatorSize.ExtraSmall
                    // THE COVER IS NOT THE ACTIVE WINDOW, and Silica's
                    // indicator gates its RotationAnimator on
                    // `_forceAnimation || (visible && Qt.application.active)`
                    // (BusyIndicator.qml:80). On a cover the second half is
                    // always false, so the spinner appeared, sat perfectly
                    // still, and read as a frozen app. `_forceAnimation` is
                    // the escape hatch that predicate is written around.
                    _forceAnimation: true
                }

                Image {
                    anchors.centerIn: parent
                    source: "image://theme/icon-s-warning"
                    visible: cover._showFailure
                }
            }

            Label {
                id: subtitleLabel
                objectName: "subtitle"
                width: parent.width - statusSlot.width - Theme.paddingSmall
                anchors.verticalCenter: parent.verticalCenter
                textFormat: Text.PlainText
                // Fixed, translated strings only -- never the server's error
                // text.
                text: cover._showFailure
                      ? (cover.syncErrorIsAuth ? qsTr("Sign-in failed") : qsTr("Refresh failed"))
                      : (cover.syncing ? qsTr("Refreshing") : qsTr("Unread"))
                font.pixelSize: Theme.fontSizeExtraSmall
                color: cover._showFailure ? Theme.errorColor : Theme.secondaryHighlightColor
                truncationMode: TruncationMode.Fade
            }
        }
    }

    Label {
        id: unreadLabel
        objectName: "unreadTotal"
        anchors {
            top: parent.top
            right: parent.right
            topMargin: Theme.paddingMedium
            rightMargin: Theme.paddingLarge
        }
        textFormat: Text.PlainText
        // Three digits is what a feed reader needs -- an unread count in the
        // hundreds is an ordinary week here, not the runaway a chat app's
        // would be. Past that the reader is not counting them off a cover
        // anyway.
        text: cover.unreadCount > 999 ? "999+" : cover.unreadCount
        // Four glyphs at the huge size run straight over the app's name; the
        // number is anchored to the edge and grows leftwards into it. It
        // steps down instead, which keeps the digits legible AND the name
        // readable.
        font.pixelSize: cover.unreadCount > 99 ? Theme.fontSizeExtraLarge
                                               : Theme.fontSizeHuge
        color: Theme.primaryColor
    }

    CoverActionList {
        CoverAction {
            iconSource: "image://theme/icon-cover-refresh"
            onTriggered: cover.refresh()
        }
    }

    signal refresh()
}
