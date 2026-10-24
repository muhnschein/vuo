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

    /// TRANSLATED CONSTANT, never foreign text. See Label below.
    property string title
    property bool current: false
    property bool portrait: true
    property int titleFontSize: Theme.fontSizeLarge
    /// Half strip slack, folded into first and last tab: fitting strip ends
    /// centred. TabButton.qml:63-65.
    property real extraMargin: 0
    property int tabIndex: 0
    property int tabCount: 1

    /// Underline tracks TEXT, not button: Silica reads active TabButton's
    /// `contentItem.x`/`.width` (TabBar.qml:181, 204); TabButton contentItem IS
    /// text column (TabButton.qml:58). Can't be named `contentItem`:
    /// BackgroundItem uses that for full-width press rectangle
    /// (BackgroundItem.qml:49) = Silica's OTHER tab style (TabBar.qml:184,
    /// 206).
    property Item labelItem: label

    /// Width WITHOUT extraMargin. ScopeTabBar sums these for extraMargin;
    /// mustn't depend on it. Silica splits same way (TabButton.qml:61 vs :63).
    property real tabContentWidth: 2 * Theme.paddingLarge + label.implicitWidth

    // BackgroundItem defaults `width: parent.width` (BackgroundItem.qml:55). In
    // Row every child sizes itself; set explicitly.
    width: button.tabContentWidth
           + (button.tabIndex === 0 ? button.extraMargin : 0)
           + (button.tabIndex === button.tabCount - 1 ? button.extraMargin : 0)

    // TabButton.qml:67-68. Whole landscape adaptation: strip tall → short,
    // labels keep size.
    height: Math.max(button.portrait ? Theme.itemSizeLarge : Theme.itemSizeSmall,
                     label.implicitHeight
                     + 2 * (button.portrait ? Theme.paddingLarge : Theme.paddingMedium))

    Label {
        id: label

        // TabButton.qml:98-107: outer tabs hug inside edge; extraMargin padding
        // stays outside text, underline = word width, not word + slack.
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
        // §9.3 rule 1. qsTr() constants, but rule has no exceptions. Silica
        // TabButton NOT safe here: `title` aliases `titleLabel.text`
        // (TabButton.qml:46), no textFormat → Text.AutoText.
        textFormat: Text.PlainText
        font.pixelSize: button.titleFontSize
        // TabButton.qml:122 cross-fades primary → highlight with drag. No drag
        // here: plain two-state colour.
        color: button.highlighted || button.current ? Theme.highlightColor
                                                    : Theme.primaryColor
    }
}
