import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * What the cover has to say while the app is minimised: how much is unread,
 * and whether sync is in trouble.
 *
 * The count is NEGATIVE SPACE. The whole cover is texture, after Jolla's own
 * packaging -- see components/TextArt.qml -- and the number is the part of
 * it where the lines are not: they hug the outline of each digit, and a few
 * lines out they have forgotten it and are the usual sweeps. Nothing is
 * drawn inside the digits, and nothing is drawn on top of the texture; the
 * number is not a label, and it is not a hole cut in the pattern. It is what
 * the pattern was painted around.
 *
 * That makes the texture a SET of masks rather than one: every count from 0
 * to 99 has its own, and everything past that shares "99+". The digits' ink
 * is centred across the cover, and centred between the top edge and the
 * cover-action strip at the bottom, where the reload button is unchanged.
 * The text is filler and means nothing; the count is the message.
 *
 * Sync has lost the heading it used to speak under, so it says its piece in
 * the strip just above the action area instead: a spinner while refreshing,
 * a warning for a few seconds after a refresh fails, and a fixed line of
 * text beside either. It is never the server's text (§9.3).
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

    /// The count as the art says it, and as the mask for it is named. Two
    /// digits is what fits at a size that reads from across a room; an
    /// unread count in the hundreds is an ordinary week for a feed reader,
    /// and past a hundred the reader is not counting them off a cover
    /// anyway. A count below zero cannot happen, and is shown as none.
    readonly property string countKey:
        cover.unreadCount > 99 ? "99+" : "" + Math.max(0, cover.unreadCount)

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

    // The texture, which is the whole of the cover: the mask painted around
    // this count. No fade and no cleared disc -- the room for the number is
    // already in the mask, and nothing else sits on the texture for long.
    TextArt {
        id: art
        objectName: "textArt"
        anchors.fill: parent
        source: "../art/cover/" + cover.countKey + ".png"
        // An ink of 1.5, not the usual 0.55: the cover's masks carry their
        // own strength -- full where the lines touch the digits, easing
        // down to 0.55 / 1.5 a few lines out -- so the far lines land at
        // 0.55 as everywhere else, and the lines on the digits are driven
        // past full. The shader's output saturates there, which makes the
        // thin glyphs bolder and brighter than a mask alone could; the
        // number is the brightest thing here and the sweeps recede from
        // it. See tools/textart/render.qml.
        ink: 1.5
    }

    // The count as DATA, for anything that reads the cover rather than
    // looks at it. Never drawn: the art already says it.
    Label {
        objectName: "unreadTotal"
        visible: false
        textFormat: Text.PlainText
        text: cover.countKey
    }

    // Where sync speaks: just above the action strip, centred. Exactly one
    // of the two states occupies the slot, so the spinner cannot be drawn
    // across the warning; the line beside it is a fixed, translated string.
    Row {
        id: status
        objectName: "syncStatus"
        anchors {
            horizontalCenter: parent.horizontalCenter
            bottom: parent.bottom
            bottomMargin: Theme.itemSizeSmall + Theme.paddingSmall
        }
        spacing: Theme.paddingSmall
        visible: cover.syncing || cover._showFailure

        Item {
            id: statusSlot
            width: Theme.iconSizeSmall
            height: statusLabel.height
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
                //
                // Bound to the cover's own status rather than set to
                // `true`. A plain `true` overrides the `visible` half of
                // Silica's predicate as well as the `Qt.application.active`
                // half -- so a sync running with the screen off, or with
                // another app in front, drove a rotation animation and the
                // repaints that go with it for nobody. `Cover.Active` is
                // exactly "this cover is the one being shown", which is
                // the case the escape hatch was wanted for and the only
                // case it is now used in.
                _forceAnimation: cover.status === Cover.Active
            }

            Image {
                anchors.centerIn: parent
                source: "image://theme/icon-s-warning"
                visible: cover._showFailure
            }
        }

        Label {
            id: statusLabel
            objectName: "syncStatusLabel"
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            // Fixed, translated strings only -- never the server's error
            // text. Empty while there is nothing to say, so the row can
            // hide without a stale word in it.
            text: cover._showFailure
                  ? (cover.syncErrorIsAuth ? qsTr("Sign-in failed") : qsTr("Refresh failed"))
                  : (cover.syncing ? qsTr("Refreshing") : "")
            font.pixelSize: Theme.fontSizeExtraSmall
            color: cover._showFailure ? Theme.errorColor : Theme.secondaryHighlightColor
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
