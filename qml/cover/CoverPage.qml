import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * What the cover has to say while the app is minimised: how much is unread,
 * where it came from, and whether sync is in trouble.
 *
 * Three states, and nothing else on the cover:
 *
 *   I.   No feeds -- a fresh install, or an account that has never synced --
 *        and the cover says so, in a line.
 *   II.  Feeds, but nothing new. The feeds' favicons, laid out on rings
 *        around the centre, all of them faint, all monochrome. The centre is
 *        empty: that is the news.
 *   III. Feeds, and something new. The same rings, and the unread count in
 *        the middle of them, large. The innermost ring is drawn solid and
 *        each ring outward fainter, so the eye lands on the number and the
 *        feeds fall away around it. Still monochrome, so a favicon's own
 *        colours never compete with the count.
 *
 * The feeds are repeated around the rings until the cover is full: a handful
 * of feeds is a handful of icons, and the field would otherwise be mostly
 * empty. Feeds with something new come first from the model, so they take
 * the innermost ring.
 *
 * The rings are laid out in a pass over the feeds the model hands over as
 * JSON, which a view over its rows could not do: a view draws each row once,
 * and the point here is to REPEAT the feeds until the rings are full.
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
    /// What the field draws: `{feed, ring, x, y, size}` per cell, the feeds
    /// repeated ring by ring until the cover is full. `ring` is 0 for the
    /// innermost. Each cell carries its own size, so a cell can never be
    /// drawn at a size other than the one it was placed for.
    property var cells: []

    /// The three states, named once. II is `hasFeeds && !hasNews`.
    readonly property bool hasFeeds: cover.feedList.length > 0
    readonly property bool hasNews: cover.unreadCount > 0

    /// The field's measurements, all from the cover's width.
    ///
    /// A FUNCTION, deliberately, and `gather` calls it rather than reading
    /// properties bound to these values. A change handler such as
    /// `onWidthChanged` runs BEFORE the bindings that depend on the width
    /// have been re-evaluated, so a pass that read `cover.iconSize` from
    /// there laid the rings out with a one-pixel icon -- and then spent the
    /// rest of the session making hundreds of thousands of cells. Measured
    /// under qmlscene: the same pass that gives four rings at the right size
    /// gave a ring every 1.3 pixels the moment before.
    ///
    ///   icon      an icon's edge. A favicon is a 32-pixel image, and a
    ///             bigger cell only upscales it further past recognition;
    ///             this is about seven across.
    ///   inner     the innermost ring's radius: the hole the count sits in,
    ///             sized for three digits at the size they are drawn below
    ///             with the icons' inner edges clear of them.
    ///   ringStep  from one ring to the next, centre to centre.
    ///   cellStep  along a ring, centre to centre.
    function metrics() {
        var icon = Math.max(1, Math.round(cover.width * 0.14))
        return {
            icon: icon,
            inner: cover.width * 0.34,
            ringStep: icon * 1.3,
            cellStep: icon * 1.25
        }
    }
    /// An icon's edge, for whatever draws relative to it.
    property int iconSize: cover.metrics().icon

    /// How faint the field is when there is nothing new (state II).
    property real quietOpacity: 0.3
    /// With news (state III): the innermost ring is solid, and each ring out
    /// is this fraction of the one inside it, down to `farOpacity`.
    property real fade: 0.55
    property real farOpacity: 0.15

    /// Read the feeds again and lay them out on the rings. Called on every
    /// change to the list and whenever the shape changes, so the cells are
    /// always the right number.
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
        if (all.length > 0 && cover.width > 0 && cover.height > 0) {
            var m = cover.metrics()
            var cx = cover.width / 2
            var cy = cover.height / 2
            // Rings out to the corners: the cover is taller than it is wide,
            // so a ring runs past the sides long before it reaches the top.
            var reach = Math.sqrt(cx * cx + cy * cy) + m.icon / 2
            var next = 0
            // The cap is a backstop, not a design: a cover's shape gives
            // four or five rings, and a pass that wants more than thirty-two
            // has been handed a size it should not have been.
            for (var ring = 0; ring < 32; ring++) {
                var radius = m.inner + ring * m.ringStep
                if (radius > reach) {
                    break
                }
                var around = Math.max(1, Math.floor(2 * Math.PI * radius / m.cellStep))
                // Every other ring turns by half a cell, so the icons nest
                // rather than line up along spokes.
                var turn = (ring % 2) * Math.PI / around
                for (var i = 0; i < around; i++) {
                    var angle = -Math.PI / 2 + turn + i * 2 * Math.PI / around
                    // Rounded BEFORE the edge test below, so the cell that
                    // is kept is the cell that is drawn.
                    var x = Math.round(cx + radius * Math.cos(angle) - m.icon / 2)
                    var y = Math.round(cy + radius * Math.sin(angle) - m.icon / 2)
                    // A cell the edge would cut off entirely is not drawn,
                    // and takes no feed: the first feeds -- the ones with
                    // something new -- must all land where they can be seen.
                    if (x + m.icon <= 0 || x >= cover.width
                            || y + m.icon <= 0 || y >= cover.height) {
                        continue
                    }
                    var feed = all[next % all.length]
                    next++
                    made.push({ feed: feed, ring: ring, x: x, y: y, size: m.icon })
                }
            }
        }
        cover.feedList = all
        cover.cells = made
    }
    onFeedsJsonChanged: cover.gather()
    onWidthChanged: cover.gather()
    onHeightChanged: cover.gather()
    Component.onCompleted: cover.gather()

    // No feeds yet: say so, in a line that wraps rather than runs off the
    // cover in a language where it is longer.
    Label {
        objectName: "emptyLabel"
        anchors.centerIn: parent
        width: parent.width - 2 * Theme.paddingLarge
        visible: !cover.hasFeeds
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        font.pixelSize: Theme.fontSizeSmall
        color: Theme.secondaryColor
        text: qsTr("No feeds")
    }

    // The feeds, on rings around the centre, filling the cover. The outer
    // rings run past every edge, which the clip takes care of.
    Item {
        id: field
        objectName: "faviconField"
        anchors.fill: parent
        clip: true

        // ALL MONOCHROME, in one pass. The field is rendered to a texture and
        // drawn through a shader that keeps each pixel's brightness and drops
        // its colour, so a favicon's own palette never competes with the
        // count and the field reads as one material. Greyscale rather than
        // a tint in the theme's colour: tinting keeps the brightness too, so
        // a dark icon would vanish into a dark ambience, and on a light one
        // -- where the primary colour is black -- every icon would flatten
        // to a silhouette. One texture the size of the cover rather than an
        // effect per cell, and QtQuick's own ShaderEffect rather than
        // QtGraphicalEffects, which the app does not otherwise import. The
        // shader is fixed text: nothing foreign is anywhere near it (§9.3).
        layer.enabled: true
        layer.effect: ShaderEffect {
            // Qt draws with premultiplied alpha, so the brightness of the
            // premultiplied colour is already scaled by the pixel's coverage,
            // and a grey made from it is premultiplied too.
            fragmentShader: "
                varying highp vec2 qt_TexCoord0;
                uniform sampler2D source;
                uniform lowp float qt_Opacity;
                void main() {
                    lowp vec4 pixel = texture2D(source, qt_TexCoord0);
                    lowp float light = dot(pixel.rgb, vec3(0.299, 0.587, 0.114));
                    gl_FragColor = vec4(vec3(light), pixel.a) * qt_Opacity;
                }"
        }

        Repeater {
            model: cover.cells

            Item {
                objectName: "fieldCell"
                /// Which ring this cell is on, 0 innermost. Read by the
                /// cover's test.
                property int ring: modelData.ring

                x: modelData.x
                y: modelData.y
                width: modelData.size
                height: modelData.size
                // OPAQUE INSIDE, TRANSPARENT OUTSIDE while there is news;
                // ALL TRANSPARENT while there is none. Nothing is drawn
                // behind the icons -- no plate, no outline. The field IS the
                // icons, and the rings are the only structure in it.
                opacity: cover.hasNews
                         ? Math.max(cover.farOpacity, Math.pow(cover.fade, ring))
                         : cover.quietOpacity

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
                // sync -- would otherwise leave the field blank. Then, and only
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

    // The count, in the hole the rings leave at the centre. Only while there
    // is one: a zero would say what the empty centre already says, and the
    // number is the one thing on the cover with any colour, so it must mean
    // something when it is there.
    Label {
        id: unreadLabel
        objectName: "unreadTotal"
        anchors.centerIn: parent
        visible: cover.hasNews
        textFormat: Text.PlainText
        // Three digits is what a feed reader needs -- an unread count in the
        // hundreds is an ordinary week here, not the runaway a chat app's
        // would be. Past that the reader is not counting them off a cover
        // anyway.
        text: cover.unreadCount > 999 ? "999+" : cover.unreadCount
        // Four glyphs at the huge size would reach the innermost ring; the
        // number steps down instead, which keeps the digits legible AND the
        // hole clear around them.
        font.pixelSize: cover.unreadCount > 99 ? Theme.fontSizeExtraLarge
                                               : Theme.fontSizeHuge
        color: Theme.primaryColor
    }

    // Sync speaks along the top edge, and only while it has something to
    // say. There is no room on a cover for anything longer, and the count
    // in the centre must not be replaced by a spinner: it is the one thing
    // the cover is for. The outer rings under this line are the faintest, so
    // it stays legible without a plate. The TOP edge rather than the bottom,
    // which is where the cover action's icon is drawn.
    Row {
        objectName: "status"
        anchors {
            top: parent.top
            horizontalCenter: parent.horizontalCenter
            topMargin: Theme.paddingMedium
        }
        spacing: Theme.paddingSmall
        visible: cover._showFailure || cover.syncing

        // Exactly one of the two states occupies this, so the spinner
        // cannot be drawn across the warning.
        Item {
            width: Theme.iconSizeSmall
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
            anchors.verticalCenter: parent.verticalCenter
            textFormat: Text.PlainText
            // Fixed, translated strings only -- never the server's error
            // text.
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
