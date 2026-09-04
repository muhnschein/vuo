import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * What the cover has to say while the app is minimised: how much is unread,
 * where it came from, and whether sync is in trouble.
 *
 * The heading is laid out as the platform's own covers lay theirs out: the
 * name top left with a line under it, and the number top right, large. Under
 * it, a staggered field of feed favicons and nothing else -- no plates behind
 * them -- dimmed, the few repeated to fill it; whichever feed has something
 * new is drawn bright where it stands. No feeds yet -- a fresh install, or an
 * account that has never synced -- and the cover says so in a line instead.
 *
 * The grid is laid out in a pass over the feeds the model hands over as JSON,
 * which a view over its rows could not do: a view draws each row once, and the
 * point here is to REPEAT the feeds until the grid is full.
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

    /// The feeds, as the feed model hands them over: a JSON list of
    /// `{feedId, title, unread, icon}`, the ones with something new first.
    ///
    /// Parsed with JSON.parse, never eval -- and nothing here builds QML out
    /// of it (§9.3). The titles are the feed operators' words, so the letter
    /// drawn from one is drawn as PlainText; the icons are their bytes,
    /// already sniffed and capped by the core and carried as `data:` URIs, so
    /// drawing one fetches nothing from the network.
    property string feedsJson: ""

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

    /// The feeds, in the order they arrived.
    property var feedList: []
    /// What the grid draws: `{feed, row, col, loud}` per cell, the feeds
    /// repeated to fill it and `loud` on the first cell of any feed with
    /// something new.
    property var cells: []

    /// The grid's shape: four across, with every other row shifted half a cell
    /// and holding one more, cut off at both edges -- so the rows nest, and
    /// the grid reads as a field of icons rather than a table. Four rather
    /// than the three a grid of faces would use: a favicon is a 32-pixel
    /// image, and a wider cell only upscales it further past recognition.
    property int columns: 4
    property int cellSize: Math.floor(cover.width / cover.columns)
    property int rowStep: Math.max(1, Math.round(cover.cellSize * 0.9))
    property int rows: cover.cellSize > 0
                       ? Math.ceil(grid.height / cover.rowStep)
                       : 0

    /// Read the feeds again: who is there and what fills the grid. Called on
    /// every change to the list and whenever the shape changes, so the cells
    /// are always the right number.
    function gather() {
        var all = []
        try {
            all = JSON.parse(cover.feedsJson)
        } catch (err) {
            all = []
        }
        if (!all || all.length === undefined) {
            all = []
        }
        var made = []
        var lit = {}
        var next = 0
        for (var row = 0; row < cover.rows && all.length > 0; row++) {
            var across = row % 2 === 0 ? cover.columns + 1 : cover.columns
            for (var col = 0; col < across; col++) {
                var feed = all[next % all.length]
                next++
                var loud = feed.unread > 0 && !lit[feed.feedId]
                if (loud) {
                    lit[feed.feedId] = true
                }
                made.push({ feed: feed, row: row, col: col, loud: loud })
            }
        }
        cover.feedList = all
        cover.cells = made
    }
    onFeedsJsonChanged: cover.gather()
    onRowsChanged: cover.gather()
    Component.onCompleted: cover.gather()

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

            // Exactly one of the two states occupies this, so the spinner
            // cannot be drawn across the warning.
            Item {
                id: statusSlot
                width: cover._showFailure || cover.syncing ? Theme.iconSizeSmall : 0
                height: Theme.iconSizeSmall
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

    // No feeds yet: say so, in a line that wraps rather than runs off the
    // cover in a language where it is longer.
    Label {
        objectName: "emptyLabel"
        anchors.centerIn: grid
        width: parent.width - 2 * Theme.paddingLarge
        visible: cover.feedList.length === 0
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        font.pixelSize: Theme.fontSizeSmall
        color: Theme.secondaryColor
        text: qsTr("No feeds")
    }

    // The feeds, filling the room under the heading. The shifted rows run past
    // both edges by half a cell, which the clip takes care of.
    Item {
        id: grid
        objectName: "faviconGrid"
        anchors {
            top: heading.bottom
            left: parent.left
            right: parent.right
            bottom: parent.bottom
            topMargin: Theme.paddingMedium
        }
        clip: true

        Repeater {
            model: cover.cells

            Item {
                objectName: "gridCell"
                // Read by the cover's test, and the one place the quiet/loud
                // distinction is written down.
                property bool loud: modelData.loud

                x: modelData.col * cover.cellSize
                   - (modelData.row % 2 === 0 ? cover.cellSize / 2 : 0)
                y: modelData.row * cover.rowStep
                // A fraction of the cell rather than the cell less a padding:
                // the rows are a nested 0.9 of a cell apart, so an icon the
                // size of its cell would overlap the row above it and the
                // field would close up into a wall. This leaves a gap between
                // neighbours about as wide as the one between diagonal ones.
                width: Math.round(cover.cellSize * 0.74)
                height: width
                // Nothing is drawn behind the icons -- no plate, no ring. The
                // field IS the icons, and the only thing separating a feed
                // with something new from the rest is that the rest are
                // dimmed. (Postivene desaturates its quiet half through
                // QtGraphicalEffects; a shader module the app otherwise never
                // imports, running per cell, is a steep price for a favicon,
                // and dimming reads the same at cover size.)
                opacity: loud ? 1.0 : 0.35

                Image {
                    id: favicon
                    objectName: "cellFavicon"
                    anchors.fill: parent
                    sourceSize.width: width
                    sourceSize.height: height
                    fillMode: Image.PreserveAspectFit
                    // A `data:` URI built in Rust from bytes already in the
                    // mirror -- no network fetch happens here, so drawing the
                    // cover cannot leak the device's IP (§9.3).
                    source: modelData.feed.icon
                    asynchronous: true
                    // An icon whose format the device ships no handler for
                    // leaves its cell empty rather than dropping a letter into
                    // a field of pictures. The model sends feeds without an
                    // icon only when NO feed has one, so that is the one case
                    // the initial below is drawn for.
                    visible: status === Image.Ready
                }

                // A mirror whose icons have not been fetched yet -- a first
                // sync -- would otherwise leave the grid blank. Then, and only
                // then, the feeds arrive without icons and this draws them.
                Label {
                    objectName: "cellInitial"
                    anchors.centerIn: parent
                    visible: modelData.feed.icon.length === 0
                    textFormat: Text.PlainText
                    text: modelData.feed.title.substring(0, 1).toUpperCase()
                    font.pixelSize: Math.round(parent.width * 0.5)
                    color: Theme.primaryColor
                }
            }
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
