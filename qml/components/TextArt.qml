import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Texture, after Jolla's own packaging: lines of tiny filler text laid along
 * nested, flowing curves, in the theme's colour and nothing else.
 *
 * The curves are the level sets of a soft distance to a few short strokes
 * (see `field`): each stroke wears a family of capsules that grow outward
 * and merge into one another, which is the shape the packaging draws. They
 * are traced in JavaScript and the text is set along them by arc length on
 * this canvas, once, whenever the size or the theme changes. The canvas
 * paints on its own thread into an image, so the pass -- some thousands of
 * glyphs -- never holds up the window.
 *
 * Two ways to leave room for whatever sits on top of it: a band at the top
 * that the text fades in beneath (`fadeFrom`..`fadeTo`, for a heading), and
 * a disc it fades out of (`clearRadius` around `clearX`,`clearY`, for a
 * title in the middle). Both off by default.
 *
 * The text is fixed and means nothing; nothing foreign is anywhere near this
 * file (§9.3).
 */
Canvas {
    id: art

    // Painted on the canvas's own thread into an image rather than on the
    // render thread into a framebuffer: the pass sets a few thousand glyphs
    // and takes a good fraction of a second on a phone, and neither a cover
    // nor a page must stall the window it belongs to.
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

    /// A glyph's height. Texture, not reading matter: about forty-five to
    /// the width of a cover, which is the packaging's own density. A page
    /// is wider than a cover, so it is set from a reference width rather
    /// than the item's own, and the same size on both.
    property real glyph: Math.max(4, Math.round(referenceWidth / 46))
    /// What `glyph` is measured against: a cover's width. A page passes its
    /// own width divided by how many covers wide it is.
    property real referenceWidth: art.width
    /// From one line of text to the next, centre to centre.
    property real spacing: art.glyph * 1.6
    /// How softly the strokes' families of curves merge (see `field`).
    property real softness: art.spacing * 2.0
    /// The text's darkness on the ground. Greyscale, by way of the theme's
    /// own colour at less than full strength.
    property real ink: 0.55

    /// A band at the top the text fades in beneath: nothing above
    /// `fadeFrom`, full strength from `fadeTo` down. Off while
    /// `fadeTo <= fadeFrom`.
    property real fadeFrom: 0
    property real fadeTo: 0

    /// A disc the text fades out of, for something drawn in the middle.
    /// Off while `clearRadius` is 0. Full strength beyond
    /// `clearRadius + clearFeather`.
    property real clearX: 0
    property real clearY: 0
    property real clearRadius: 0
    property real clearFeather: 0

    /// The strokes the curves grow out of, as `{x, y, x2, y2}` in fractions
    /// of the width and height. Each is a stroke rather than a point so the
    /// innermost curves are capsules and every family has a direction,
    /// which is what gives the sweeps their lean. Three by default, in the
    /// composition the cover uses.
    property var strokes: [
        { x: 0.22, y: 0.40, x2: 0.34, y2: 0.30 },
        { x: 0.90, y: 0.68, x2: 0.80, y2: 0.56 },
        { x: 0.42, y: 1.02, x2: 0.30, y2: 0.92 }
    ]

    /// The theme's colour, and a repaint when the ambience changes it.
    property color colour: Theme.primaryColor
    onColourChanged: art.requestPaint()
    onWidthChanged: art.requestPaint()
    onHeightChanged: art.requestPaint()
    onStrokesChanged: art.requestPaint()

    /// The strokes in pixels.
    function lobes() {
        var out = []
        for (var i = 0; i < art.strokes.length; i++) {
            var s = art.strokes[i]
            out.push({ x: s.x * art.width, y: s.y * art.height,
                       x2: s.x2 * art.width, y2: s.y2 * art.height })
        }
        return out
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

    /// A soft "distance to the nearest stroke": exactly the nearest stroke's
    /// distance far from all of them, and a smooth blend where two are
    /// close. Its level sets are the curves, one every `spacing`, and
    /// because the field is a distance they are evenly spaced everywhere.
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

    /// Pull a point onto the curve at `level`: a few Newton steps along the
    /// gradient.
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

    /// Follow the curve at `level` from `seed`, one way, until it closes on
    /// itself or leaves the canvas. Two pixels a step, each step settled
    /// back onto the curve, so the line stays on its level for as long as it
    /// runs.
    function walk(L, seed, level, direction) {
        var h = 2
        var points = []
        var p = seed
        for (var i = 0; i < 6000; i++) {
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

    /// Every curve on the canvas, as a polyline each: `{points, closed}`.
    ///
    /// Pure, and what the cover's test reads: the painting below only sets
    /// text along what this returns. Levels one `spacing` apart out to the
    /// farthest corner; on each level a curve is started from several points
    /// around each stroke and followed both ways, and a start that lands on
    /// a curve already drawn is skipped -- which is how two strokes' families
    /// become one curve where they merge.
    function layout() {
        var L = art.lobes()
        var paths = []
        if (art.width <= 0 || art.height <= 0 || L.length === 0) {
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

    /// How strongly to draw at (x, y): 1 in the open, fading to 0 in the
    /// band at the top and in the disc.
    function weight(x, y) {
        var w = 1
        if (art.fadeTo > art.fadeFrom) {
            w = Math.min(w, Math.max(0, Math.min(1, (y - art.fadeFrom) / (art.fadeTo - art.fadeFrom))))
        }
        if (art.clearRadius > 0) {
            var dx = x - art.clearX
            var dy = y - art.clearY
            var r = Math.sqrt(dx * dx + dy * dy)
            var feather = Math.max(1, art.clearFeather)
            w = Math.min(w, Math.max(0, Math.min(1, (r - art.clearRadius) / feather)))
        }
        return w
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
            // Arc length along the polyline, so glyphs are set by distance
            // and not by point count.
            var along = [0]
            for (var i = 1; i < points.length; i++) {
                var dx = points[i].x - points[i - 1].x
                var dy = points[i].y - points[i - 1].y
                along.push(along[i - 1] + Math.sqrt(dx * dx + dy * dy))
            }
            var total = along[along.length - 1]
            var s = 0
            var seg = 1
            // Each curve starts elsewhere in the filler, or every line would
            // open with the same word.
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
                    var w = art.weight(x, y)
                    if (w > 0.02) {
                        ctx.fillStyle = "rgba(" + r + "," + g + "," + b + "," + (art.ink * w) + ")"
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
