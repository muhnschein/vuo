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
 * That makes the texture a SET of masks rather than one: every count from 1
 * to 99 has its own, and everything past that shares "99+". The digits' ink
 * is centred across the cover, and centred between the top edge and the
 * cover-action strip at the bottom, where the reload button is unchanged.
 * The text is filler and means nothing; the count is the message.
 *
 * Nothing unread has NO number: not a 0 the lines flow around, but the
 * sweeps alone, with nothing in them and no outline over them. A cover that
 * has nothing to report says so by having nothing to say.
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

    /// Unread across whole mirror, not one scope.
    property int unreadCount: 0
    property bool syncing: false
    /// Last refresh error text, or empty. FOREIGN TEXT: never rendered here,
    /// only flag; cover has no room for actionable words, entry list shows
    /// them.
    property string syncError: ""
    property bool syncErrorIsAuth: false

    /// Count as cover shows it. Two digits fit at size readable across room;
    /// hundreds unread = ordinary week, nobody counts past hundred off cover.
    /// Below zero impossible, shown as none.
    readonly property string countKey:
        cover.unreadCount > 99 ? "99+" : "" + Math.max(0, cover.unreadCount)

    /// Mask art painted from: count's own, or "none" (pattern without number)
    /// when nothing unread. Zero still count as data; never drawn.
    readonly property string maskKey: cover.unreadCount > 0 ? cover.countKey : "none"

    /// True few seconds after bad refresh end.
    ///
    /// Error itself sticky (entry list shows until next refresh), but warning
    /// triangle forever = worse lie than endless spinner. Count is cover's
    /// purpose.
    property bool _showFailure: false

    /// One expression: failure trigger below can't drift from what counts as
    /// failure.
    property string _errorToken: cover.syncErrorIsAuth ? "auth" : cover.syncError

    /// True while sync has something to say (spinner or warning). Status row
    /// and texture retreat both hang off this; room never opens without it or
    /// outlasts it.
    readonly property bool hasStatus: cover.syncing || cover._showFailure

    /// Height above bottom edge where sync speaks: status row bottom = floor of
    /// room texture gives up.
    ///
    /// Rule: line as far above refresh icon as icon above bottom edge. Icon =
    /// Silica's, centred in cover-action strip; cover can't query strip height,
    /// so measured: device screenshot, icon cleared bottom by ~8% of cover
    /// height, top ~18% up → line bottom ~quarter up. One `paddingSmall`
    /// (original) left seven pixels.
    ///
    /// One number on purpose: nudging line moves its room too.
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

    // Clear when new refresh starts: old failure never under fresh spinner.
    onSyncingChanged: if (cover.syncing) {
        cover._showFailure = false
        failureTimer.stop()
    }

    Timer {
        id: failureTimer
        interval: 5000
        onTriggered: cover._showFailure = false
    }

    // Texture = whole cover: mask painted around count. No fade, no cleared
    // disc: room for number already in mask, nothing else sits on texture long.
    TextArt {
        id: art
        objectName: "textArt"
        anchors.fill: parent
        source: "../art/cover/" + cover.maskKey + ".png"
        // Count outline over texture in ambience colour. Second mask from same
        // painter, not digit set here: masks' face not on phone; see
        // components/TextArt.qml. No number = no outline, no file.
        edgeSource: cover.maskKey === "none"
                    ? ""
                    : "../art/cover/" + cover.maskKey + "-edge.png"
        edgeColour: Theme.highlightColor
        // Ink 1.5, not usual 0.55: cover masks carry own strength (full where lines touch digits,
        // easing to 0.55 / 1.5 few lines out, 0.55 / 1.5 throughout "none"). Far lines land at 0.55
        // like elsewhere; lines on digits driven past full, shader saturates: thin glyphs bolder,
        // brighter than mask alone. Number brightest, sweeps recede. See tools/textart/render.qml.
        ink: 1.5

        // Room for status line made by texture, not over it: mask runs out
        // toward bottom while sync has something to say, whole again when it
        // stops.
        //
        // Band = twice height of everything below row top: line mid-ramp,
        // pattern thinned under it, foot nearly bare; WRAPPED two-row line
        // grows band, doesn't climb out.
        //
        // Off = both zero, not zero-height band: shader tests `fadeOutTo >
        // fadeOutFrom`.
        fadeOutFrom: cover.hasStatus
                     ? cover.height - (cover.statusBaseline + status.height) * 2
                     : 0
        fadeOutTo: cover.hasStatus ? cover.height : 0
    }

    // Count as DATA for readers of cover. Never drawn: art says it.
    Label {
        objectName: "unreadTotal"
        visible: false
        textFormat: Text.PlainText
        text: cover.countKey
    }

    // Where sync speaks: just above action strip, centred. Exactly one state in
    // slot; spinner never over warning. Line beside = fixed translated string.
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
                // COVER NOT ACTIVE WINDOW. Silica indicator gates
                // RotationAnimator on `_forceAnimation || (visible &&
                // Qt.application.active)` (BusyIndicator.qml:80); second half
                // always false on cover → still spinner = frozen-looking app.
                // `_forceAnimation` = escape hatch.
                //
                // Bound to cover status, not `true`: `true` also overrides
                // `visible` half, so sync with screen off or other app in front
                // animates and repaints for nobody. `Cover.Active` = "this
                // cover shown", only case hatch is for.
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
            // Fixed translated strings only, never server error text. Empty
            // when nothing to say: row hides without stale word.
            text: cover._showFailure
                  ? (cover.syncErrorIsAuth ? qsTr("Sign-in failed") : qsTr("Refresh failed"))
                  : (cover.syncing ? qsTr("Refreshing") : "")
            font.pixelSize: Theme.fontSizeExtraSmall
            color: cover._showFailure ? Theme.errorColor : Theme.secondaryHighlightColor

            // Line WRAPS, doesn't run off both edges.
            //
            // Centred `Row` gives children width they ask; `Label` asks whole
            // string on one line. "Aktualisierung fehlgeschlagen" wider than
            // cover → overhang both sides, head and tail cut. Forty catalogues:
            // shorter English not a fix.
            //
            // Bounded, not elided: cover has room UP, row hangs off bottom,
            // second line pushes row top into texture mask already gave up, gap
            // to refresh icon unchanged. `Math.min` keeps fitting line at
            // natural width: short strings centre as tight row.
            width: Math.min(implicitWidth, cover.width
                            - Theme.paddingMedium * 2
                            - statusSlot.width - status.spacing)
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
            // Three extra-small lines = most room between count and action
            // strip; past that cut, at one end on word.
            maximumLineCount: 3
            elide: Text.ElideRight
        }
    }

    CoverActionList {
        CoverAction {
            objectName: "refreshAction"
            iconSource: "image://theme/icon-cover-refresh"
            onTriggered: cover.refresh()
        }
    }

    signal refresh()
}
