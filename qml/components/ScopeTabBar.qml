import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * A horizontal tab strip with the active tab underlined -- the strip the clock
 * and Settings apps wear at the top of the screen.
 *
 * Silica HAS this widget. It is TabBar + TabButton, and it lives in
 * Sailfish.Silica.private (private/qmldir:23-26); `grep -n Tab` over the PUBLIC
 * qmldir returns nothing at all. It is not imported here for three reasons,
 * in order of weight:
 *
 *   1. TabBar only functions inside a TabView -- it locates its view by walking
 *      up the parent chain for `__silica_tab_view` (TabBar.qml:43) and reads
 *      `_tabView._page`, `.currentIndex`, `.dragging`, `._distance` off it. A
 *      TabView is a PagedView of separate tab items, which is a far larger
 *      change than this app needs and one Vuo's single shared EntryModel
 *      cannot currently support (see the note at the bottom of this file).
 *   2. `import Sailfish.Silica.private 1.0` from an app's own QML is a Harbour
 *      validation error, and pins the app to an unversioned OMP API.
 *   3. TabBar has runtime dependencies Vuo would inherit silently:
 *      Qt.createQmlObject on Nemo.Configuration with no failure handling
 *      (TabBar.qml:272-282), and `Util.findPage` + `Screen.topCutout`
 *      (TabBar.qml:72-79).
 *
 * So the geometry is rebuilt here from Sailfish.Silica 1.0 + QtQuick 2.6 only.
 * Every constant is the constant Silica uses, cited to its line.
 *
 * Deliberately NOT a view: it takes `currentIndex` and emits `tabClicked`, and
 * the host owns the scope. That seam is also where a public `PagedView` could
 * later add swipe -- see the closing note.
 */
Item {
    id: root

    /// TRANSLATED CONSTANTS ONLY. Never feed/category name: foreign text,
    /// belongs in EntryListPage's explicit PlainText Label.
    property var titles: []
    property int currentIndex: 0
    /// Host Page, for orientation. NOT named `page`: would shadow caller's `id:
    /// page` on right side of `page: page`, binding strip to itself. Never
    /// assumed non-null: qml_loads.rs instantiates file standalone, no parent,
    /// no page.
    property Item hostPage: null

    signal tabClicked(int index)

    /// `Page.isPortrait` (Page.qml:118). Explicit ternary, not `!hostPage ||
    /// hostPage.isPortrait`: host missing `isPortrait` → `undefined`, `make
    /// qml-load` fails file on "Unable to assign [undefined] to bool". Portrait
    /// = safe default.
    property bool _portrait: root.hostPage ? root.hostPage.isPortrait : true
    /// `row.children` in expression: binding re-runs as Repeater populates
    /// (`itemAt` alone doesn't notify). Silica comma trick, TabBar.qml:50.
    property Item _currentButton: root.currentIndex >= 0 && root.currentIndex < tabs.count
                                  ? (row.children, tabs.itemAt(root.currentIndex))
                                  : null
    /// Strip stands where PageHeader would, so inherits its display-cutout
    /// clearing (PageHeader.qml:66-67). TabBar same, portrait only
    /// (TabBar.qml:75-79).
    property real _topMargin: root._portrait && Screen.topCutout.height > 0
                              ? Math.max(0, Screen.topCutout.height - Theme.paddingLarge)
                              : 0
    /// Set on first tap. Silica gates Behaviors on `_tabView.moving`
    /// (TabBar.qml:115); without equivalent, underline slides in from x=0 on
    /// first frame.
    property bool _animate: false

    implicitHeight: root._topMargin + row.height

    Flickable {
        id: strip

        y: root._topMargin
        width: root.width
        height: row.height
        contentWidth: row.width
        boundsBehavior: Flickable.StopAtBounds          // TabBar.qml:90
        // Nothing to flick when tabs fit (normal for three fixed labels). Also
        // stray horizontal drag can't steal from list below.
        interactive: strip.contentWidth > strip.width

        // Keep active tab centred when strip wider than screen.
        // TabBar.qml:92-110 minus drag interpolation (meaningless without
        // TabView).
        contentX: {
            var button = root._currentButton
            if (!button) {
                return 0
            }
            return Math.max(0, Math.min(strip.contentWidth - strip.width,
                                        button.x + (button.width - strip.width) / 2))
        }
        // Silica animates recentring with SmoothedAnimation
        // (TabBar.qml:112-121). Not copied: `Behavior on contentX` puts bare
        // camelCase `contentX` in value position; qml_api_contract.rs treats
        // such words as model roles unless QML declares them → build fails.
        // Only reachable when titles overflow (rare, font-shrink rule below).
        // Also keeps `Theme.pixelRatio` out of stub.

        Row {
            id: row

            /// TabBar.qml:129-136. Reads `tabContentWidth` (extraMargin-free),
            /// so no binding loop. `|| 0` for Repeater, itself a child.
            property real extraMargin: {
                var total = 0
                for (var i = 0; i < row.children.length; ++i) {
                    total += row.children[i].tabContentWidth || 0
                }
                return Math.max(0, strip.width - total) / 2
            }

            /// Drop one font size when titles don't fit. TabBar.qml:137-154.
            ///
            /// FontMetrics, not button widths read back: those depend on this value
            /// → binding loop `make qml-load` fails on.
            property int titleFontSize: {
                var total = 0
                for (var i = 0; i < root.titles.length; ++i) {
                    total += largeMetrics.advanceWidth(root.titles[i]) + Theme.paddingLarge * 2
                }
                return total > root.width ? Theme.fontSizeMedium : Theme.fontSizeLarge
            }

            Repeater {
                id: tabs

                // `titles.length`, not `titles`: delegate reads
                // `root.titles[index]`, not `modelData`. Same result, keeps
                // bare camelCase word out of delegate (else qml_api_contract.rs
                // must be told).
                model: root.titles.length

                ScopeTabButton {
                    title: root.titles[index]
                    tabIndex: index
                    tabCount: tabs.count
                    current: index === root.currentIndex
                    portrait: root._portrait
                    titleFontSize: row.titleFontSize
                    extraMargin: row.extraMargin
                    onClicked: {
                        // Animate only on tap: underline snaps on first layout,
                        // slides only when user moved it.
                        root._animate = true
                        root.tabClicked(index)
                    }
                }
            }
        }

        // THE UNDERLINE. TabBar.qml:176-224, vanilla branch: Theme._lineWidth
        // rule in highlight colour, as wide as active tab's TEXT, 200 ms
        // SmoothedAnimation. Child of Flickable content: scrolls with tabs on
        // narrow screen.
        Rectangle {
            id: underline

            property Item _label: root._currentButton ? root._currentButton.labelItem : null

            x: underline._label ? root._currentButton.x + underline._label.x : 0
            y: underline._label
               ? underline._label.y + underline._label.height + Theme.paddingMedium
               : 0
            width: underline._label ? underline._label.width : 0
            height: Theme._lineWidth                     // TabBar.qml:209
            color: Theme.highlightColor                  // TabBar.qml:210

            Behavior on x {
                enabled: root._animate
                SmoothedAnimation { duration: 200; easing.type: Easing.InOutQuad }
            }
            Behavior on width {
                enabled: root._animate
                SmoothedAnimation { duration: 200; easing.type: Easing.InOutQuad }
            }
        }
    }

    FontMetrics {                                        // TabBar.qml:262-265
        id: largeMetrics
        font.pixelSize: Theme.fontSizeLarge
    }

    // TO ADD SWIPE LATER: public `PagedView` ("Sailfish.Silica/PagedView 1.0",
    // plugins.qmltypes:1252) between strip and lists, bind currentIndex both
    // ways, read `view.itemAt(i).x / (view.width + view.horizontalSpacing)` for
    // TabButton drag cross-fade (TabButton.qml:86-92). `_distance` not
    // exported, `itemAt()` is; strip can follow finger. First need:
    // 1. Delegate must NOT use `PagedView.isCurrentItem`: ATTACHED property
    //    (plugins.qmltypes:1332-1341), can't stub, `make qml-load` loses file.
    //    Compare `view.currentIndex === index`.
    // 2. Each tab OWN EntryModel: neighbours live mid-drag, shared model paints
    //    same rows in both.
    // 3. Blocker: local mutations don't bump sync generation.
    //    `apply_local_status`/`apply_local_starred` (worker.rs:390-402) only
    //    queue to outbox; sole `bump()` in worker loop (worker.rs:179);
    //    `setRead`/`setStarred` `mark_row` THEMSELVES (models.rs:214, :231).
    //    Per-tab models disagree on read/starred until network sync.
    //    Tap-switching fine: `setScope` always calls `reload()`
    //    (models.rs:332-335).
}
