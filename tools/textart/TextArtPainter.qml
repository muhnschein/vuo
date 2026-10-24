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

    /// Filler. Any text works; this one everybody recognises as saying nothing.
    readonly property string filler:
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do "
        + "eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut "
        + "enim ad minim veniam, quis nostrud exercitation ullamco laboris "
        + "nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in "
        + "reprehenderit in voluptate velit esse cillum dolore eu fugiat "
        + "nulla pariatur. Excepteur sint occaecat cupidatat non proident, "
        + "sunt in culpa qui officia deserunt mollit anim id est laborum. "

    /// Glyph heights across width. THE dial for texture fineness; only
    /// difference between onboarding and cover patterns: same strokes, same
    /// rules, two densities.
    property real glyphsAcross: 64
    property real glyph: Math.max(3, Math.round(art.width / art.glyphsAcross))
    /// Line to line, centre to centre.
    property real spacing: art.glyph * 1.6
    /// How softly strokes' curve families merge (see `fieldAt`).
    property real softness: art.spacing * 2.0
    /// Text darkness on ground. Greyscale via theme colour at partial strength.
    property real ink: 0.55
    property color colour: Theme.primaryColor
    /// Filler face. Device's own by default (non-phone host: whatever
    /// fontconfig has); `FontLoader` with real file names it exactly (see
    /// render.qml).
    property string fillerFont: Theme.fontFamily

    /// Strokes curves grow from, in fractions of width/height. Strokes not
    /// points: innermost curves capsules, every family has direction = sweeps'
    /// lean.
    property var strokes: [
        { x: 0.12, y: 0.16, x2: 0.28, y2: 0.08 },
        { x: 0.95, y: 0.30, x2: 0.84, y2: 0.20 },
        { x: 0.06, y: 0.78, x2: 0.16, y2: 0.66 },
        { x: 0.70, y: 1.02, x2: 0.58, y2: 0.90 },
        { x: 1.00, y: 0.72, x2: 0.92, y2: 0.62 }
    ]

    // ------------------------------------------------- the negative space

    /// Text lines flow AROUND, not over: cover unread count. Empty = none.
    /// Digits never drawn (absence of lines); mask right for exactly that
    /// count.
    property string obstacle: ""
    /// Digit family as `FontLoader` names it. Count wants heavy face: Fira Sans
    /// ExtraBold. Not Black: 4's counter closes, digit reads solid.
    property string obstacleFont: ""
    /// Ink to ink between adjacent digits, in line spacings. At font's own
    /// spacing under one line fits between 4 and 2; they merge.
    property real obstacleGap: 2.6
    /// Max share of fitted width and usable height digit ink may take. Width
    /// first: 99+ wide, width binds.
    property real obstacleMaxWidth: 0.80
    property real obstacleMaxHeight: 0.62
    /// Fraction of way from ink box centre to ink WEIGHT number moves. 0
    /// centres box, 1 centres weight.
    ///
    /// For the 1: ink box runs flag tip to stem edge, nearly all ink in stem;
    /// box centred leaves stem (what eye reads AS digit) right of centre. Fira
    /// Sans ExtraBold at 300px, weight right of box centre per digit:
    ///
    ///     0 -0.5   1 +21.5   2 +5.3   3 +11.3   4 -1.6
    ///     5 +1.9   6  -4.3   7 +2.2   8  -0.6   9 +1.4
    ///
    /// 1 = twice next worst, ~fifth of its ink width; ~34 px on 512-wide
    /// master, plainly visible on device. Symmetric digits move under a pixel.
    ///
    /// HALF, not all. Full move puts stem left of centre, reads shifted other
    /// way: eye weighs extent as well as mass, flag wide though thin. Half
    /// agrees twice: looks right side by side at 0, 0.5, 1; and type designer's
    /// own (Fira 1: left bearing 2, right 22 → advance box +10.0 off ink box
    /// centre vs weight's +21.5).
    property real obstacleOptical: 0.5
    /// Extra sideways shift by eye, fraction of digit font size: negative =
    /// left. 0 = none.
    ///
    /// Weight above = measurement, enough for most counts. No single rule
    /// matches eye on all: weight says 3 goes LEFT (spine on right), but open
    /// left side already looks left of centre (seen on device). Rule stays;
    /// wrong counts corrected one by one by painting job (see render.qml). Size
    /// fraction, not pixels: holds if master or fit changes.
    property real obstacleNudge: 0
    /// Width of line traced around digit silhouette, master pixels, 0 = none.
    /// NOT painted into pattern: written beside by `saveEdge` as own mask; app
    /// lays it over texture in own colour.
    ///
    /// Beside, not baked: pattern = theme text colour at fraction ink; line =
    /// ambience highlight full. One channel can't say both; baked line tinted
    /// with pattern.
    ///
    /// Straddles silhouette (half over digit blank, half over gap first ring
    /// leaves) = how outline looks. Room: `levelOffset` keeps first ring 0.6
    /// spacings off edge.
    property real obstacleEdge: 0
    /// Narrowest aspect (w/h) master cropped to on device, 0 = master's own.
    /// App shader keeps full height, trims sides to cover shape; digits fitted
    /// to width SURVIVING trim, else wide count clipped on narrow cover.
    property real fitAspect: 0
    /// Bottom height used by something else (cover action area). Digits centred
    /// in rest above.
    property real reservedBottom: 0
    /// First ring position, spacings from source. 0.5 = painter default; cover
    /// masters 0.6 (first ring slightly off digit edge).
    property real levelOffset: 0.5
    /// Glyph tops face: nearest source, or away. Painter sweeps face away;
    /// cover masters face in so lines under number read upright.
    property bool topsFaceSource: false
    /// Halo: lines full strength touching digits, easing to `farInk` over
    /// `haloReach` spacings; number brightest, sweeps recede. Off while
    /// `haloReach` 0 or no obstacle. Baked into mask (app shader doesn't know
    /// digit positions); app draws mask at full ink, far lines come out at
    /// `farInk`.
    property real farInk: 1.0
    property real haloReach: 0

    // --------------------------------------------- room for what sits on top

    /// Top band text fades in under: nothing above `fadeFrom`, full from
    /// `fadeTo` down. Off while `fadeTo <= fadeFrom`.
    property real fadeFrom: 0
    property real fadeTo: 0

    /// Disc text fades out of, for something drawn in middle. Off while
    /// `clearRadius` 0.
    property real clearX: 0
    property real clearY: 0
    property real clearRadius: 0
    property real clearFeather: 0

    // ------------------------------------------------- how it gets on screen

    /// Max time per slice. Two frames: per-slice overhead doesn't dominate,
    /// touch never noticeably late.
    property int budgetMs: 24
    /// True once last ring drawn.
    property bool complete: false

    /// Everything changing drawing, in one string: single handler restarts
    /// pattern, no property forgotten.
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

    /// Restart pattern. Canvas cleared by next slice, not here: screen keeps
    /// old until replacement exists.
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
    /// No obstacle: coarse field grid `obstacleField` would have sampled; see
    /// `sampleField`. Null with obstacle.
    property var _plain: null
    property var _widths: ({})
    property var _inks: []

    Component.onCompleted: art.restart()

    // GUI thread returns between slices; this asks for next.
    Timer {
        interval: 16
        repeat: true
        running: art.available && !art.complete
        onTriggered: art.requestPaint()
    }

    /// Where digits rasterised one at a time for measuring. Never shown;
    /// `obstacleField` reads pixels back. Painter-sized: no digit fit allows
    /// falls off edge.
    Canvas {
        id: stencil
        width: art.width
        height: art.height
        visible: false
        renderTarget: Canvas.Image
        renderStrategy: Canvas.Immediate
    }

    // ----------------------------------------------------------- the field

    /// Strokes in pixels.
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

    /// One character drawn alone at `px`, cropped to ink. Returns ink position
    /// relative to baseline and coverage inside box; null for inkless glyph.
    ///
    /// Ink, not advance box: 4's advance has air both sides, 2's doesn't;
    /// centring advances puts number few pixels off where it appears.
    ///
    /// `cx` = ink WEIGHT across box, coverage-weighted (antialiased edge counts
    /// for what it covers). Most digits within pixel or two of middle; 1 not.
    /// `obstacleField` uses it to centre visually, not by measure.
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
        // Read back only region glyph can reach, not whole canvas; ascent and
        // side bearings bounded by size.
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
        var mass = 0, moment = 0
        for (var yy = 0; yy < bh; yy++) {
            for (var xx = 0; xx < bw; xx++) {
                var a = data[((yy + minY) * rw + xx + minX) * 4 + 3]
                bits[yy * bw + xx] = a >= 128 ? 1 : 0
                mass += a
                moment += a * xx
            }
        }
        return { width: bw, height: bh,
                 top: ry + minY - y0, bottom: ry + maxY - y0,
                 cx: mass > 0 ? moment / mass : bw / 2,
                 bits: bits }
    }

    /// One dimension of squared distance transform (Felzenszwalb &
    /// Huttenlocher): lower envelope of each sample's parabola, one pass each
    /// way. `f` in, `d` out, `v` and `z` scratch.
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

    /// Exact Euclidean distance from every pixel to nearest set pixel of
    /// `inside`, linear time. Zero on set pixels.
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

    /// Separable Gaussian blur, `sigma` px, edges clamped. Without it 4's
    /// inside corners crease every passing ring.
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

    /// Line along edge of `inside`, `width` px wide, centred on edge, coverage
    /// 0..255.
    ///
    /// `outside` = distance to shape from every pixel beyond (`obstacleField`
    /// paid for it), one-sided: band from it alone sits BESIDE silhouette,
    /// reads as extra ring. Mirror (distance to outside from every pixel
    /// within) costs one transform, completes pair: every pixel knows edge
    /// distance either side; centred line needs that.
    ///
    /// Pixel of feather: drawn, not stepped. Distances centre to centre, so
    /// edge ~half pixel inside shape; feather covers it at these widths.
    function outlineOf(inside, outside, w, h, width) {
        var n = w * h, i
        var flipped = new Uint8Array(n)
        for (i = 0; i < n; i++) {
            flipped[i] = inside[i] ? 0 : 1
        }
        var within = art.distanceOutside(flipped, w, h)
        var half = width / 2
        var out = new Uint8Array(n)
        for (i = 0; i < n; i++) {
            var fromEdge = inside[i] ? within[i] : outside[i]
            var cover = 1 - (fromEdge - (half - 0.5))
            out[i] = cover <= 0 ? 0
                   : (cover >= 1 ? 255 : Math.round(255 * cover))
        }
        return out
    }

    /// Write outline `obstacleEdge` asked for beside pattern: same size, same
    /// crop; app overlays without alignment.
    ///
    /// Painted into stencil (done measuring, right size). White at line
    /// coverage, nothing else: `scripts/png-mask.py` keeps alpha, drops rest,
    /// like pattern.
    ///
    /// Drawn as RUNS OF FILLRECT, not one putImageData: putImageData broken in
    /// this Qt (bytes reach ImageData, read back fine, never reach canvas; file
    /// empty, `save` still true). png-mask.py quantises to sixteen alpha levels
    /// anyway: that many passes lose nothing; one pass over digit box finds
    /// every run.
    function saveEdge(file) {
        var O = art._obstacle
        if (!O || !O.edge) {
            return false
        }
        var ctx = stencil.getContext("2d")
        if (!ctx) {
            return false
        }
        var w = stencil.width, h = stencil.height
        ctx.setTransform(1, 0, 0, 1, 0, 0)
        ctx.clearRect(0, 0, w, h)

        // Only where line can be: digit ink grown by line width + feather
        // pixel. Whole canvas = 400k wasted steps per master.
        var pad = Math.ceil(art.obstacleEdge / 2) + 2
        var x0 = Math.max(0, Math.floor(O.left) - pad)
        var x1 = Math.min(w - 1, Math.ceil(O.right) + pad)
        var y0 = Math.max(0, Math.floor(O.top) - pad)
        var y1 = Math.min(h - 1, Math.ceil(O.bottom) + pad)

        var levels = 16
        var byLevel = []
        var i
        for (i = 0; i < levels; i++) {
            byLevel.push([])
        }
        var e = O.edge
        var x, y
        for (y = y0; y <= y1; y++) {
            var row = y * w
            var cur = 0, start = x0
            for (x = x0; x <= x1 + 1; x++) {
                // One past end closes last run without repeating emit in loop.
                var q = x <= x1
                        ? Math.round(e[row + x] * (levels - 1) / 255) : 0
                if (q !== cur) {
                    if (cur > 0) {
                        byLevel[cur].push(start, y, x - start)
                    }
                    cur = q
                    start = x
                }
            }
        }
        for (var L = 1; L < levels; L++) {
            var runs = byLevel[L]
            if (runs.length === 0) {
                continue
            }
            ctx.fillStyle = Qt.rgba(1, 1, 1, L / (levels - 1))
            for (i = 0; i < runs.length; i += 3) {
                ctx.fillRect(runs[i], runs[i + 1], runs[i + 2], 1)
            }
        }
        return stencil.save(file)
    }

    /// Digits as field source: distance to them from every outside pixel, plus
    /// ink position. Null without obstacle. Built once per `_key`, kept
    /// (tracing asks thousands of times per ring).
    ///
    /// Digits set one at a time, each cropped to ink, all scaled by ONE factor,
    /// joined with `obstacleGap` spacings ink to ink. Block ink bounds centred
    /// across width surviving narrowest crop, vertically in height above
    /// `reservedBottom`.
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

        // Measured at trial size, set at one size that fits.
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

        // Where number looks centred, not where it measures.
        //
        // Each glyph contributes OWN offset, weighted equally, not by ink.
        // Within glyph eye follows weight (why 1 moves); between glyphs not:
        // heavier digit mustn't drag pair (block centroid sends 18 left, 8
        // outweighs 1).
        var optical = 0, inked = 0
        for (i = 0; i < n; i++) {
            if (glyphs[i]) {
                optical += glyphs[i].cx - glyphs[i].width / 2
                inked++
            }
        }
        optical = inked > 0 ? art.obstacleOptical * optical / inked : 0
        // `optical` = how far LEFT; rightward nudge subtracts.
        optical -= art.obstacleNudge * px

        // Shift can't walk number out of crop. Room exists (fit leaves (1 -
        // obstacleMaxWidth) of width spare, ~64px vs 1's 34), but later change
        // to either must fail loudly here, not clip digit on device.
        var slack = (fitW - total) / 2
        if (Math.abs(optical) > slack) {
            console.log("OPTICAL CLAMPED", art.obstacle, optical.toFixed(1),
                        "to", slack.toFixed(1))
            optical = optical > 0 ? slack : -slack
        }

        var left = Math.round((w - total) / 2 - optical)
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
        var edge = art.obstacleEdge > 0
                   ? art.outlineOf(inside, dd, w, h, art.obstacleEdge) : null
        // Two pixels at width values were settled at (936).
        dd = art.blurred(dd, w, h, 2 * w / 936)

        var O = {
            w: w, h: h, dd: dd, edge: edge,
            k: 1.5 * spacing,
            left: left, top: baseline + top,
            right: left + total - 1, bottom: baseline + bottom,
            px: px,
            samples: null, sampleCell: 0, sampleCols: 0, sampleRows: 0
        }

        art.sampleField(L, O, O)

        art._obstacle = O
        art._obstacleKey = art._key
        return O
    }

    /// Field on coarse grid over canvas, half spacing apart, written onto
    /// `into` as `samples`, `sampleCell`, `sampleCols`, `sampleRows`, and
    /// returned. Lets ring be seeded wherever it runs (around digits, inside
    /// counter, closing on point farthest from every stroke), not only around
    /// source strokes; also says field reach.
    ///
    /// With obstacle hangs off `obstacleField`; without, off `_plain`. Strokes
    /// alone need it too: cover's middle farther from every stroke than any
    /// corner, no ray from stroke meets innermost rings, numberless cover gets
    /// bare middle patch.
    function sampleField(L, O, into) {
        var cell = art.spacing * 0.5
        var cols = Math.floor(art.width / cell) + 1
        var rows = Math.floor(art.height / cell) + 1
        var samples = new Float32Array(cols * rows)
        var sc = new Array(3 * L.length)
        var fg = [0, 0, 0]
        for (var r = 0; r < rows; r++) {
            for (var c = 0; c < cols; c++) {
                art.fieldAt(L, O, c * cell, r * cell, art.softness, sc, fg)
                samples[r * cols + c] = fg[0]
            }
        }
        into.samples = samples
        into.sampleCell = cell
        into.sampleCols = cols
        into.sampleRows = rows
        return into
    }

    /// Field and gradient at one point, into `out` as `[value, dx, dy]`.
    ///
    /// Field = soft "distance to nearest stroke": exact far from all, smooth
    /// blend where two close; level sets = curves, one per `spacing`, evenly
    /// spaced (field is distance).
    ///
    /// Gradient EXACT, same pass (whole tracing speed-up): weighted average of
    /// unit vectors away from each stroke, same weights as value. Finite
    /// differences cost four more evaluations, no more accurate.
    ///
    /// Exponentials relative to nearest stroke: no underflow at any distance;
    /// `sc` = caller-owned scratch, walk allocates nothing.
    ///
    /// With obstacle `O`: strokes' value joined to digit distance by polynomial
    /// smooth minimum of width `O.k`: exact where one is over `k` nearer,
    /// rounded only at seam. Digit distance sampled bilinearly, gradient from
    /// same four texels. Join gradient = `mix` of both by same weight, exactly:
    /// `k h (1 - h)` term's derivative cancels weight's own, so Newton
    /// correction converges at seam like anywhere.
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

    /// Ring count field's reach needs.
    ///
    /// Farthest point from every stroke can be anywhere (between strokes and
    /// digits, middle of cover with neither); coarse grid (`sampleField`) asked
    /// as well as corners.
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
        var G = O ? O : art._plain
        if (G) {
            for (var i = 0; i < G.samples.length; i++) {
                if (G.samples[i] > far) { far = G.samples[i] }
            }
        }
        return Math.ceil((far + art.spacing) / art.spacing)
    }

    // ----------------------------------------------------------- the tracing

    /// Follow curve at `level` from seed, one way, until it closes, leaves
    /// canvas, or reaches ground earlier curve of this ring covered. Returns
    /// flat `[x0, y0, x1, y1, ...]`.
    ///
    /// One stride along tangent, one Newton correction back to level: two
    /// evaluations per step; correction stops drift off ring over thousands of
    /// pixels.
    ///
    /// `covered` = where ring's earlier curves went (see `traceRing`); stopping
    /// there keeps ring from being TRACED TWICE (second seed just past first
    /// curve's end would retrace hundreds of px two px aside). Checked every
    /// stride, not just at seed: later curve fills rest of ring, stops where
    /// pattern exists.
    ///
    /// Same question `overlaps` asks after ("ink within `near` of here?"):
    /// can't disagree on drawing twice; curves meet, not overlap by grid cell
    /// size.
    ///
    /// `own` = same question of THIS curve: ring inside 4's counter follows
    /// both triangle sides into apex, where curve's two sides come closer than
    /// text line (`overlaps` doesn't count it; curve may near itself around
    /// tight cap) → text over text in every 4. Points enter `own` `skip`
    /// strides late, first `skip` never: curve not stopped by stride just taken
    /// nor kept from closing on own start. Backward walk from seed starts with
    /// forward walk's points in `own`.
    function walk(L, O, sx, sy, level, direction, step, maxSteps, soft, sc, fg,
                  covered, own, cell, near) {
        var points = []
        var x = sx, y = sy
        var w = art.width, h = art.height
        var margin = art.spacing
        var closeEnough = step * step
        var skip = Math.ceil(near / step) + 2
        // Max off-level distance kept: twentieth of spacing, so rings a spacing
        // apart never closer than nine tenths. Seeds held to pixel; same idea
        // for point with stride behind it.
        var tolerance = art.spacing * 0.05
        var reach, correction, g2
        art.fieldAt(L, O, x, y, soft, sc, fg)
        for (var i = 0; i < maxSteps; i++) {
            // `fg` = field at (x, y), on level.
            var gx = fg[1], gy = fg[2]
            var gn = Math.sqrt(gx * gx + gy * gy)
            if (gn < 1e-9) {
                break
            }
            var px = x, py = y
            x += direction * step * (-gy / gn)
            y += direction * step * (gx / gn)
            // Back to level: one Newton correction, check landing, up to three
            // more if off. Nearly always on; check = one extra evaluation per
            // stride. Off-level point (tight bend in 2's pocket, seam where
            // digit rings meet stroke rings) lands in NEXT ring's room = text
            // over text. Three more fail → walk lost ring, ends here, not
            // drawing off it.
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
                // Never further than stride. Strokes' field ~distance (unit
                // gradient), never bound correction; digit field has RIDGES
                // (4's counter middle, 2's pocket) where blurred distance
                // flattens, gradient → 0, one correction throws walk across
                // pocket onto another ring.
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
            // Walk not progressing = at ridge where level set pinches to point:
            // curve ends.
            var mx = x - px, my = y - py
            if (mx * mx + my * my < step * step * 0.04) {
                return { points: points, closed: false }
            }
            // BEFORE point kept, not after: stride as long as curvature allows,
            // curve can go comfortable → touching in one. Keeping stopping
            // point leaves overlaps. No `i > 6` guard: `covered` holds only
            // curves traced BEFORE this one, nothing of own to trip on.
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

    /// Any recorded `grid` point within `near` of (x, y). Grid buckets by
    /// `cell`; `near` <= cell, so nine surrounding cells suffice.
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

    /// Record point as drawn, for `inked`.
    ///
    /// Not `ink`: that's darkness property; same-named function unreachable
    /// ("TypeError", number asked to be function).
    function markInk(grid, cell, x, y) {
        var key = Math.floor(y / cell) * 65536 + Math.floor(x / cell)
        if (!grid[key]) {
            grid[key] = []
        }
        grid[key].push(x, y)
    }

    /// Every curve of one ring, as `{points, closed}`, `points` flat.
    ///
    /// Seeded from several directions per stroke: ring = separate curves until
    /// stroke families merge; seed landing on traced curve dropped (merged ones
    /// = one curve). Grid makes check cheap.
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

        // Chord of this length within `tol` of circle radius `level`: innermost
        // (curving) rings walked in short strides.
        //
        // CAP matters on outer ones; half spacing, not three. Overlap checks
        // compare sampled POINTS; stride long enough to cross another curve
        // between own samples hides crossing (text over text at shallow angle,
        // samples all far apart). Crossing segments have endpoints within
        // stride of crossing; stride under half spacing can't hide one from
        // check refusing 0.8 of it. Painted on host now, can afford care.
        var tol = 0.4
        var step = Math.sqrt(8 * level * tol)
        if (step < 1.5) { step = 1.5 } else if (step > spacing * 0.5) { step = spacing * 0.5 }

        // How close two line bits may come before later gives up: just under
        // ring spacing (spacing IS room text line needs). Half spacing visibly
        // wrong: glyphs nearly as tall as spacing, lines within half collide
        // though no curve drawn twice. Seed check and walk both mean this by
        // "already drawn"; no point within kept, so `overlaps` passes by
        // construction, not luck.
        var near = spacing * 0.95
        // Big enough: nine cells around point hold all within `near`, stride
        // can't skip cell.
        var cell = Math.max(spacing, step)
        var grid = {}
        var sc = new Array(3 * n)
        var fg = [0, 0, 0]
        var maxSteps = Math.ceil((2 * Math.PI * level + 2 * (w + h)) / step) + 64

        // Where to look for ring's curves. Eight directions around each stroke;
        // plus every coarse-grid point within half spacing of level (finds
        // rings wrapping only digits, inside 4/9 counter, closing on middle of
        // digitless cover). Seeds on traced curve dropped; extras free where
        // stroke seeds sufficed.
        var seeds = []
        var si, i
        for (si = 0; si < n * 8; si++) {
            i = si % n
            var angle = -Math.PI / 2 + Math.floor(si / n) * Math.PI / 4 + i * 0.7
            var cx = (L[i].x + L[i].x2) / 2
            var cy = (L[i].y + L[i].y2) / 2
            seeds.push(cx + level * Math.cos(angle), cy + level * Math.sin(angle))
        }
        var G = O ? O : art._plain
        if (G) {
            var band = spacing * 0.5
            for (var r = 0; r < G.sampleRows; r++) {
                for (var c = 0; c < G.sampleCols; c++) {
                    if (Math.abs(G.samples[r * G.sampleCols + c] - level) < band) {
                        seeds.push(c * G.sampleCell, r * G.sampleCell)
                    }
                }
            }
        }

        var crumb = art.glyph * 6
        for (si = 0; si < seeds.length; si += 2) {
            var sx = seeds[si], sy = seeds[si + 1]

            // Pull seed onto ring.
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
                // Way back mustn't run into way out, except first few strides
                // beside seed (own neighbours, not another arm).
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
            // Crumbs: closed loop at stroke core or ring sliver between earlier
            // curves carries one stray word. Six glyph heights = shortest run
            // reading as text line, not litter.
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

    /// Whether glyphs set along `points` (as `paintCurve` sets) face tops
    /// toward HIGHER field (away from nearest source). Sampled up to sixteen
    /// places, majority wins (curve may cross family seam, briefly disagree).
    ///
    /// `paintCurve` stands glyph left of travel direction: along (dx, dy) tops
    /// point (dy, -dx); vs gradient (gx, gy) = sign of dx * gy - dy * gx.
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

    /// Where two DIFFERENT curves overlap: only defect people notice (text on
    /// text).
    ///
    /// Returns pairs of points closer than half line spacing. `make textart`
    /// refuses master reporting any (ring traced twice by two seeds).
    /// SAME-curve pairs not counted: curve legitimately nears itself around
    /// tight end cap (capsule barely wider than stride).
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
        // Below walk's 0.95, so has teeth: fails when curves run closer than
        // text line needs, not only on same line drawn twice.
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

    /// Every curve on canvas. Pure; cover test reads it; painting only sets
    /// text along it.
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

    /// Draw strength at point: 1 in open, fading to 0 in top band and inside
    /// disc.
    function weightAt(x, y) {
        var w = 1
        var O = art._obstacle
        if (O && art.haloReach > 0) {
            // Nearest digit-distance texel enough for one glyph run's strength;
            // smoothstep: ring touching digits plainly brightest, fall-off has
            // no edge.
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

    /// Max glyph run length at point before it stops following curve, or fade
    /// under it, closely enough to pass for per-glyph drawing.
    ///
    /// Straightness: arc length l on radius `level` turns l / level; run kept
    /// within 1/16 radian. Fade: where ink CHANGES, run's single alpha bands;
    /// runs shrink to glyph or two there only.
    ///
    /// Six glyphs max: host measurement, longer runs bought nothing (cost =
    /// glyph rasterising, not calls). Shorter safer: hugs curve.
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

    /// Advance of every filler character, measured once. Per-glyph
    /// `measureText` was big share of paint cost.
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

    /// Set text along one curve in runs. Returns filler position to continue
    /// from: no two curves open with same word.
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

        // One cursor for both lookups: run start never past own middle, next
        // run starts past this middle; only moves forward.
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
                    // Set, not stacked: save + restore per glyph cost more than
                    // glyph.
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
            // Digits measured on stencil, available slightly later; timer asks
            // again.
            if (art.obstacle.length > 0 && !stencil.available) {
                return
            }
            ctx.setTransform(1, 0, 0, 1, 0, 0)
            ctx.clearRect(0, 0, art.width, art.height)
            // Quoted: family with spaces read as several otherwise; falls back
            // to default sans with unseen warning.
            ctx.font = art.glyph + "px \"" + art.fillerFont + "\""
            ctx.textBaseline = "middle"
            art._lobes = art.lobes()
            art._obstacle = art.obstacleField(art._lobes)
            art._plain = art._obstacle ? null : art.sampleField(art._lobes, null, {})
            art._rings = art.ringCount(art._lobes, art._obstacle)
            art._widths = art.advances(ctx)
            // Ink in thirty-two steps: run doesn't build colour from string
            // each draw.
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
