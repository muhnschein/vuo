import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The first thing a new install shows: the texture the cover wears, filling
 * the page, and in the middle the app's name, a line about it, and the one
 * thing to do -- go and set up the Miniflux instance.
 *
 * Shown by the root window while no account is stored. Continue hands over to
 * SetupDialog, which is where the flow ends: setup moves forward the whole
 * way, and this page steps aside for it rather than waiting underneath.
 *
 * It used to wait underneath, watching its own `status` to notice the user
 * coming back from Settings with an account saved, and then navigating
 * forward from inside that handler. Qt reported that as a binding loop on
 * `status`; the user saw the welcome screen flash past on the way to the
 * article list. Backward navigation cannot mean "done" without reading as a
 * glitch, so setup is accepted rather than swiped away from.
 *
 * No skip and no dismiss, since there is nothing to show without a server.
 */
Page {
    id: page

    /// The user is ready to set an account up. The root window takes it on.
    signal continued()

    allowedOrientations: Orientation.All

    // The cover's texture, at a page's density: the same pattern, painted
    // much finer, which is the whole of the difference between the two
    // masks. The middle is cleared for the words.
    TextArt {
        id: art
        objectName: "textArt"
        anchors.fill: parent
        source: "../art/onboarding.png"
        ink: 0.6
        clearX: page.width / 2
        clearY: page.height * 0.45
        clearRadius: Math.min(page.width, page.height) * 0.22
        clearFeather: Math.min(page.width, page.height) * 0.16
    }

    Column {
        anchors {
            horizontalCenter: parent.horizontalCenter
            verticalCenter: parent.verticalCenter
            verticalCenterOffset: -page.height * 0.05
        }
        width: parent.width - Theme.horizontalPageMargin * 2
        spacing: Theme.paddingMedium

        Label {
            objectName: "title"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            text: "Vuo"
            font.family: Theme.fontFamilyHeading
            font.pixelSize: Theme.fontSizeHuge
            color: Theme.highlightColor
        }

        Label {
            objectName: "tagline"
            width: parent.width
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            textFormat: Text.PlainText
            text: qsTr("Focus on what matters.")
            font.pixelSize: Theme.fontSizeLarge
            color: Theme.primaryColor
        }

        Item { width: 1; height: Theme.paddingLarge * 2 }

        Button {
            objectName: "continueButton"
            anchors.horizontalCenter: parent.horizontalCenter
            text: qsTr("Continue")
            onClicked: page.continued()
        }
    }
}
