import QtQuick 2.6
import Sailfish.Silica 1.0
import "../components"

/*
 * The first thing a new install shows: the texture the cover wears, filling
 * the page, and in the middle the app's name, a line about it, and the one
 * thing to do -- go and set up the Miniflux instance.
 *
 * Shown by the root window while no account is stored, and replaced with the
 * entry list the moment one is: coming back from Settings with a server and
 * a key saved is what finishes it (see `finished`). No skip and no dismiss,
 * since there is nothing to show without a server.
 */
Page {
    id: page

    /// The root window's Settings object, asked whether an account exists.
    property var account: null

    /// Raised once an account is stored: the root window takes it from here.
    signal finished()

    allowedOrientations: Orientation.All

    // On the way back from Settings, most likely -- and if a server and a key
    // were saved there, this page has done its job.
    onStatusChanged: if (status === PageStatus.Active && page.account && page.account.configured) {
        page.finished()
    }

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
            onClicked: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"))
        }
    }
}
