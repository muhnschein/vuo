import QtQuick 2.6
import QtQuick.Window 2.2

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
 * pattern finer than the cover's, and it is the only difference between the
 * two.
 */
Window {
    id: win

    width: 400; height: 220
    visible: true
    color: "#101820"

    readonly property var masters: [
        // A phone screen, at the largest width Sailfish devices ship.
        { file: "onboarding.png", width: 1080, height: 2160, across: 115 },
        // A cover, which is about half that wide.
        { file: "cover.png", width: 640, height: 960, across: 64 }
    ]

    property int done: 0

    Text {
        anchors.centerIn: parent
        color: "white"
        text: win.done + " of " + win.masters.length + " painted"
    }

    Repeater {
        model: win.masters

        TextArtPainter {
            width: modelData.width
            height: modelData.height
            glyphsAcross: modelData.across
            ink: 1.0
            colour: "white"
            visible: false

            property bool written: false
            onCompleteChanged: {
                if (complete && !written) {
                    written = true
                    console.log("wrote", modelData.file, save(modelData.file))
                    win.done++
                    if (win.done === win.masters.length) {
                        Qt.quit()
                    }
                }
            }
        }
    }

    // So a painter that never finishes cannot hang a build.
    Timer {
        interval: 300000
        running: true
        onTriggered: {
            console.log("gave up with", win.done, "of", win.masters.length, "painted")
            Qt.quit()
        }
    }
}
