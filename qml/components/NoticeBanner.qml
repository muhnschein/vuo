import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A transient status banner that does not move the page under it.
 *
 * Notices used to be a Column inside the entry list's `header`. A positioner
 * that content also lives in is the wrong place for something that appears and
 * disappears on its own schedule: every notice re-flowed the list, and one
 * that arrived while the user was reading pushed the rows they were looking at
 * down the screen. Being part of the header also meant it was only visible at
 * the very top of the list, so the timeout it was reporting was easy to miss
 * entirely.
 *
 * A `DockedPanel` fixes both halves. It slides in over the content from the
 * bottom edge -- so it is on screen wherever the user has scrolled to, and the
 * page's own layout never changes -- and Silica animates and dismisses it with
 * the same motion the rest of the system uses.
 *
 * Silica's `Notices` singleton (exported publicly as
 * "Sailfish.Silica/Notices 1.0") is the other candidate and is deliberately
 * not used: its item renders the text it is handed with no way to force
 * `textFormat`, and every string here can be the SERVER'S OWN WORDS. §9.3: a
 * crafted error message in a rich-text context is markup injection, and can
 * pull a remote image that leaks the device's IP. This file sets
 * `Text.PlainText` explicitly, which is the whole reason it exists.
 */
DockedPanel {
    id: banner

    /// FOREIGN TEXT. Always PlainText.
    property string message: ""
    /// Colours text, holds banner open: error user must act on mustn't vanish
    /// while read.
    property bool isError: false
    /// Non-empty = action button shown. TRANSLATED CONSTANT.
    property string actionText: ""
    /// Non-error banner lifetime. Errors ignore it unless no action; see
    /// `post`.
    property int autoHideMs: 6000

    signal actionTriggered()

    width: parent ? parent.width : 0
    height: content.height + Theme.paddingLarge

    // OPAQUE background, replaces DockedPanel default PanelBackground.
    //
    // Default = gradient fading to transparent: fine for panel user pulls open,
    // wrong for banner appearing by itself (device: "barely readable, content
    // shines through"). `Theme.overlayBackgroundColor` = Silica's public fully
    // opaque colour for this; follows ambience.
    background: Rectangle {
        color: Theme.overlayBackgroundColor

        // Hairline: banner reads as surface ON page, not hole in it.
        Rectangle {
            anchors { top: parent.top; left: parent.left; right: parent.right }
            height: Theme._lineWidth
            color: banner.isError ? Theme.errorColor
                                  : Theme.rgba(Theme.highlightColor, 0.4)
        }
    }

    /// Show `text`. `error` colours it; `action` (translated constant or empty)
    /// adds button.
    ///
    /// Function, not bindings on `message`/`open`: notice is EVENT. Bound
    /// visibility re-shows banner on every re-evaluation (same failure reported
    /// twice).
    function post(text, error, action) {
        banner.message = text
        banner.isError = error === true
        banner.actionText = action === undefined ? "" : action
        // Error with no action still auto-hides: undismissable, unactionable
        // banner = just smaller version of replaced layout.
        hideTimer.interval = banner.isError && banner.actionText.length > 0
                             ? 0
                             : banner.autoHideMs
        hideTimer.restart()
        banner.show()
    }

    function dismiss() {
        hideTimer.stop()
        banner.hide()
    }

    Timer {
        id: hideTimer
        // `interval: 0` = sticky; `running` gated on it so zero-interval timer
        // never fires at once.
        repeat: false
        running: false
        onTriggered: if (hideTimer.interval > 0) banner.hide()
    }

    Column {
        id: content

        x: Theme.horizontalPageMargin
        width: parent.width - Theme.horizontalPageMargin * 2
        y: Theme.paddingMedium
        spacing: Theme.paddingMedium

        Label {
            width: parent.width
            // Point of component. Never AutoText or StyledText: string can come
            // straight from server.
            textFormat: Text.PlainText
            text: banner.message
            wrapMode: Text.Wrap
            maximumLineCount: 4
            elide: Text.ElideRight
            font.pixelSize: Theme.fontSizeSmall
            color: banner.isError ? Theme.errorColor : Theme.primaryColor
        }

        Button {
            anchors.horizontalCenter: parent.horizontalCenter
            visible: banner.actionText.length > 0
            text: banner.actionText
            onClicked: {
                banner.dismiss()
                banner.actionTriggered()
            }
        }
    }

    // Tap dismisses. Below button in file so button wins tap.
    MouseArea {
        anchors.fill: parent
        z: -1
        onClicked: banner.dismiss()
    }
}
