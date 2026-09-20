import QtQuick 2.6
import QtQuick.Window 2.2
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
 * The cover is a SET of masters, one per unread count from 0 to 99 and one
 * for "99+": the count is negative space in the pattern (the lines flow
 * around it), so the whole background depends on the number and no two
 * counts can share a file. One painter does them in turn -- a hundred
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

    // The digits' face. Fetched by the script beside this file; see there
    // for why it is Fira Sans and not the device's own.
    FontLoader { id: digits; source: "FiraSans-ExtraBold.ttf" }

    // The filler's face: the device's own, Sail Sans Pro, when the script
    // found a copy to put beside this file. It is not redistributable, so
    // it is never fetched and never committed; without it the filler is
    // set in whatever the host calls "Sail Sans Pro", in practice its
    // default sans -- the caveat in docs/packaging.md.
    FontLoader { id: filler; source: "SailSansPro-Light.ttf" }

    // A cover is 234x374 at pixel ratio 1 -- an aspect of about 0.626 -- and
    // at most 1.75 times that on the phones Sailfish ships on. 512x768 is
    // the smallest 2:3 master that still only scales DOWN on a cover at
    // ratio 2, and being wider than any cover it is always trimmed at the
    // sides, never the top or bottom, so a height on the master is the same
    // fraction of the height on every cover.
    readonly property int coverWidth: 512
    readonly property int coverHeight: 768

    /// One cover master. `key` is the count, or "99+".
    function coverJob(key) {
        return {
            file: "cover/" + key + ".png",
            width: win.coverWidth, height: win.coverHeight, across: 64,
            obstacle: key,
            // How much of the cover the digits may take. Less than the
            // painter's own defaults: the number wants air around it on
            // a cover, or it reads as pressed against the edges.
            obstacleMaxWidth: 0.72,
            obstacleMaxHeight: 0.56,
            // The narrowest cover this must survive being cropped to. A
            // little under the platform's own 0.626, for the digits' sake.
            fitAspect: 0.60,
            // The cover-action strip at the bottom: Theme.itemSizeSmall (80
            // at ratio 1) of a 374-tall cover. Both scale with the ratio,
            // so the fraction holds; the digits are centred above it.
            reservedBottom: Math.round(win.coverHeight * 80 / 374),
            levelOffset: 0.6,
            topsFaceSource: true
        }
    }

    readonly property var jobs: {
        var out = []
        if (Selection.sets.indexOf("onboarding") >= 0) {
            // A phone screen, at the largest width Sailfish devices ship.
            out.push({ file: "onboarding.png", width: 1080, height: 2160, across: 115 })
        }
        if (Selection.sets.indexOf("cover") >= 0) {
            // Every count, unless the script was asked for particular ones
            // (a dozen renders of one count while adjusting the painter
            // should not cost a hundred).
            var keys = Selection.keys
            if (keys.length === 0) {
                for (var n = 0; n <= 99; n++) {
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
        // Parked at nothing until a job is set, so the painter's own first
        // paint -- at whatever size it has before then -- does no work.
        width: win.job ? win.job.width : 0
        height: win.job ? win.job.height : 0
        glyphsAcross: win.job ? win.job.across : 64
        obstacle: win.job && win.job.obstacle ? win.job.obstacle : ""
        obstacleFont: digits.name
        obstacleMaxWidth: win.job && win.job.obstacleMaxWidth ? win.job.obstacleMaxWidth : 0.80
        obstacleMaxHeight: win.job && win.job.obstacleMaxHeight ? win.job.obstacleMaxHeight : 0.62
        fillerFont: filler.status === FontLoader.Ready ? filler.name : Theme.fontFamily
        fitAspect: win.job && win.job.fitAspect ? win.job.fitAspect : 0
        reservedBottom: win.job && win.job.reservedBottom ? win.job.reservedBottom : 0
        levelOffset: win.job && win.job.levelOffset ? win.job.levelOffset : 0.5
        topsFaceSource: win.job && win.job.topsFaceSource ? true : false
        ink: 1.0
        colour: "white"
        visible: false

        onCompleteChanged: {
            if (!complete || !win.job || width === 0) {
                return
            }
            var job = win.job
            console.log("wrote", job.file, save(job.file))
            // Traced a second time, and only to be told off: a ring drawn
            // twice is the one defect of this pattern anyone notices, and
            // it has shipped once. The script reads this line and refuses
            // the master if it is not zero.
            console.log("OVERLAPS", job.file, overlaps())
            if (_obstacle) {
                // Where the digits' ink is, for checking the placement.
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

    // The font has to be there before the first digit is measured. A
    // FontLoader reading a local file is ready within the first event loop
    // turn, and this waits for it rather than assuming so.
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

    // So a painter that never finishes cannot hang a build. Per master, not
    // per run: a hundred and one of them take longer than five minutes.
    Timer {
        id: patience
        interval: 300000
        onTriggered: {
            console.log("gave up with", win.done, "of", win.jobs.length, "painted")
            Qt.quit()
        }
    }
}
