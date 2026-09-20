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
 * # Negative space
 *
 * The cover's count is not drawn on the pattern; it is where the pattern is
 * not. Set `obstacle` and the digits become a second source for the field
 * (see `obstacleField`): their outside is measured by an exact distance
 * transform, and that distance is joined to the strokes' by a polynomial
 * smooth minimum. The first rings hug every edge of every digit, the
 * counter of a 4 included, and a few rings out the lines have forgotten the
 * number and are the usual sweeps. Nothing is traced inside the digits,
 * because the field is below the first level there; no case is made of it.
 *
 * The join is a polynomial smooth-min and not the log-sum-exp the strokes
 * use among themselves, and the difference is visible. Log-sum-exp lowers
 * the field everywhere two sources compete, by up to `soft * ln(n)`, which
 * around the digits opened a wide empty band between the first ring and the
 * rest. The polynomial is exact away from its seam, so the digit rings are
 * spaced right and the sweeps are untouched, and it rounds only where the
 * two families meet.
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
    /// The face the filler is set in. The device's own by default, which a
    /// host that is not a phone resolves to whatever fontconfig has; a
    /// `FontLoader` with the real file names it exactly (see render.qml).
    property string fillerFont: Theme.fontFamily

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

    // ------------------------------------------------- the negative space

    /// Text the lines flow AROUND rather than over: the cover's unread
    /// count. Empty for none. The digits are never drawn -- they are the
    /// absence of lines -- so a mask painted with one is right for exactly
    /// that count and no other.
    property string obstacle: ""
    /// The family the digits are set in, as a `FontLoader` names it. The
    /// count wants a heavy face: Fira Sans ExtraBold. Black was tried and
    /// rejected, because the counter of the 4 closes up at that weight and
    /// the digit reads as a solid shape.
    property string obstacleFont: ""
    /// Ink to ink between adjacent digits, in line spacings. At the font's
    /// own spacing fewer than one line fits between a 4 and a 2, and the two
    /// merge into one shape.
    property real obstacleGap: 2.6
    /// The most of the fitted width, and of the usable height, the digits'
    /// ink may take. Width first: a 99+ is wide, and it is the width that
    /// binds.
    property real obstacleMaxWidth: 0.80
    property real obstacleMaxHeight: 0.62
    /// The narrowest aspect (width over height) this master is cropped to
    /// on a device, or 0 for the master's own. The app's shader keeps the
    /// mask's full height and trims its sides to the cover's shape, so the
    /// digits are fitted to the width that SURVIVES that trim, or a wide
    /// count would be clipped on a narrow cover.
    property real fitAspect: 0
    /// Height at the bottom that something else occupies -- the cover's
    /// action area. The digits are centred in what is left above it.
    property real reservedBottom: 0
    /// Where the first ring sits, in spacings from its source. Half a
    /// spacing is the painter's own; the cover's masters use 0.6, which
    /// moves the first ring a little off the digit edge.
    property real levelOffset: 0.5
    /// Which way the tops of the glyphs face: their nearest source, or away
    /// from it. The painter's sweeps have always faced away; the cover's
    /// masters face in, so the lines beneath the number read upright.
    property bool topsFaceSource: false
    /// A halo: the lines are painted at full strength where they touch the
    /// digits and ease down to `farInk` of it over `haloReach` line
    /// spacings, so the number is the brightest thing on the cover and the
    /// sweeps recede from it. Off while `haloReach` is 0, or with no
    /// obstacle. Baked into the mask, since the app's shader knows nothing
    /// of where the digits are; the app then draws the mask at full ink and
    /// the far lines come out as they did at `farInk`.
    property real farInk: 1.0
    property real haloReach: 0

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
        art.softness, art.ink, art.fillerFont, art.fadeFrom, art.fadeTo,
        art.clearX, art.clearY, art.clearRadius, art.clearFeather,
        art.strokes.length,
        art.obstacle, art.obstacleFont, art.obstacleGap, art.obstacleMaxWidth,
        art.obstacleMaxHeight, art.fitAspect, art.reservedBottom,
        art.levelOffset, art.topsFaceSource, art.farInk, art.haloReach
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
    property var _obstacle: null
    property string _obstacleKey: ""
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

    /// Where the digits are rasterised to be measured, one at a time. Never
    /// shown; `obstacleField` reads its pixels back. It is the painter's
    /// size so that no digit the fit allows can fall off its edge.
    Canvas {
        id: stencil
        width: art.width
        height: art.height
        visible: false
        renderTarget: Canvas.Image
        renderStrategy: Canvas.Immediate
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

    // ------------------------------------------------- the negative space

    /// One character, drawn alone at `px` and cropped to its ink. Returns
    /// where the ink is relative to the baseline it was drawn on, and the
    /// coverage inside that box, or null for a glyph with no ink.
    ///
    /// The ink and not the advance box: the advance of a 4 has air on both
    /// sides that a 2's does not, and centring advances would centre the
    /// number a few pixels off where it appears to be.
    function inkOf(ctx, ch, px) {
        var w = stencil.width, h = stencil.height
        var x0 = Math.round(w * 0.1)
        var y0 = Math.round(h * 0.75)
        ctx.setTransform(1, 0, 0, 1, 0, 0)
        ctx.clearRect(0, 0, w, h)
        ctx.font = px + "px \"" + art.obstacleFont + "\""
        ctx.textBaseline = "alphabetic"
        ctx.fillStyle = "#ffffff"
        ctx.fillText(ch, x0, y0)
        // Read back the region the glyph can reach rather than the whole
        // canvas; the ascent and the side bearings are bounded by the size.
        var advance = ctx.measureText(ch).width
        var rx = Math.max(0, Math.floor(x0 - px * 0.5))
        var ry = Math.max(0, Math.floor(y0 - px * 1.2))
        var rw = Math.min(w - rx, Math.ceil(advance + px))
        var rh = Math.min(h - ry, Math.ceil(px * 1.7))
        var data = ctx.getImageData(rx, ry, rw, rh).data
        var minX = rw, minY = rh, maxX = -1, maxY = -1
        for (var y = 0; y < rh; y++) {
            for (var x = 0; x < rw; x++) {
                if (data[(y * rw + x) * 4 + 3] >= 128) {
                    if (x < minX) { minX = x }
                    if (x > maxX) { maxX = x }
                    if (y < minY) { minY = y }
                    if (y > maxY) { maxY = y }
                }
            }
        }
        if (maxX < 0) {
            return null
        }
        var bw = maxX - minX + 1, bh = maxY - minY + 1
        var bits = new Uint8Array(bw * bh)
        for (var yy = 0; yy < bh; yy++) {
            for (var xx = 0; xx < bw; xx++) {
                bits[yy * bw + xx] =
                    data[((yy + minY) * rw + xx + minX) * 4 + 3] >= 128 ? 1 : 0
            }
        }
        return { width: bw, height: bh,
                 top: ry + minY - y0, bottom: ry + maxY - y0,
                 bits: bits }
    }

    /// One dimension of the squared distance transform, Felzenszwalb and
    /// Huttenlocher's: the lower envelope of the parabolas each sample
    /// raises, in one pass each way. `f` in, `d` out, `v` and `z` scratch.
    function edt1d(f, n, d, v, z) {
        var k = 0
        v[0] = 0
        z[0] = -1e300
        z[1] = 1e300
        var q, s
        for (q = 1; q < n; q++) {
            s = ((f[q] + q * q) - (f[v[k]] + v[k] * v[k])) / (2 * q - 2 * v[k])
            while (s <= z[k]) {
                k--
                s = ((f[q] + q * q) - (f[v[k]] + v[k] * v[k])) / (2 * q - 2 * v[k])
            }
            k++
            v[k] = q
            z[k] = s
            z[k + 1] = 1e300
        }
        k = 0
        for (q = 0; q < n; q++) {
            while (z[k + 1] < q) { k++ }
            d[q] = (q - v[k]) * (q - v[k]) + f[v[k]]
        }
    }

    /// The Euclidean distance from every pixel to the nearest set one of
    /// `inside`, exact, in linear time. Zero on the set pixels.
    function distanceOutside(inside, w, h) {
        var n = w * h
        var sq = new Float64Array(n)
        var i
        for (i = 0; i < n; i++) {
            sq[i] = inside[i] ? 0 : 1e20
        }
        var m = Math.max(w, h)
        var f = new Float64Array(m), d = new Float64Array(m)
        var v = new Int32Array(m), z = new Float64Array(m + 1)
        var x, y
        for (x = 0; x < w; x++) {
            for (y = 0; y < h; y++) { f[y] = sq[y * w + x] }
            art.edt1d(f, h, d, v, z)
            for (y = 0; y < h; y++) { sq[y * w + x] = d[y] }
        }
        for (y = 0; y < h; y++) {
            for (x = 0; x < w; x++) { f[x] = sq[y * w + x] }
            art.edt1d(f, w, d, v, z)
            for (x = 0; x < w; x++) { sq[y * w + x] = d[x] }
        }
        var out = new Float32Array(n)
        for (i = 0; i < n; i++) {
            out[i] = Math.sqrt(sq[i])
        }
        return out
    }

    /// A Gaussian blur of `sigma` pixels, separable, edges clamped. Without
    /// it the inside corners of a 4 put sharp creases in every ring that
    /// passes them.
    function blurred(src, w, h, sigma) {
        var r = Math.ceil(sigma * 3)
        if (r < 1) {
            return src
        }
        var kernel = new Float64Array(2 * r + 1)
        var sum = 0, i
        for (i = -r; i <= r; i++) {
            kernel[i + r] = Math.exp(-(i * i) / (2 * sigma * sigma))
            sum += kernel[i + r]
        }
        for (i = 0; i < kernel.length; i++) { kernel[i] /= sum }
        var tmp = new Float32Array(w * h)
        var out = new Float32Array(w * h)
        var x, y, acc, xx, yy
        for (y = 0; y < h; y++) {
            for (x = 0; x < w; x++) {
                acc = 0
                for (i = -r; i <= r; i++) {
                    xx = x + i
                    if (xx < 0) { xx = 0 } else if (xx >= w) { xx = w - 1 }
                    acc += kernel[i + r] * src[y * w + xx]
                }
                tmp[y * w + x] = acc
            }
        }
        for (y = 0; y < h; y++) {
            for (x = 0; x < w; x++) {
                acc = 0
                for (i = -r; i <= r; i++) {
                    yy = y + i
                    if (yy < 0) { yy = 0 } else if (yy >= h) { yy = h - 1 }
                    acc += kernel[i + r] * tmp[yy * w + x]
                }
                out[y * w + x] = acc
            }
        }
        return out
    }

    /// The digits as a source for the field: the distance to them from
    /// every pixel outside them, and where their ink is. Null when there is
    /// no obstacle. Built once per `_key` and kept, since tracing asks for
    /// it thousands of times a ring.
    ///
    /// The digits are set one at a time, each cropped to its ink, all scaled
    /// by ONE factor, and joined with `obstacleGap` spacings between the ink
    /// of one and the next. The block's ink bounds are centred across the
    /// width that survives the narrowest crop, and centred vertically in
    /// the height above `reservedBottom`.
    function obstacleField(L) {
        if (art.obstacle.length === 0) {
            return null
        }
        if (art._obstacle && art._obstacleKey === art._key) {
            return art._obstacle
        }
        var w = art.width, h = art.height
        var ctx = stencil.getContext("2d")
        if (!ctx) {
            console.log("the stencil canvas is not available yet")
            return null
        }
        var spacing = art.spacing
        var gap = Math.round(art.obstacleGap * spacing)
        var fitW = art.fitAspect > 0 ? Math.min(w, h * art.fitAspect) : w
        var usableH = h - art.reservedBottom
        var chars = art.obstacle.split("")
        var n = chars.length

        // Measured at a trial size, then set at the one size that fits.
        var trial = Math.max(8, Math.round(usableH * art.obstacleMaxHeight))
        var inkW = 0, inkH = 0, i, g
        for (i = 0; i < n; i++) {
            g = art.inkOf(ctx, chars[i], trial)
            if (g) {
                inkW += g.width
                if (g.height > inkH) { inkH = g.height }
            }
        }
        var roomW = art.obstacleMaxWidth * fitW - (n - 1) * gap
        var roomH = art.obstacleMaxHeight * usableH
        var scale = Math.min(inkW > 0 ? roomW / inkW : 1, inkH > 0 ? roomH / inkH : 1)
        var px = Math.max(8, Math.floor(trial * scale))

        var glyphs = []
        var total = 0, top = 1e9, bottom = -1e9
        for (i = 0; i < n; i++) {
            g = art.inkOf(ctx, chars[i], px)
            glyphs.push(g)
            if (g) {
                total += g.width
                if (g.top < top) { top = g.top }
                if (g.bottom > bottom) { bottom = g.bottom }
            }
        }
        total += (n - 1) * gap
        ctx.clearRect(0, 0, w, h)

        var left = Math.round((w - total) / 2)
        var baseline = Math.round(usableH / 2 - (top + bottom) / 2)
        var inside = new Uint8Array(w * h)
        var cursor = left
        var x, y, gx, gy
        for (i = 0; i < n; i++) {
            g = glyphs[i]
            if (!g) {
                continue
            }
            for (y = 0; y < g.height; y++) {
                gy = baseline + g.top + y
                if (gy < 0 || gy >= h) { continue }
                for (x = 0; x < g.width; x++) {
                    if (g.bits[y * g.width + x]) {
                        gx = cursor + x
                        if (gx >= 0 && gx < w) {
                            inside[gy * w + gx] = 1
                        }
                    }
                }
            }
            cursor += g.width + gap
        }

        var dd = art.distanceOutside(inside, w, h)
        // Two pixels at the width the values were settled at (936).
        dd = art.blurred(dd, w, h, 2 * w / 936)

        var O = {
            w: w, h: h, dd: dd,
            k: 1.5 * spacing,
            left: left, top: baseline + top,
            right: left + total - 1, bottom: baseline + bottom,
            px: px,
            samples: null, sampleCell: 0, sampleCols: 0, sampleRows: 0
        }

        // The field at a coarse grid over the whole canvas, so a ring can
        // be seeded wherever it runs -- around the digits, inside a
        // counter -- and not only around the strokes it used to grow from.
        var cell = spacing * 0.5
        var cols = Math.floor(w / cell) + 1, rows = Math.floor(h / cell) + 1
        var samples = new Float32Array(cols * rows)
        var sc = new Array(3 * L.length)
        var fg = [0, 0, 0]
        for (var r = 0; r < rows; r++) {
            for (var c = 0; c < cols; c++) {
                art.fieldAt(L, O, c * cell, r * cell, art.softness, sc, fg)
                samples[r * cols + c] = fg[0]
            }
        }
        O.samples = samples
        O.sampleCell = cell
        O.sampleCols = cols
        O.sampleRows = rows

        art._obstacle = O
        art._obstacleKey = art._key
        return O
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
    ///
    /// With an obstacle `O`, the strokes' value is then joined to the
    /// distance from the digits by a polynomial smooth minimum of width
    /// `O.k`: exact wherever one of the two is more than `k` nearer, rounded
    /// only at the seam. The digits' distance is sampled bilinearly and its
    /// gradient taken from the same four texels. The gradient of the join
    /// is `mix` of the two gradients by the same weight -- exactly, not
    /// approximately: the derivative of the `k h (1 - h)` term cancels
    /// against the weight's own, so the Newton correction converges at the
    /// seam as well as it does anywhere.
    function fieldAt(L, O, x, y, soft, sc, out) {
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
        var sv = nearest - soft * Math.log(sum)
        var sgx = gx / sum, sgy = gy / sum
        if (!O) {
            out[0] = sv
            out[1] = sgx
            out[2] = sgy
            return
        }
        var w = O.w, h = O.h, dd = O.dd
        var fx = x, fy = y
        if (fx < 0) { fx = 0 } else if (fx > w - 1.001) { fx = w - 1.001 }
        if (fy < 0) { fy = 0 } else if (fy > h - 1.001) { fy = h - 1.001 }
        var ix = Math.floor(fx), iy = Math.floor(fy)
        var tx = fx - ix, ty = fy - iy
        var i00 = iy * w + ix
        var v00 = dd[i00], v10 = dd[i00 + 1], v01 = dd[i00 + w], v11 = dd[i00 + w + 1]
        var dv = (1 - ty) * ((1 - tx) * v00 + tx * v10) + ty * ((1 - tx) * v01 + tx * v11)
        var dgx = (1 - ty) * (v10 - v00) + ty * (v11 - v01)
        var dgy = (1 - tx) * (v01 - v00) + tx * (v11 - v10)
        var k = O.k
        var hh = 0.5 + 0.5 * (sv - dv) / k
        if (hh < 0) { hh = 0 } else if (hh > 1) { hh = 1 }
        out[0] = sv + (dv - sv) * hh - k * hh * (1 - hh)
        out[1] = sgx + (dgx - sgx) * hh
        out[2] = sgy + (dgy - sgy) * hh
    }

    /// How many rings the field's own reach asks for.
    ///
    /// The corners are enough for the strokes alone. With an obstacle the
    /// field is a minimum of two distances, and the farthest point from
    /// both can be anywhere between them, so a coarse grid is asked too.
    function ringCount(L, O) {
        if (art.width <= 0 || art.height <= 0 || L.length === 0) {
            return 0
        }
        var sc = new Array(3 * L.length)
        var fg = [0, 0, 0]
        var corners = [[0, 0], [art.width, 0], [0, art.height], [art.width, art.height]]
        var far = 0
        for (var c = 0; c < corners.length; c++) {
            art.fieldAt(L, O, corners[c][0], corners[c][1], art.softness, sc, fg)
            if (fg[0] > far) { far = fg[0] }
        }
        if (O) {
            for (var i = 0; i < O.samples.length; i++) {
                if (O.samples[i] > far) { far = O.samples[i] }
            }
        }
        return Math.ceil((far + art.spacing) / art.spacing)
    }

    // ----------------------------------------------------------- the tracing

    /// Follow the curve at `level` from a seed, one way, until it closes on
    /// itself, leaves the canvas, or reaches ground an earlier curve of this
    /// ring has already covered. Returns flat `[x0, y0, x1, y1, ...]`.
    ///
    /// One stride along the tangent, then one Newton correction back onto the
    /// level: two evaluations a step, and the correction is what stops the
    /// line drifting off its own ring over thousands of pixels.
    ///
    /// `covered` is where the ring's earlier curves have been (see
    /// `traceRing`), and stopping when this one reaches them is what keeps a
    /// ring from being TRACED TWICE. A device reported the pattern
    /// overlapping itself in a couple of places: two seeds on one ring, the
    /// second of them just beyond the end of the curve the first had drawn
    /// -- so the grid, which was only ever asked about the SEED, let it
    /// through, and it retraced seven hundred pixels of the same line two
    /// pixels to the side. Asking on every stride is the whole fix: a later
    /// curve fills whatever is left of its ring and stops where the pattern
    /// is already there.
    ///
    /// The question it asks is the same one `overlaps` asks afterwards --
    /// "is there already ink within `near` of here" -- so the two cannot
    /// disagree about what counts as drawing twice, and the curves meet
    /// rather than overlapping by however coarse a grid cell happens to be.
    ///
    /// `own` is the same question asked of THIS curve: a ring inside the
    /// counter of a 4 follows both sides of the triangle up into its apex,
    /// where the two sides of one curve come closer than a line of text --
    /// which `overlaps` does not count, since a curve is allowed near
    /// itself around a tight cap, and which shipped as text over text in
    /// every 4. Points go into `own` `skip` strides late, and the first
    /// `skip` never do, so a curve is never stopped by the stride it just
    /// took or kept from closing on its own start. The walk back from a
    /// seed gets the forward walk's points in `own` to begin with.
    function walk(L, O, sx, sy, level, direction, step, maxSteps, soft, sc, fg,
                  covered, own, cell, near) {
        var points = []
        var x = sx, y = sy
        var w = art.width, h = art.height
        var margin = art.spacing
        var closeEnough = step * step
        var skip = Math.ceil(near / step) + 2
        // How far off its level a point may be and still be kept: a
        // twentieth of a spacing, so two rings a spacing apart can never
        // draw closer than nine tenths of one, whatever their walks did.
        // The seeds are held to a pixel; this is the same idea for a point
        // with a stride behind it.
        var tolerance = art.spacing * 0.05
        var reach, correction, g2
        art.fieldAt(L, O, x, y, soft, sc, fg)
        for (var i = 0; i < maxSteps; i++) {
            // `fg` is the field at (x, y), which is on the level.
            var gx = fg[1], gy = fg[2]
            var gn = Math.sqrt(gx * gx + gy * gy)
            if (gn < 1e-9) {
                break
            }
            var px = x, py = y
            x += direction * step * (-gy / gn)
            y += direction * step * (gx / gn)
            // Back onto the level: one Newton correction, then a look at
            // where it landed, and up to three more if it is not there yet.
            // Nearly always it is, and the look is the one extra evaluation
            // a stride costs now. Where it is not -- a tight bend inside
            // the pocket of a 2, or the seam where the digits' rings meet
            // the strokes' -- a point kept a few pixels off its ring was
            // kept in the NEXT ring's room, and that shipped as text over
            // text. If three more corrections will not do it, the walk has
            // lost its ring and ends here rather than drawing off it.
            var lost = false
            for (var settle = 0; ; settle++) {
                art.fieldAt(L, O, x, y, soft, sc, fg)
                if (Math.abs(fg[0] - level) <= tolerance) {
                    break
                }
                g2 = fg[1] * fg[1] + fg[2] * fg[2]
                if (settle >= 3 || g2 < 1e-12) {
                    lost = true
                    break
                }
                correction = (fg[0] - level) / g2
                // Never further than the stride itself. The strokes' field
                // is nearly a distance, whose gradient is a unit vector, so
                // this never bound the correction before; the digits' field
                // has RIDGES -- the middle of the counter of a 4, the pocket
                // inside a 2 -- where the blurred distance flattens, the
                // gradient goes to nothing and one correction threw the
                // walk clean across the pocket, where it piled a few dozen
                // points on one spot of somebody else's ring.
                reach = Math.abs(correction) * Math.sqrt(g2)
                if (reach > step) {
                    correction *= step / reach
                }
                x -= correction * fg[1]
                y -= correction * fg[2]
            }
            if (lost) {
                return { points: points, closed: false }
            }
            // A walk that no longer gets anywhere is at such a ridge, where
            // the level set pinches to a point: its curve ends here.
            var mx = x - px, my = y - py
            if (mx * mx + my * my < step * step * 0.04) {
                return { points: points, closed: false }
            }
            // BEFORE the point is kept, not after: a stride is as long as
            // the ring's own curvature allows, so a curve can go from a
            // comfortable distance to touching in one of them. Keeping the
            // point that stopped the walk is what left a few overlaps behind
            // when this stopped on grid cells instead. No `i > 6` guard is
            // needed -- `covered` holds only the curves traced BEFORE this
            // one, so there is nothing of its own to trip on.
            if (art.inked(covered, cell, x, y, near)) {
                return { points: points, closed: false }
            }
            if (i > 6) {
                var dx = x - sx, dy = y - sy
                if (dx * dx + dy * dy < closeEnough) {
                    points.push(x, y)
                    return { points: points, closed: true }
                }
            }
            if (art.inked(own, cell, x, y, near)) {
                return { points: points, closed: false }
            }
            points.push(x, y)
            var lag = i - skip
            if (lag >= skip) {
                art.markInk(own, cell, points[2 * lag], points[2 * lag + 1])
            }
            if (x < -margin || x > w + margin || y < -margin || y > h + margin) {
                return { points: points, closed: false }
            }
        }
        return { points: points, closed: false }
    }

    /// Whether any point already recorded in `grid` is within `near` of
    /// (x, y). The grid buckets points by `cell`, and `near` is never more
    /// than a cell, so the nine around it are all that can hold one.
    function inked(grid, cell, x, y, near) {
        var gx = Math.floor(x / cell), gy = Math.floor(y / cell)
        var limit = near * near
        for (var ox = -1; ox <= 1; ox++) {
            for (var oy = -1; oy <= 1; oy++) {
                var bucket = grid[(gy + oy) * 65536 + (gx + ox)]
                if (!bucket) {
                    continue
                }
                for (var i = 0; i < bucket.length; i += 2) {
                    var dx = x - bucket[i], dy = y - bucket[i + 1]
                    if (dx * dx + dy * dy < limit) {
                        return true
                    }
                }
            }
        }
        return false
    }

    /// Record a point as drawn, for `inked` to find.
    ///
    /// Not `ink`: that is the property saying how dark the text is, and a
    /// function of the same name is simply not reachable -- the call comes
    /// back "TypeError", because it is asking a number to be a function.
    function markInk(grid, cell, x, y) {
        var key = Math.floor(y / cell) * 65536 + Math.floor(x / cell)
        if (!grid[key]) {
            grid[key] = []
        }
        grid[key].push(x, y)
    }

    /// Every curve of one ring, as `{points, closed}` with `points` flat.
    ///
    /// Seeded from several directions around each stroke, because a ring is
    /// several separate curves until the strokes' families merge; a seed that
    /// lands on a curve already traced is dropped, which is what makes the
    /// merged ones one curve. The grid is what makes that check cheap -- it
    /// used to be a scan of every point traced so far, on every seed.
    function traceRing(L, O, k) {
        var out = []
        var w = art.width, h = art.height
        var n = L.length
        if (w <= 0 || h <= 0 || n === 0) {
            return out
        }
        var soft = art.softness
        var spacing = art.spacing
        var level = spacing * (k + art.levelOffset)

        // A chord of this length sits within `tol` of a circle of radius
        // `level`, so the innermost rings, which actually curve, are walked
        // in short strides.
        //
        // The CAP is what matters on the outer ones, and it is half a
        // spacing rather than three: the checks that stop two curves being
        // drawn over one another compare sampled POINTS, so a stride long
        // enough to cross another curve between two of its own samples hides
        // the crossing from them -- which is exactly what shipped, text over
        // text at a shallow angle with every sample comfortably far from
        // every other. Two segments that cross have endpoints within a
        // stride of the crossing, so a stride under half the spacing cannot
        // hide one from a check that refuses 0.8 of it. This was a stride of
        // twenty pixels when the pattern was painted on the phone and the
        // arithmetic had to be cheap; it is painted here now, where it can
        // afford to be careful.
        var tol = 0.4
        var step = Math.sqrt(8 * level * tol)
        if (step < 1.5) { step = 1.5 } else if (step > spacing * 0.5) { step = spacing * 0.5 }

        // How close two bits of line may come before the later one gives
        // up: just under the spacing the rings are drawn at, because that
        // spacing IS the room a line of text needs. Half of it was tried and
        // is visibly wrong -- the glyphs are nearly as tall as the spacing,
        // so lines that pass within half of it collide even though no curve
        // is drawn twice. The seed check and the walk both mean this by
        // "already drawn", and no point that comes within it is ever kept,
        // so `overlaps` passes by construction rather than by luck.
        var near = spacing * 0.95
        // Big enough that the nine cells around a point hold everything
        // within `near` of it, and that a stride cannot skip over a cell.
        var cell = Math.max(spacing, step)
        var grid = {}
        var sc = new Array(3 * n)
        var fg = [0, 0, 0]
        var maxSteps = Math.ceil((2 * Math.PI * level + 2 * (w + h)) / step) + 64

        // Where to start looking for this ring's curves. Eight directions
        // around each stroke, as always; and, when there is an obstacle,
        // every point of the coarse grid the field was sampled on that lies
        // within half a spacing of this level -- which is how a ring that
        // wraps only the digits, or sits inside the counter of a 4 or a 9,
        // gets found at all. Seeds that land on a curve already traced are
        // dropped, so the extra ones cost nothing where the stroke seeds
        // were enough.
        var seeds = []
        var si, i
        for (si = 0; si < n * 8; si++) {
            i = si % n
            var angle = -Math.PI / 2 + Math.floor(si / n) * Math.PI / 4 + i * 0.7
            var cx = (L[i].x + L[i].x2) / 2
            var cy = (L[i].y + L[i].y2) / 2
            seeds.push(cx + level * Math.cos(angle), cy + level * Math.sin(angle))
        }
        if (O) {
            var band = spacing * 0.5
            for (var r = 0; r < O.sampleRows; r++) {
                for (var c = 0; c < O.sampleCols; c++) {
                    if (Math.abs(O.samples[r * O.sampleCols + c] - level) < band) {
                        seeds.push(c * O.sampleCell, r * O.sampleCell)
                    }
                }
            }
        }

        var crumb = art.glyph * 6
        for (si = 0; si < seeds.length; si += 2) {
            var sx = seeds[si], sy = seeds[si + 1]

            // Pull the seed onto the ring.
            for (var it = 0; it < 6; it++) {
                art.fieldAt(L, O, sx, sy, soft, sc, fg)
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
            art.fieldAt(L, O, sx, sy, soft, sc, fg)
            if (Math.abs(fg[0] - level) > 1) {
                continue
            }

            if (art.inked(grid, cell, sx, sy, near)) {
                continue
            }

            var forward = art.walk(L, O, sx, sy, level, 1, step, maxSteps, soft, sc, fg,
                                   grid, {}, cell, near)
            var points = forward.points
            if (!forward.closed) {
                // The way back must not run into the way out, except for
                // the first few strides beside the seed, which are its own
                // neighbours rather than another arm of the curve.
                var own = {}
                var skip = Math.ceil(near / step) + 2
                for (var o = 2 * skip; o < points.length; o += 2) {
                    art.markInk(own, cell, points[o], points[o + 1])
                }
                var back = art.walk(L, O, sx, sy, level, -1, step, maxSteps, soft, sc, fg,
                                    grid, own, cell, near).points
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
            // Crumbs: a closed loop at a stroke's core, or the sliver of a
            // ring left between two earlier curves, would carry one stray
            // word. Six glyph heights is the shortest run that reads as a
            // line of text rather than as litter.
            var length = 0
            for (var q = 2; q < points.length; q += 2) {
                var ex = points[q] - points[q - 2], ey = points[q + 1] - points[q - 1]
                length += Math.sqrt(ex * ex + ey * ey)
            }
            if (length < crumb) {
                continue
            }
            if (art.topsFaceSource && art.facesAway(L, O, points, soft, sc, fg)) {
                var flipped = []
                for (var v = points.length - 2; v >= 0; v -= 2) {
                    flipped.push(points[v], points[v + 1])
                }
                points = flipped
            }
            for (var m = 0; m < points.length; m += 2) {
                art.markInk(grid, cell, points[m], points[m + 1])
            }
            out.push({ points: points, closed: forward.closed })
        }
        return out
    }

    /// Whether the glyphs set along `points`, as `paintCurve` sets them,
    /// would have their tops facing the HIGHER field -- away from the curve's
    /// nearest source. Sampled at up to sixteen places along the curve and
    /// decided by the majority, since a curve can cross the seam between two
    /// families and briefly disagree with itself.
    ///
    /// `paintCurve` stands a glyph up on the left of its direction of
    /// travel, so along a direction (dx, dy) the tops point to (dy, -dx);
    /// against a gradient (gx, gy) that is the sign of dx * gy - dy * gx.
    function facesAway(L, O, points, soft, sc, fg) {
        var count = points.length >> 1
        var stride = Math.max(1, Math.floor(count / 16))
        var vote = 0
        for (var i = 1; i < count; i += stride) {
            var dx = points[2 * i] - points[2 * i - 2]
            var dy = points[2 * i + 1] - points[2 * i - 1]
            art.fieldAt(L, O, points[2 * i], points[2 * i + 1], soft, sc, fg)
            vote += (dx * fg[2] - dy * fg[1]) < 0 ? 1 : -1
        }
        return vote > 0
    }

    /// Where two DIFFERENT curves are drawn over one another, which is the
    /// one defect of this pattern a person notices: text on top of text.
    ///
    /// Returns how many pairs of points sit closer than half the line
    /// spacing. `make textart` refuses to ship a master that reports any,
    /// because that is exactly what shipped once -- a ring traced twice by
    /// two seeds. Pairs from the SAME curve are not counted: a curve comes
    /// legitimately near itself around a tight end cap, where a ring is a
    /// capsule barely wider than the stride that walks it.
    function overlaps() {
        var curves = art.layout()
        var xs = [], ys = [], owner = []
        for (var c = 0; c < curves.length; c++) {
            var p = curves[c].points
            for (var i = 0; i < p.length; i += 2) {
                xs.push(p[i])
                ys.push(p[i + 1])
                owner.push(c)
            }
        }
        var cell = art.spacing
        var grid = {}
        for (var n = 0; n < xs.length; n++) {
            var key = Math.floor(ys[n] / cell) * 65536 + Math.floor(xs[n] / cell)
            if (!grid[key]) {
                grid[key] = []
            }
            grid[key].push(n)
        }
        // Below the walk's own 0.95, so this has teeth: it fails on a
        // regression that lets two curves run closer than a line of text
        // needs, rather than only on one that draws the same line twice.
        var tooClose = cell * 0.8
        var found = 0
        for (var a = 0; a < xs.length; a++) {
            var gx = Math.floor(xs[a] / cell), gy = Math.floor(ys[a] / cell)
            for (var ox = -1; ox <= 1; ox++) {
                for (var oy = -1; oy <= 1; oy++) {
                    var bucket = grid[(gy + oy) * 65536 + (gx + ox)]
                    if (!bucket) {
                        continue
                    }
                    for (var bi = 0; bi < bucket.length; bi++) {
                        var b = bucket[bi]
                        if (b <= a || owner[a] === owner[b]) {
                            continue
                        }
                        var dx = xs[a] - xs[b], dy = ys[a] - ys[b]
                        if (Math.sqrt(dx * dx + dy * dy) < tooClose) {
                            found++
                        }
                    }
                }
            }
        }
        return found
    }

    /// Every curve on the canvas. Pure, and what the cover's test reads: the
    /// painting below only sets text along what this returns.
    function layout() {
        var L = art.lobes()
        var O = art.obstacleField(L)
        var rings = art.ringCount(L, O)
        var out = []
        for (var k = 0; k < rings; k++) {
            var curves = art.traceRing(L, O, k)
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
        var O = art._obstacle
        if (O && art.haloReach > 0) {
            // The nearest texel of the digits' distance is enough for the
            // strength of one run of glyphs; smoothstep, so the ring that
            // touches the digits is plainly the brightest and the fall-off
            // has no edge of its own.
            var ix = Math.round(x), iy = Math.round(y)
            if (ix < 0) { ix = 0 } else if (ix >= O.w) { ix = O.w - 1 }
            if (iy < 0) { iy = 0 } else if (iy >= O.h) { iy = O.h - 1 }
            var t = O.dd[iy * O.w + ix] / (art.haloReach * art.spacing)
            if (t < 0) { t = 0 } else if (t > 1) { t = 1 }
            var eased = t * t * (3 - 2 * t)
            w = 1 - (1 - art.farInk) * eased
        }
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
        var O = art._obstacle
        if (O && art.haloReach > 0) {
            var ix = Math.round(x), iy = Math.round(y)
            if (ix < 0) { ix = 0 } else if (ix >= O.w) { ix = O.w - 1 }
            if (iy < 0) { iy = 0 } else if (iy >= O.h) { iy = O.h - 1 }
            if (O.dd[iy * O.w + ix] < art.haloReach * art.spacing) {
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
            // The digits are measured on the stencil, which comes up a
            // moment after this does; the timer asks again.
            if (art.obstacle.length > 0 && !stencil.available) {
                return
            }
            ctx.setTransform(1, 0, 0, 1, 0, 0)
            ctx.clearRect(0, 0, art.width, art.height)
            // Quoted: a family with spaces in it is otherwise read as
            // several, and the text falls back to the default sans with a
            // warning nobody sees.
            ctx.font = art.glyph + "px \"" + art.fillerFont + "\""
            ctx.textBaseline = "middle"
            art._lobes = art.lobes()
            art._obstacle = art.obstacleField(art._lobes)
            art._rings = art.ringCount(art._lobes, art._obstacle)
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
        var O = art._obstacle
        var spacing = art.spacing
        do {
            var curves = art.traceRing(L, O, ring)
            var level = spacing * (ring + art.levelOffset)
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
