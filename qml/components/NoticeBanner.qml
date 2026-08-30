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

    /// FOREIGN TEXT. Rendered as PlainText, always.
    property string message: ""
    /// Colours the text and holds the banner open: an error the user has to
    /// act on should not vanish while they are reading it.
    property bool isError: false
    /// When non-empty, an action button is shown. A TRANSLATED CONSTANT.
    property string actionText: ""
    /// How long a non-error banner stays. Errors ignore this unless they carry
    /// no action -- see `post`.
    property int autoHideMs: 6000

    signal actionTriggered()

    width: parent ? parent.width : 0
    height: content.height + Theme.paddingLarge

    /// Show `text`. `error` colours it; `action` (a translated constant, or
    /// empty) adds a button.
    ///
    /// A function rather than bindings on `message`/`open`, because a notice
    /// is an EVENT. Bound visibility re-shows the same banner every time the
    /// property that produced it is re-evaluated, which is how "fetch original
    /// content" managed to report the same failure twice.
    function post(text, error, action) {
        banner.message = text
        banner.isError = error === true
        banner.actionText = action === undefined ? "" : action
        // An error with nothing to do about it still goes away on its own: a
        // banner the user cannot dismiss and cannot act on is just a smaller
        // version of the layout it replaced.
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
        // `interval: 0` is the sticky case; `running` is gated on it so a
        // zero-interval timer never fires immediately.
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
            // The whole point of this component. Never AutoText, never
            // StyledText: this string can come straight from the server.
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

    // Tapping the banner dismisses it. Below the button in the file so the
    // button wins the tap.
    MouseArea {
        anchors.fill: parent
        z: -1
        onClicked: banner.dismiss()
    }
}
