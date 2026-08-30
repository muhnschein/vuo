import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One tab in ScopeTabBar. A public-API rebuild of the TabButton that ships in
 * Sailfish.Silica.private -- see the header of ScopeTabBar.qml for why this is
 * rebuilt rather than imported. Every metric below is the metric Silica's own
 * TabButton uses, cited to the line it came from.
 */
BackgroundItem {
    id: button

    /// A TRANSLATED CONSTANT, never foreign text. See the Label below.
    property string title
    property bool current: false
    property bool portrait: true
    property int titleFontSize: Theme.fontSizeLarge
    /// Half the strip's slack, folded into the first and last tab so a strip
    /// that fits ends up centred. TabButton.qml:63-65.
    property real extraMargin: 0
    property int tabIndex: 0
    property int tabCount: 1

    /// The underline tracks the TEXT, not the button: Silica reads the active
    /// TabButton's `contentItem.x`/`.width` (TabBar.qml:181, 204) and a
    /// TabButton's contentItem IS its text column (TabButton.qml:58). This
    /// cannot be called `contentItem`, because BackgroundItem already declares
    /// that name for its full-width press rectangle (BackgroundItem.qml:49) --
    /// anchoring to that would give the full-button-width bar, which is
    /// Silica's OTHER tab style (TabBar.qml:184, 206).
    property Item labelItem: label

    /// Width WITHOUT extraMargin. ScopeTabBar sums these to work out
    /// extraMargin, so this must not depend on it. Silica splits it the same
    /// way, for the same reason (TabButton.qml:61 vs :63).
    property real tabContentWidth: 2 * Theme.paddingLarge + label.implicitWidth

    // BackgroundItem defaults to `width: parent.width` (BackgroundItem.qml:55).
    // Inside a Row every child must size itself, so this is set explicitly.
    width: button.tabContentWidth
           + (button.tabIndex === 0 ? button.extraMargin : 0)
           + (button.tabIndex === button.tabCount - 1 ? button.extraMargin : 0)

    // TabButton.qml:67-68. This one line is the whole landscape adaptation:
    // the strip drops from a tall row to a short one, labels keep their size.
    height: Math.max(button.portrait ? Theme.itemSizeLarge : Theme.itemSizeSmall,
                     label.implicitHeight
                     + 2 * (button.portrait ? Theme.paddingLarge : Theme.paddingMedium))

    Label {
        id: label

        // TabButton.qml:98-107: the outer tabs hug the inside edge, so their
        // extraMargin padding stays outside the text and the underline ends up
        // the width of the word rather than the word plus the slack.
        x: {
            if (button.tabCount > 1 && button.tabIndex === 0) {
                return button.width - label.width - Theme.paddingMedium
            }
            if (button.tabCount > 1 && button.tabIndex === button.tabCount - 1) {
                return Theme.paddingMedium
            }
            return (button.width - label.width) / 2
        }
        y: (button.height - label.height) / 2

        text: button.title
        // §9.3 rule 1. These are qsTr() constants, but the rule has no
        // exceptions. Worth noting that Silica's own TabButton would NOT be
        // safe here: `title` aliases only `titleLabel.text` (TabButton.qml:46)
        // and that Label sets no textFormat, so it runs as Text.AutoText.
        textFormat: Text.PlainText
        font.pixelSize: button.titleFontSize
        // TabButton.qml:122 cross-fades primary -> highlight with the drag
        // position. There is no drag here, so it is a plain two-state colour.
        color: button.highlighted || button.current ? Theme.highlightColor
                                                    : Theme.primaryColor
    }
}
