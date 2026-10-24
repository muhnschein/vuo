import QtQuick 2.6
import QtQuick.Window 2.2

/*
 * The Harbour store's cover image, 1080x540.
 *
 * Not part of the app and not installed: this is the banner at the top of
 * Vuo's Store page. It is the onboarding screen, laid out for a landscape
 * frame -- the same painter, the same cleared disc, the same words -- so the
 * page and the app look like the same thing.
 *
 * Run it with scripts/render-store-cover.sh, which puts the fonts beside it
 * and writes store/cover.png. Committed, like the masks in qml/art/, because
 * the store wants a file rather than a recipe.
 *
 * The strokes are NOT the painter's defaults. Those are placed for a portrait
 * page; at 2:1 the two near the bottom edge fall outside the frame entirely
 * and the pattern loses half its structure. These are the same idea -- five
 * short strokes, spread, each with a lean -- read for this shape.
 */
Window {
    id: win

    width: 1080
    height: 540
    visible: true
    // Ground app's dark ambience sits at. App transparent over wallpaper; store
    // banner brings own.
    color: "#0e1417"

    // Sail Sans Pro not redistributable; wordmark = Fira Sans (humanist, light,
    // close enough that banner and screenshot look like same app).
    FontLoader { id: light; source: "FiraSans-Light.ttf" }
    FontLoader { id: book; source: "FiraSans-Regular.ttf" }

    Item {
        id: frame
        anchors.fill: parent

        // Ground INSIDE frame, not on Window. `grabToImage` captures item tree;
        // Window `color` not in it, so grab transparent and white text art
        // flattens to nothing.
        Rectangle {
            anchors.fill: parent
            color: win.color
        }

        TextArtPainter {
            anchors.fill: parent
            // Coarser than onboarding's 115: viewed small in browser beside
            // text.
            glyphsAcross: 96
            ink: 0.52
            colour: "#ffffff"
            strokes: [
                { x: 0.08, y: 0.30, x2: 0.20, y2: 0.10 },
                { x: 0.37, y: 1.06, x2: 0.26, y2: 0.86 },
                { x: 0.63, y: -0.06, x2: 0.74, y2: 0.14 },
                { x: 0.94, y: 0.86, x2: 0.82, y2: 0.66 },
                { x: 1.04, y: 0.24, x2: 0.96, y2: 0.40 }
            ]
            // Room for words, like onboarding page.
            clearX: win.width / 2
            clearY: win.height * 0.47
            clearRadius: win.height * 0.44
            clearFeather: win.height * 0.30

            onCompleteChanged: if (complete) win.write()
        }

        Column {
            anchors.centerIn: parent
            anchors.verticalCenterOffset: -win.height * 0.02
            width: parent.width
            spacing: 14

            Text {
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                text: "Vuo"
                font.family: light.name
                font.pixelSize: 128
                // Theme.highlightColor, as onboarding page sets.
                color: "#80c0ff"
            }

            Text {
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                text: "Focus on what matters."
                font.family: book.name
                font.pixelSize: 34
                font.letterSpacing: 0.6
                color: "#eef3f4"
            }
        }
    }

    /// One grab of composed frame once pattern finished.
    function write() {
        frame.grabToImage(function (result) {
            console.log("wrote cover.png", result.saveToFile("cover.png"))
            Qt.quit()
        })
    }

    // Never-finishing painter can't hang script.
    Timer {
        interval: 300000
        running: true
        onTriggered: { console.log("gave up"); Qt.quit() }
    }
}
