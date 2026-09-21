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
 * text beside either. It is never the server's text (§9.3). That is the one
 * thing that does get drawn over the texture, so while it is there the
 * TEXTURE GETS OUT OF ITS WAY: the mask sinks away towards the foot of the
 * cover, further the nearer the bottom edge, and the line sits in what it
 * leaves behind. Nothing is laid on top -- a wash over the pattern would
 * still be a thing drawn on the cover, and the cover is the pattern.
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

    /// True while sync has a word to say -- a spinner or a warning. The
    /// status row and the texture's retreat behind it both hang off this,
    /// so the room cannot open without the thing it is made for, or
    /// outlast it.
    readonly property bool hasStatus: cover.syncing || cover._showFailure

    /// How far above the bottom edge sync speaks: the bottom of the status
    /// row, and the floor of the room the texture gives up for it.
    ///
    /// The rule this is set to is that the line should sit as far above the
    /// refresh icon as the icon sits above the bottom edge. The icon is
    /// Silica's, centred in the cover-action strip, and a cover cannot ask
    /// how tall that strip is -- so this is measured rather than derived:
    /// off a device screenshot the icon cleared the bottom edge by about
    /// 8% of the cover's height and its own top was about 18% up, which
    /// puts the line's bottom about a quarter of the way up the cover.
    /// One `paddingSmall`, where this started, left seven pixels.
    ///
    /// It is one number, deliberately: nudging the line moves the room made
    /// for it with it.
    readonly property real statusBaseline:
        Theme.itemSizeSmall + Theme.paddingLarge

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
        // The count's own outline, drawn over the texture in the ambience's
        // colour. It is a second mask off the same painter rather than a
        // digit set here, because the face the masks were cut in is not on
        // the phone -- see components/TextArt.qml.
        edgeSource: "../art/cover/" + cover.countKey + "-edge.png"
        edgeColour: Theme.highlightColor
        // An ink of 1.5, not the usual 0.55: the cover's masks carry their
        // own strength -- full where the lines touch the digits, easing
        // down to 0.55 / 1.5 a few lines out -- so the far lines land at
        // 0.55 as everywhere else, and the lines on the digits are driven
        // past full. The shader's output saturates there, which makes the
        // thin glyphs bolder and brighter than a mask alone could; the
        // number is the brightest thing here and the sweeps recede from
        // it. See tools/textart/render.qml.
        ink: 1.5

        // The room for the status line, made by the texture rather than
        // over it: the mask runs out towards the bottom edge while sync has
        // something to say, and is whole again the moment it stops.
        //
        // The band is twice the height of everything below the row's top,
        // so the line lands around the middle of the ramp with the pattern
        // already well thinned under it and the very foot of the cover
        // nearly bare -- and a line that WRAPPED to two rows grows the band
        // with it rather than climbing out of the top of it.
        //
        // Off means both at zero, not a band of no height at the bottom:
        // the shader's test is `fadeOutTo > fadeOutFrom`.
        fadeOutFrom: cover.hasStatus
                     ? cover.height - (cover.statusBaseline + status.height) * 2
                     : 0
        fadeOutTo: cover.hasStatus ? cover.height : 0
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
            bottomMargin: cover.statusBaseline
        }
        spacing: Theme.paddingSmall
        visible: cover.hasStatus

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

            // The line WRAPS rather than running off both edges.
            //
            // A `Row` centred on the cover gives its children whatever
            // width they ask for, and a `Label` asks for the whole string
            // on one line. English fits; "Aktualisierung fehlgeschlagen"
            // does not, so the row grew wider than the cover, centring put
            // the overhang on both sides, and the German read as a phrase
            // with its head and tail cut off. Forty catalogues means this
            // was never going to be a question of picking shorter English.
            //
            // Bounded instead of elided: a cover has the room going UP, and
            // the row hangs off its bottom edge, so a second line pushes the
            // top of the row up into texture the mask has already given up
            // and leaves the gap to the refresh icon exactly as it was.
            // `Math.min` keeps a line that does fit at its natural width,
            // so short strings still centre as a tight row rather than a
            // centred block in a full-width box.
            width: Math.min(implicitWidth, cover.width
                            - Theme.paddingMedium * 2
                            - statusSlot.width - status.spacing)
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
            // Three lines of extra-small text is most of the room between
            // the count and the action strip; past that the line is cut,
            // which is at least cut at one end and on a word.
            maximumLineCount: 3
            elide: Text.ElideRight
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
