import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * What the cover has to say while the app is minimised: how much is unread,
 * and whether sync is in trouble.
 *
 * The heading is laid out as the platform's own covers lay theirs out: the
 * name top left with a line under it, and the number top right, large. The
 * rest of the cover is texture, after Jolla's own packaging: lines of tiny
 * text laid along nested, flowing curves, in the theme's colour and nothing
 * else. The text is filler and means nothing; the count is the message, and
 * the texture is what makes the cover Vuo's rather than a number on a plain
 * ground.
 *
 * The curves are the level sets of a soft distance to three short strokes
 * (see `field`): each stroke wears a family of capsules that grow outward and
 * merge into one another, which is the shape the packaging draws. They are
 * traced in JavaScript and the text is set along them by arc length in the
 * canvas, once, whenever the cover's size or the theme changes. The canvas
 * paints on its own thread into an image, so the pass -- some thousands of
 * glyphs -- never holds up the window.
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

    // The texture, under everything else.
    Canvas {
        id: art
        objectName: "textArt"
        anchors.fill: parent

        // Painted on the canvas's own thread into an image rather than on the
        // render thread into a framebuffer: the pass sets a few thousand
        // glyphs and takes a good fraction of a second on a phone, and a
        // cover must not stall the window it belongs to.
        renderTarget: Canvas.Image
        renderStrategy: Canvas.Threaded

        /// The filler. Any text would do; this one is the one everybody
        /// recognises as saying nothing.
        readonly property string filler:
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do "
            + "eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut "
            + "enim ad minim veniam, quis nostrud exercitation ullamco laboris "
            + "nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in "
            + "reprehenderit in voluptate velit esse cillum dolore eu fugiat "
            + "nulla pariatur. Excepteur sint occaecat cupidatat non proident, "
            + "sunt in culpa qui officia deserunt mollit anim id est laborum. "

        /// A glyph's height. Texture, not reading matter: about forty-five
        /// to the cover's width, which is the packaging's own density.
        readonly property real glyph: Math.max(4, Math.round(cover.width / 46))
        /// From one line of text to the next, centre to centre.
        readonly property real spacing: art.glyph * 1.6
        /// How softly the strokes' families of curves merge (see `field`).
        readonly property real softness: art.spacing * 2.0
        /// The text's darkness on the ground. Greyscale, by way of the
        /// theme's own colour at less than full strength.
        readonly property real ink: 0.55
        /// The heading sits on top; the texture fades in beneath it rather
        /// than being cut off under it, from `fadeFrom` down to `fadeTo`.
        readonly property real fadeFrom: heading.y + heading.height * 0.6
        readonly property real fadeTo: heading.y + heading.height + cover.height * 0.12

        /// The theme's colour, and a repaint when the ambience changes it.
        property color colour: Theme.primaryColor
        onColourChanged: art.requestPaint()
        onWidthChanged: art.requestPaint()
        onHeightChanged: art.requestPaint()

        /// Three short strokes, in the cover's own proportions, that the
        /// curves grow out of. Each is a stroke rather than a point so the
        /// innermost curves are capsules and every family has a direction,
        /// which is what gives the sweeps their lean.
        function lobes() {
            var w = art.width
            var h = art.height
            return [
                { x: w * 0.22, y: h * 0.40, x2: w * 0.34, y2: h * 0.30 },
                { x: w * 0.90, y: h * 0.68, x2: w * 0.80, y2: h * 0.56 },
                { x: w * 0.42, y: h * 1.02, x2: w * 0.30, y2: h * 0.92 }
            ]
        }

        /// Distance from (x, y) to the stroke `l`.
        function strokeDistance(l, x, y) {
            var vx = l.x2 - l.x
            var vy = l.y2 - l.y
            var wx = x - l.x
            var wy = y - l.y
            var vv = vx * vx + vy * vy
            var t = vv > 0 ? Math.max(0, Math.min(1, (wx * vx + wy * vy) / vv)) : 0
            var dx = x - (l.x + t * vx)
            var dy = y - (l.y + t * vy)
            return Math.sqrt(dx * dx + dy * dy)
        }

        /// A soft "distance to the nearest stroke": exactly the nearest
        /// stroke's distance far from all of them, and a smooth blend where
        /// two are close. Its level sets are the curves, one every
        /// `spacing`, and because the field is a distance they are evenly
        /// spaced everywhere.
        function field(L, x, y) {
            var s = 0
            for (var i = 0; i < L.length; i++) {
                s += Math.exp(-art.strokeDistance(L[i], x, y) / art.softness)
            }
            return -art.softness * Math.log(s)
        }

        function gradient(L, x, y) {
            var e = 0.5
            return { x: (art.field(L, x + e, y) - art.field(L, x - e, y)) / (2 * e),
                     y: (art.field(L, x, y + e) - art.field(L, x, y - e)) / (2 * e) }
        }

        /// Pull a point onto the curve at `level`: a few Newton steps along
        /// the gradient.
        function settle(L, p, level) {
            for (var k = 0; k < 3; k++) {
                var g = art.gradient(L, p.x, p.y)
                var n2 = g.x * g.x + g.y * g.y
                if (n2 < 1e-9) {
                    break
                }
                var f = art.field(L, p.x, p.y) - level
                p = { x: p.x - f * g.x / n2, y: p.y - f * g.y / n2 }
            }
            return p
        }

        function inside(p, margin) {
            return p.x > -margin && p.x < art.width + margin
                    && p.y > -margin && p.y < art.height + margin
        }

        /// Follow the curve at `level` from `seed`, one way, until it closes
        /// on itself or leaves the cover. Two pixels a step, each step
        /// settled back onto the curve, so the line stays on its level for
        /// as long as it runs.
        function walk(L, seed, level, direction) {
            var h = 2
            var points = []
            var p = seed
            for (var i = 0; i < 4000; i++) {
                var g = art.gradient(L, p.x, p.y)
                var n = Math.sqrt(g.x * g.x + g.y * g.y)
                if (n < 1e-6) {
                    break
                }
                p = art.settle(L, { x: p.x + direction * h * (-g.y / n),
                                    y: p.y + direction * h * (g.x / n) }, level)
                points.push(p)
                if (i > 10 && Math.abs(p.x - seed.x) < h && Math.abs(p.y - seed.y) < h) {
                    return { points: points, closed: true }
                }
                if (!art.inside(p, art.spacing)) {
                    return { points: points, closed: false }
                }
            }
            return { points: points, closed: false }
        }

        /// Every curve on the cover, as a polyline each: `{points, closed}`.
        ///
        /// Pure, and what the cover's test reads: the painting below only
        /// sets text along what this returns. Levels one `spacing` apart out
        /// to the farthest corner; on each level a curve is started from
        /// several points around each stroke and followed both ways, and a
        /// start that lands on a curve already drawn is skipped -- which is
        /// how two strokes' families become one curve where they merge.
        function layout() {
            var L = art.lobes()
            var paths = []
            if (art.width <= 0 || art.height <= 0) {
                return paths
            }
            var far = 0
            var corners = [[0, 0], [art.width, 0], [0, art.height], [art.width, art.height]]
            for (var c = 0; c < corners.length; c++) {
                far = Math.max(far, art.field(L, corners[c][0], corners[c][1]))
            }
            for (var k = 0; k * art.spacing < far + art.spacing; k++) {
                var level = art.spacing * (k + 0.5)
                var drawn = []
                for (var si = 0; si < L.length * 8; si++) {
                    var i = si % L.length
                    var angle = -Math.PI / 2 + Math.floor(si / L.length) * Math.PI / 4 + i * 0.7
                    var cx = (L[i].x + L[i].x2) / 2
                    var cy = (L[i].y + L[i].y2) / 2
                    var seed = art.settle(L, { x: cx + level * Math.cos(angle),
                                               y: cy + level * Math.sin(angle) }, level)
                    if (!art.inside(seed, 0) || Math.abs(art.field(L, seed.x, seed.y) - level) > 1) {
                        continue
                    }
                    var covered = false
                    for (var d = 0; d < drawn.length && !covered; d++) {
                        if (Math.abs(drawn[d].x - seed.x) < art.spacing * 0.6
                                && Math.abs(drawn[d].y - seed.y) < art.spacing * 0.6) {
                            covered = true
                        }
                    }
                    if (covered) {
                        continue
                    }
                    var forward = art.walk(L, seed, level, 1)
                    var points = forward.points
                    if (!forward.closed) {
                        var back = art.walk(L, seed, level, -1).points
                        back.reverse()
                        points = back.concat([seed], points)
                    }
                    if (points.length < 4) {
                        continue
                    }
                    for (var j = 0; j < points.length; j += 3) {
                        drawn.push(points[j])
                    }
                    paths.push({ points: points, closed: forward.closed })
                }
            }
            return paths
        }

        onPaint: {
            var ctx = art.getContext("2d")
            if (!ctx) {
                return
            }
            ctx.clearRect(0, 0, art.width, art.height)
            ctx.font = art.glyph + "px " + Theme.fontFamily
            ctx.textBaseline = "middle"
            var r = Math.round(art.colour.r * 255)
            var g = Math.round(art.colour.g * 255)
            var b = Math.round(art.colour.b * 255)

            var paths = art.layout()
            var offset = 0
            for (var p = 0; p < paths.length; p++) {
                var points = paths[p].points
                // Arc length along the polyline, so glyphs are set by
                // distance and not by point count.
                var along = [0]
                for (var i = 1; i < points.length; i++) {
                    var dx = points[i].x - points[i - 1].x
                    var dy = points[i].y - points[i - 1].y
                    along.push(along[i - 1] + Math.sqrt(dx * dx + dy * dy))
                }
                var total = along[along.length - 1]
                var s = 0
                var seg = 1
                // Each curve starts elsewhere in the filler, or every line
                // would open with the same word.
                var t = offset
                while (s < total) {
                    var ch = art.filler.charAt(t % art.filler.length)
                    t++
                    var advance = ctx.measureText(ch).width + art.glyph * 0.08
                    var mid = s + advance / 2
                    while (seg < along.length - 1 && along[seg] < mid) {
                        seg++
                    }
                    var a = points[seg - 1]
                    var b2 = points[seg]
                    var run = along[seg] - along[seg - 1]
                    var f = run > 0 ? (mid - along[seg - 1]) / run : 0
                    var x = a.x + (b2.x - a.x) * f
                    var y = a.y + (b2.y - a.y) * f
                    if (ch !== " ") {
                        var fade = Math.max(0, Math.min(1, (y - art.fadeFrom) / (art.fadeTo - art.fadeFrom)))
                        if (fade > 0.02) {
                            ctx.fillStyle = "rgba(" + r + "," + g + "," + b + "," + (art.ink * fade) + ")"
                            ctx.save()
                            ctx.translate(x, y)
                            ctx.rotate(Math.atan2(b2.y - a.y, b2.x - a.x))
                            ctx.fillText(ch, -advance / 2, 0)
                            ctx.restore()
                        }
                    }
                    s += advance
                }
                offset = (offset + 37) % art.filler.length
            }
        }
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
