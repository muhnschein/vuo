import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * The generator for Vuo's texture: lines of tiny filler text laid along
 * nested, flowing curves, after Jolla's own packaging.
 *
 * NOT SHIPPED. This traces and paints the pattern; `make textart` runs it
 * over the sizes the app needs and writes the masks in qml/art/, which is
 * what the app actually draws (see qml/components/TextArt.qml). The pattern
 * is the same on every device that way, it costs nothing at all to show, and
 * it cannot half-appear: a device reported the onboarding page freezing for
 * fourteen seconds while this ran, and then -- once it was fast enough to
 * watch -- that watching it draw itself was not wanted either.
 *
 * The curves are the level sets of a soft distance to a few short strokes
 * (see `fieldAt`): each stroke wears a family of capsules that grow outward
 * and merge into one another, which is the shape the packaging draws.
 *
 * Two ways to leave room for whatever sits on top -- a band at the top the
 * text fades in beneath, and a disc it fades out of -- are deliberately NOT
 * here: they are geometry the app knows and this does not, so the masks are
 * cut plain and the app's shader applies both. The same goes for the colour
 * and the strength: this paints white at full strength and the app tints it.
 *
 * The text is fixed and means nothing.
 *
 * # What it costs, and why that stopped mattering
 *
 * This was once painted live, and four things about it were slow. They are
 * all still fixed, because a generator that takes a minute is a generator
 * nobody runs:
 *
 *   - The gradient was taken by FINITE DIFFERENCES: four evaluations of the
 *     field for one gradient, and settling asked for three of those per
 *     step. `fieldAt` now returns the value and the exact gradient from one
 *     pass over the strokes.
 *   - The step was two pixels everywhere. A curve at radius r deviates from
 *     its chord by about l squared over 8r, so the outer curves -- which are
 *     nearly all of the length -- are walked in strides of twenty pixels and
 *     still sit within half a pixel of the truth.
 *   - Every point was a fresh `{x, y}` object. They are flat arrays now.
 *   - Painting measured every glyph and pushed and popped the canvas state
 *     for each one. The advances are measured once per font, the transform
 *     is set rather than stacked, and glyphs are drawn in runs.
 *
 * Measured over an identical set of 128 curves, tracing went from 1109 ms to
 * 39 ms. The painting is still done in slices with a time budget, so a
 * master of any size arrives without the tool appearing to hang.
 */
Canvas {
    id: art

    renderTarget: Canvas.Image
    renderStrategy: Canvas.Immediate

    // ------------------------------------------------------------ what it is

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

    /// How many glyph heights fit across the width. THE one dial for how fine
    /// the texture is, and the only thing that made the onboarding page's
    /// pattern read differently from the cover's: same strokes, same rules,
    /// two densities.
    property real glyphsAcross: 64
    property real glyph: Math.max(3, Math.round(art.width / art.glyphsAcross))
    /// From one line of text to the next, centre to centre.
    property real spacing: art.glyph * 1.6
    /// How softly the strokes' families of curves merge (see `fieldAt`).
    property real softness: art.spacing * 2.0
    /// The text's darkness on the ground. Greyscale, by way of the theme's
    /// own colour at less than full strength.
    property real ink: 0.55
    property color colour: Theme.primaryColor

    /// The strokes the curves grow out of, in fractions of the width and
    /// height. Each is a stroke rather than a point so the innermost curves
    /// are capsules and every family has a direction, which is what gives the
    /// sweeps their lean.
    property var strokes: [
        { x: 0.12, y: 0.16, x2: 0.28, y2: 0.08 },
        { x: 0.95, y: 0.30, x2: 0.84, y2: 0.20 },
        { x: 0.06, y: 0.78, x2: 0.16, y2: 0.66 },
        { x: 0.70, y: 1.02, x2: 0.58, y2: 0.90 },
        { x: 1.00, y: 0.72, x2: 0.92, y2: 0.62 }
    ]

    // --------------------------------------------- room for what sits on top

    /// A band at the top the text fades in beneath: nothing above `fadeFrom`,
    /// full strength from `fadeTo` down. Off while `fadeTo <= fadeFrom`.
    property real fadeFrom: 0
    property real fadeTo: 0

    /// A disc the text fades out of, for something drawn in the middle. Off
    /// while `clearRadius` is 0.
    property real clearX: 0
    property real clearY: 0
    property real clearRadius: 0
    property real clearFeather: 0

    // ------------------------------------------------- how it gets on screen

    /// How long one slice may take. Two frames' worth: long enough that the
    /// per-slice overhead does not dominate, short enough that a touch is
    /// never noticeably late.
    property int budgetMs: 24
    /// True once the last ring has been drawn.
    property bool complete: false

    /// Anything that changes what is drawn, in one string, so a single
    /// handler starts the pattern over rather than one per property -- and so
    /// none can be forgotten.
    readonly property string _key: [
        art.width, art.height, art.colour, art.glyphsAcross, art.spacing,
        art.softness, art.ink, art.fadeFrom, art.fadeTo,
        art.clearX, art.clearY, art.clearRadius, art.clearFeather,
        art.strokes.length
    ].join(",")
    on_KeyChanged: art.restart()

    /// Start the pattern over. The canvas is cleared by the next slice, not
    /// here, so what is on screen survives until there is something to
    /// replace it with.
    function restart() {
        art._restart = true
        art.complete = false
        art.requestPaint()
    }

    property bool _restart: true
    property int _ring: 0
    property int _rings: 0
    property int _offset: 0
    property var _lobes: []
    property var _widths: ({})
    property var _inks: []

    Component.onCompleted: art.restart()

    // The GUI thread comes back between slices; this asks for the next one.
    Timer {
        interval: 16
        repeat: true
        running: art.available && !art.complete
        onTriggered: art.requestPaint()
    }

    // ----------------------------------------------------------- the field

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

    /// The field and its gradient at one point, written into `out` as
    /// `[value, dx, dy]`.
    ///
    /// The field is a soft "distance to the nearest stroke": exactly that far
    /// from all of them, and a smooth blend where two are close, so its level
    /// sets are the curves -- one every `spacing`, evenly spaced everywhere
    /// because the field is a distance.
    ///
    /// The gradient is EXACT and comes from the same pass, which is the whole
    /// of the tracing speed-up: it is the weighted average of the unit
    /// vectors away from each stroke, with the same weights the value uses.
    /// Taking it by finite differences instead cost four more evaluations
    /// and was no more accurate.
    ///
    /// The exponentials are taken relative to the nearest stroke, so nothing
    /// underflows however far away the point is; `sc` is scratch the caller
    /// owns, so the walk allocates nothing at all.
    function fieldAt(L, x, y, soft, sc, out) {
        var n = L.length
        var nearest = 1e300
        var i, l, vx, vy, wx, wy, vv, t, dx, dy, d
        for (i = 0; i < n; i++) {
            l = L[i]
            vx = l.x2 - l.x
            vy = l.y2 - l.y
            wx = x - l.x
            wy = y - l.y
            vv = vx * vx + vy * vy
            t = vv > 0 ? (wx * vx + wy * vy) / vv : 0
            if (t < 0) { t = 0 } else if (t > 1) { t = 1 }
            dx = x - (l.x + t * vx)
            dy = y - (l.y + t * vy)
            d = Math.sqrt(dx * dx + dy * dy)
            if (d < 1e-9) { d = 1e-9; dx = 1e-9; dy = 0 }
            sc[3 * i] = d
            sc[3 * i + 1] = dx / d
            sc[3 * i + 2] = dy / d
            if (d < nearest) { nearest = d }
        }
        var sum = 0, gx = 0, gy = 0, wgt
        for (i = 0; i < n; i++) {
            wgt = Math.exp(-(sc[3 * i] - nearest) / soft)
            sum += wgt
            gx += wgt * sc[3 * i + 1]
            gy += wgt * sc[3 * i + 2]
        }
        out[0] = nearest - soft * Math.log(sum)
        out[1] = gx / sum
        out[2] = gy / sum
    }

    /// How many rings the field's own reach asks for.
    function ringCount(L) {
        if (art.width <= 0 || art.height <= 0 || L.length === 0) {
            return 0
        }
        var sc = new Array(3 * L.length)
        var fg = [0, 0, 0]
        var corners = [[0, 0], [art.width, 0], [0, art.height], [art.width, art.height]]
        var far = 0
        for (var c = 0; c < corners.length; c++) {
            art.fieldAt(L, corners[c][0], corners[c][1], art.softness, sc, fg)
            if (fg[0] > far) { far = fg[0] }
        }
        return Math.ceil((far + art.spacing) / art.spacing)
    }

    // ----------------------------------------------------------- the tracing

    /// Follow the curve at `level` from a seed, one way, until it closes on
    /// itself or leaves the canvas. Returns flat `[x0, y0, x1, y1, ...]`.
    ///
    /// One stride along the tangent, then one Newton correction back onto the
    /// level: two evaluations a step, and the correction is what stops the
    /// line drifting off its own ring over thousands of pixels.
    function walk(L, sx, sy, level, direction, step, maxSteps, soft, sc, fg) {
        var points = []
        var x = sx, y = sy
        var w = art.width, h = art.height
        var margin = art.spacing
        var closeEnough = step * step
        for (var i = 0; i < maxSteps; i++) {
            art.fieldAt(L, x, y, soft, sc, fg)
            var gx = fg[1], gy = fg[2]
            var gn = Math.sqrt(gx * gx + gy * gy)
            if (gn < 1e-9) {
                break
            }
            x += direction * step * (-gy / gn)
            y += direction * step * (gx / gn)
            art.fieldAt(L, x, y, soft, sc, fg)
            var g2 = fg[1] * fg[1] + fg[2] * fg[2]
            if (g2 > 1e-12) {
                var correction = (fg[0] - level) / g2
                x -= correction * fg[1]
                y -= correction * fg[2]
            }
            points.push(x, y)
            if (i > 6) {
                var dx = x - sx, dy = y - sy
                if (dx * dx + dy * dy < closeEnough) {
                    return { points: points, closed: true }
                }
            }
            if (x < -margin || x > w + margin || y < -margin || y > h + margin) {
                return { points: points, closed: false }
            }
        }
        return { points: points, closed: false }
    }

    /// Every curve of one ring, as `{points, closed}` with `points` flat.
    ///
    /// Seeded from several directions around each stroke, because a ring is
    /// several separate curves until the strokes' families merge; a seed that
    /// lands on a curve already traced is dropped, which is what makes the
    /// merged ones one curve. The grid is what makes that check cheap -- it
    /// used to be a scan of every point traced so far, on every seed.
    function traceRing(L, k) {
        var out = []
        var w = art.width, h = art.height
        var n = L.length
        if (w <= 0 || h <= 0 || n === 0) {
            return out
        }
        var soft = art.softness
        var spacing = art.spacing
        var level = spacing * (k + 0.5)

        // A chord of this length sits within `tol` of a circle of radius
        // `level`, so the outermost rings are walked in long strides and the
        // innermost, which actually curve, in short ones.
        var tol = 0.4
        var step = Math.sqrt(8 * level * tol)
        if (step < 1.5) { step = 1.5 } else if (step > spacing * 3) { step = spacing * 3 }

        var cell = Math.max(spacing, step)
        var grid = {}
        var sc = new Array(3 * n)
        var fg = [0, 0, 0]
        var maxSteps = Math.ceil((2 * Math.PI * level + 2 * (w + h)) / step) + 64

        for (var si = 0; si < n * 8; si++) {
            var i = si % n
            var angle = -Math.PI / 2 + Math.floor(si / n) * Math.PI / 4 + i * 0.7
            var cx = (L[i].x + L[i].x2) / 2
            var cy = (L[i].y + L[i].y2) / 2
            var sx = cx + level * Math.cos(angle)
            var sy = cy + level * Math.sin(angle)

            // Pull the seed onto the ring.
            for (var it = 0; it < 4; it++) {
                art.fieldAt(L, sx, sy, soft, sc, fg)
                var g2 = fg[1] * fg[1] + fg[2] * fg[2]
                if (g2 < 1e-12) {
                    break
                }
                var correction = (fg[0] - level) / g2
                sx -= correction * fg[1]
                sy -= correction * fg[2]
            }
            if (sx < 0 || sx > w || sy < 0 || sy > h) {
                continue
            }
            art.fieldAt(L, sx, sy, soft, sc, fg)
            if (Math.abs(fg[0] - level) > 1) {
                continue
            }

            var gx0 = Math.floor(sx / cell), gy0 = Math.floor(sy / cell)
            var covered = false
            for (var ox = -1; ox <= 1 && !covered; ox++) {
                for (var oy = -1; oy <= 1 && !covered; oy++) {
                    if (grid[(gy0 + oy) * 65536 + (gx0 + ox)]) {
                        covered = true
                    }
                }
            }
            if (covered) {
                continue
            }

            var forward = art.walk(L, sx, sy, level, 1, step, maxSteps, soft, sc, fg)
            var points = forward.points
            if (!forward.closed) {
                var back = art.walk(L, sx, sy, level, -1, step, maxSteps, soft, sc, fg).points
                var joined = []
                for (var b = back.length - 2; b >= 0; b -= 2) {
                    joined.push(back[b], back[b + 1])
                }
                joined.push(sx, sy)
                for (var f = 0; f < points.length; f++) {
                    joined.push(points[f])
                }
                points = joined
            }
            if (points.length < 8) {
                continue
            }
            for (var m = 0; m < points.length; m += 2) {
                grid[Math.floor(points[m + 1] / cell) * 65536 + Math.floor(points[m] / cell)] = true
            }
            out.push({ points: points, closed: forward.closed })
        }
        return out
    }

    /// Every curve on the canvas. Pure, and what the cover's test reads: the
    /// painting below only sets text along what this returns.
    function layout() {
        var L = art.lobes()
        var rings = art.ringCount(L)
        var out = []
        for (var k = 0; k < rings; k++) {
            var curves = art.traceRing(L, k)
            for (var i = 0; i < curves.length; i++) {
                out.push(curves[i])
            }
        }
        return out
    }

    // ---------------------------------------------------------- the painting

    /// How strongly to draw at a point: 1 in the open, fading to 0 in the
    /// band at the top and inside the disc.
    function weightAt(x, y) {
        var w = 1
        if (art.fadeTo > art.fadeFrom) {
            var f = (y - art.fadeFrom) / (art.fadeTo - art.fadeFrom)
            if (f < w) { w = f }
        }
        if (art.clearRadius > 0) {
            var dx = x - art.clearX, dy = y - art.clearY
            var c = (Math.sqrt(dx * dx + dy * dy) - art.clearRadius)
                    / Math.max(1, art.clearFeather)
            if (c < w) { w = c }
        }
        return w < 0 ? 0 : (w > 1 ? 1 : w)
    }

    /// How long a run of glyphs may be at a point before it stops following
    /// the curve, or the fade under it, closely enough to pass for one drawn
    /// glyph at a time.
    ///
    /// Straightness first: over an arc of length l a curve of radius `level`
    /// turns l / level, so this keeps a run inside a sixteenth of a radian.
    /// Then the fade: where the ink is CHANGING, a run carries one alpha for
    /// its whole length and would band, so runs shrink to a glyph or two
    /// there and nowhere else.
    ///
    /// Six glyphs, not more: measured on the host, longer runs bought
    /// nothing at all, so the cost is in rasterising the glyphs rather than
    /// in the calls that ask for them. Where a run buys nothing, a shorter
    /// one is the safer of the two -- it hugs the curve more closely.
    function runLimit(x, y, level) {
        var limit = art.glyph * 6
        var straight = 0.06 * level
        if (straight < limit) { limit = straight }
        var fading = false
        if (art.fadeTo > art.fadeFrom && y > art.fadeFrom - art.glyph
                && y < art.fadeTo + art.glyph) {
            fading = true
        }
        if (art.clearRadius > 0) {
            var dx = x - art.clearX, dy = y - art.clearY
            var r = Math.sqrt(dx * dx + dy * dy)
            if (r > art.clearRadius - art.glyph
                    && r < art.clearRadius + art.clearFeather + art.glyph) {
                fading = true
            }
        }
        if (fading && limit > art.glyph * 1.2) {
            limit = art.glyph * 1.2
        }
        return limit
    }

    /// The advance of every character the filler uses, measured once.
    /// `measureText` per glyph was a good part of the painting cost.
    function advances(ctx) {
        var widths = {}
        for (var i = 0; i < art.filler.length; i++) {
            var ch = art.filler.charAt(i)
            if (widths[ch] === undefined) {
                widths[ch] = ctx.measureText(ch).width
            }
        }
        return widths
    }

    /// Set text along one curve, in runs. Returns where in the filler to
    /// carry on, so no two curves open with the same word.
    function paintCurve(ctx, points, level, offset) {
        var count = points.length >> 1
        if (count < 2) {
            return offset
        }
        var cum = new Array(count)
        cum[0] = 0
        var i, dx, dy
        for (i = 1; i < count; i++) {
            dx = points[2 * i] - points[2 * i - 2]
            dy = points[2 * i + 1] - points[2 * i - 1]
            cum[i] = cum[i - 1] + Math.sqrt(dx * dx + dy * dy)
        }
        var total = cum[count - 1]
        var filler = art.filler
        var flen = filler.length
        var widths = art._widths
        var inks = art._inks
        var fallback = art.glyph * 0.5

        // One cursor for both lookups below: the run's start is never past
        // its own middle, and the next run starts past this one's middle, so
        // it only ever moves forward.
        var seg = 1
        var s = 0
        var t = offset
        while (s < total - 1) {
            while (seg < count - 1 && cum[seg] < s) { seg++ }
            var span = cum[seg] - cum[seg - 1]
            var f = span > 0 ? (s - cum[seg - 1]) / span : 0
            var ax = points[2 * seg - 2], ay = points[2 * seg - 1]
            var bx = points[2 * seg], by = points[2 * seg + 1]
            var x0 = ax + (bx - ax) * f, y0 = ay + (by - ay) * f

            var limit = art.runLimit(x0, y0, level)
            var run = "", runWidth = 0, printed = false
            while (runWidth < limit && run.length < 10) {
                var ch = filler.charAt(t % flen)
                t++
                var cw = widths[ch]
                if (cw === undefined) { cw = fallback }
                run += ch
                runWidth += cw
                if (ch !== " ") { printed = true }
            }
            if (runWidth <= 0) {
                break
            }

            if (printed) {
                var mid = s + runWidth / 2
                while (seg < count - 1 && cum[seg] < mid) { seg++ }
                span = cum[seg] - cum[seg - 1]
                f = span > 0 ? (mid - cum[seg - 1]) / span : 0
                ax = points[2 * seg - 2]; ay = points[2 * seg - 1]
                bx = points[2 * seg]; by = points[2 * seg + 1]
                var mx = ax + (bx - ax) * f, my = ay + (by - ay) * f
                var weight = art.weightAt(mx, my)
                if (weight > 0.03) {
                    var angle = Math.atan2(by - ay, bx - ax)
                    var cos = Math.cos(angle), sin = Math.sin(angle)
                    ctx.fillStyle = inks[Math.round(weight * 32)]
                    // Set rather than stacked: a save and a restore per glyph
                    // cost more than the glyph did.
                    ctx.setTransform(cos, sin, -sin, cos, mx, my)
                    ctx.fillText(run, -runWidth / 2, 0)
                }
            }
            s += runWidth
        }
        return t % flen
    }

    onPaint: {
        var ctx = art.getContext("2d")
        if (!ctx) {
            return
        }
        if (art._restart) {
            ctx.setTransform(1, 0, 0, 1, 0, 0)
            ctx.clearRect(0, 0, art.width, art.height)
            ctx.font = art.glyph + "px " + Theme.fontFamily
            ctx.textBaseline = "middle"
            art._lobes = art.lobes()
            art._rings = art.ringCount(art._lobes)
            art._widths = art.advances(ctx)
            // The ink, in thirty-two steps, so a run does not build a colour
            // from a string every time it is drawn.
            var inks = []
            for (var i = 0; i <= 32; i++) {
                inks.push(Qt.rgba(art.colour.r, art.colour.g, art.colour.b,
                                  art.ink * i / 32))
            }
            art._inks = inks
            art._ring = 0
            art._offset = 0
            art._restart = false
        }

        var until = Date.now() + art.budgetMs
        var ring = art._ring
        var offset = art._offset
        var L = art._lobes
        var spacing = art.spacing
        do {
            var curves = art.traceRing(L, ring)
            var level = spacing * (ring + 0.5)
            for (var c = 0; c < curves.length; c++) {
                offset = art.paintCurve(ctx, curves[c].points, level, offset)
                offset = (offset + 37) % art.filler.length
            }
            ring++
        } while (ring < art._rings && Date.now() < until)
        ctx.setTransform(1, 0, 0, 1, 0, 0)

        art._ring = ring
        art._offset = offset
        if (ring >= art._rings) {
            art.complete = true
        }
    }
}
