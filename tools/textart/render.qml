import QtQuick 2.6
import QtQuick.Window 2.2
import Sailfish.Silica 1.0
import "selection.js" as Selection

/*
 * Paints Vuo's texture at the sizes the app ships it in, and writes each one
 * beside itself as an RGBA PNG. `scripts/render-textart.sh` runs this and
 * then reduces the results to the masks in qml/art/.
 *
 * Masters are painted WHITE at full strength with no fade and no clearing:
 * the app tints them, dims them and cuts both. See TextArtPainter.qml.
 *
 * The sizes are chosen to be at least as large as the largest surface that
 * draws them, so a device only ever scales the pattern DOWN. Their density
 * -- how many glyph heights fit across the width -- is what makes the page's
 * pattern finer than the cover's.
 *
 * The cover is a SET of masters, one per unread count from 1 to 99 and one
 * for "99+": the count is negative space in the pattern (the lines flow
 * around it), so the whole background depends on the number and no two
 * counts can share a file. Nothing unread is "none", the sweeps with no
 * number in them at all. One painter does them in turn -- a hundred
 * canvases at once would want a hundred times the memory for no gain.
 *
 * `selection.js` names which sets to paint, and which counts of the cover's;
 * the script writes it from its arguments, so a change to the cover's
 * masters need not repaint the page's, and a change to the painter can be
 * looked at on one count before it is run over all of them.
 */
Window {
    id: win

    width: 400; height: 220
    visible: true
    color: "#101820"

    // Digit face. Fetched by script beside this file; see there for why Fira
    // Sans, not device's.
    FontLoader { id: digits; source: "FiraSans-ExtraBold.ttf" }

    // Filler face: device's Sail Sans Pro when script found copy beside this
    // file. Not redistributable: never fetched or committed. Without it filler
    // set in host's "Sail Sans Pro" (default sans); caveat in
    // docs/packaging.md.
    FontLoader { id: filler; source: "SailSansPro-Light.ttf" }

    // Cover 234x374 at pixel ratio 1 (aspect ~0.626), at most 1.75x that
    // (410x655) on Sailfish phones. 512x768 = smallest 2:3 master only scaling
    // DOWN on ratio-2 cover; 480x720 looked grainy. Wider than any cover:
    // always trimmed at sides, never top/bottom, so master height fraction =
    // same on every cover.
    readonly property int coverWidth: 512
    readonly property int coverHeight: 768

    /// Counts moved sideways by eye after painter centring, fraction of digit font size;
    /// negative = left. See TextArtPainter `obstacleNudge` for why list exists.
    ///
    /// From device reports, not measurement: after half-weight shift 1 and 4 still read
    /// right of centre, 3 less left than rule put it. Single counts only: correction is per
    /// shape; 13 isn't 1 and 3 side by side to the eye.
    readonly property var coverNudge: ({ "1": -0.039, "3": 0.0105, "4": -0.022 })

    /// One cover master. `key` = count, "99+", or "none".
    function coverJob(key) {
        if (key === "none") {
            // Nothing unread, nothing said: no digits, no negative space, no
            // outline; sweeps alone at cover density, glyphs facing same way.
            // Empty cover doesn't read "0".
            //
            // No digits = no halo for far-line strength; goes in as painter ink
            // instead: cover draws every mask at 1.5, 0.55 / 1.5 lands at usual
            // 0.55 like other counts' far lines.
            return {
                file: "cover/none.png",
                width: win.coverWidth, height: win.coverHeight, across: 64,
                levelOffset: 0.6,
                topsFaceSource: true,
                ink: 0.55 / 1.5
            }
        }
        return {
            file: "cover/" + key + ".png",
            // Digit outline written beside pattern as own mask; cover draws it
            // in ambience colour. See TextArtPainter `obstacleEdge`.
            edgeFile: "cover/" + key + "-edge.png",
            // Thin, measured on master not device: cover max 410 wide vs 512,
            // so 3.5 px here lands just under 3 there.
            obstacleEdge: 3.5,
            width: win.coverWidth, height: win.coverHeight, across: 64,
            obstacle: key,
            // Share of cover digits may take. Below painter defaults: number
            // needs air on cover, else pressed against edges.
            obstacleMaxWidth: 0.72,
            obstacleMaxHeight: 0.56,
            obstacleNudge: win.coverNudge[key] || 0,
            // Narrowest cover crop to survive. Bit under platform's 0.626, for
            // digits.
            fitAspect: 0.60,
            // Cover-action strip at bottom: Theme.itemSizeSmall (80 at ratio 1)
            // of 374-tall cover. Both scale with ratio; fraction holds. Digits
            // centred above.
            reservedBottom: Math.round(win.coverHeight * 80 / 374),
            levelOffset: 0.6,
            topsFaceSource: true,
            // Halo: full on digits, easing to 0.367 six lines out. Cover draws
            // mask at ink 1.5: far lines land at usual 0.55, digit lines driven
            // PAST full; shader saturates, thin glyphs bolder and brighter than
            // mask alone.
            farInk: 0.55 / 1.5,
            haloReach: 6
        }
    }

    readonly property var jobs: {
        var out = []
        if (Selection.sets.indexOf("onboarding") >= 0) {
            // Phone screen at largest Sailfish device width.
            out.push({ file: "onboarding.png", width: 1080, height: 2160, across: 115 })
        }
        if (Selection.sets.indexOf("cover") >= 0) {
            // Every count unless script asked for some (dozen renders of one
            // count while tuning shouldn't cost a hundred).
            var keys = Selection.keys
            if (keys.length === 0) {
                keys.push("none")
                for (var n = 1; n <= 99; n++) {
                    keys.push("" + n)
                }
                keys.push("99+")
            }
            for (var k = 0; k < keys.length; k++) {
                out.push(win.coverJob(keys[k]))
            }
        }
        return out
    }

    property int done: 0
    property var job: null

    Text {
        anchors.centerIn: parent
        color: "white"
        text: win.done + " of " + win.jobs.length + " painted"
    }

    TextArtPainter {
        id: painter
        // Zero size until job set: painter's first paint at pre-job size does
        // no work.
        width: win.job ? win.job.width : 0
        height: win.job ? win.job.height : 0
        glyphsAcross: win.job ? win.job.across : 64
        obstacle: win.job && win.job.obstacle ? win.job.obstacle : ""
        obstacleFont: digits.name
        obstacleMaxWidth: win.job && win.job.obstacleMaxWidth ? win.job.obstacleMaxWidth : 0.80
        obstacleMaxHeight: win.job && win.job.obstacleMaxHeight ? win.job.obstacleMaxHeight : 0.62
        obstacleEdge: win.job && win.job.obstacleEdge ? win.job.obstacleEdge : 0
        obstacleNudge: win.job && win.job.obstacleNudge ? win.job.obstacleNudge : 0
        fillerFont: filler.status === FontLoader.Ready ? filler.name : Theme.fontFamily
        fitAspect: win.job && win.job.fitAspect ? win.job.fitAspect : 0
        reservedBottom: win.job && win.job.reservedBottom ? win.job.reservedBottom : 0
        levelOffset: win.job && win.job.levelOffset ? win.job.levelOffset : 0.5
        topsFaceSource: win.job && win.job.topsFaceSource ? true : false
        farInk: win.job && win.job.farInk ? win.job.farInk : 1.0
        haloReach: win.job && win.job.haloReach ? win.job.haloReach : 0
        ink: win.job && win.job.ink ? win.job.ink : 1.0
        colour: "white"
        visible: false

        onCompleteChanged: {
            if (!complete || !win.job || width === 0) {
                return
            }
            var job = win.job
            console.log("wrote", job.file, save(job.file))
            if (job.edgeFile) {
                console.log("wrote", job.edgeFile, saveEdge(job.edgeFile))
            }
            // Traced again only to check: ring drawn twice = only defect anyone
            // notices. Script reads this line, refuses master if non-zero.
            console.log("OVERLAPS", job.file, overlaps())
            if (_obstacle) {
                // Digit ink position, for checking placement.
                console.log("OBSTACLE", job.file, _obstacle.left, _obstacle.top,
                            _obstacle.right, _obstacle.bottom, "px", _obstacle.px)
            }
            win.done++
            win.next()
        }
    }

    function next() {
        if (win.done >= win.jobs.length) {
            Qt.quit()
            return
        }
        patience.restart()
        win.job = win.jobs[win.done]
    }

    // Font must exist before first digit measured. Local-file FontLoader ready
    // within first event loop turn; wait for it, don't assume.
    Timer {
        interval: 50
        repeat: true
        running: digits.status !== FontLoader.Loading && filler.status !== FontLoader.Loading
        onTriggered: {
            stop()
            if (digits.status === FontLoader.Error) {
                console.log("the digits' font did not load; see render-textart.sh")
                Qt.quit()
                return
            }
            console.log("FILLER FONT", filler.status === FontLoader.Ready
                        ? filler.name : "not found; using the host's " + Theme.fontFamily)
            win.next()
        }
    }

    // Never-finishing painter can't hang build. Per master, not per run: 101
    // masters take over five minutes.
    Timer {
        id: patience
        interval: 300000
        onTriggered: {
            console.log("gave up with", win.done, "of", win.jobs.length, "painted")
            Qt.quit()
        }
    }
}
