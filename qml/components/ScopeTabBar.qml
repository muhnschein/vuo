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

    /// TRANSLATED CONSTANTS ONLY. Never a feed or category name: those are
    /// foreign text, and they belong in the explicit PlainText Label that
    /// EntryListPage already owns.
    property var titles: []
    property int currentIndex: 0
    /// The host Page, for orientation. Deliberately NOT called `page`: a
    /// property of that name would shadow the caller's `id: page` on the
    /// right-hand side of `page: page` and bind the strip to itself.
    /// Never assumed non-null -- qml_loads.rs instantiates this file
    /// standalone, with no parent and no page.
    property Item hostPage: null

    signal tabClicked(int index)

    /// `Page.isPortrait` (Page.qml:118). Explicitly ternary, not
    /// `!hostPage || hostPage.isPortrait`: with no host that expression is
    /// fine, but with a host whose `isPortrait` is missing it evaluates to
    /// `undefined`, and `make qml-load` fails the file on
    /// "Unable to assign [undefined] to bool". Portrait is the safe default.
    property bool _portrait: root.hostPage ? root.hostPage.isPortrait : true
    /// `row.children` is in the expression so the binding re-runs as the
    /// Repeater populates; `itemAt` alone would not notify. Silica uses the
    /// same comma trick at TabBar.qml:50.
    property Item _currentButton: root.currentIndex >= 0 && root.currentIndex < tabs.count
                                  ? (row.children, tabs.itemAt(root.currentIndex))
                                  : null
    /// This strip stands where PageHeader would, so it inherits PageHeader's
    /// job of clearing a display cutout (PageHeader.qml:66-67). TabBar does the
    /// same, portrait only (TabBar.qml:75-79).
    property real _topMargin: root._portrait && Screen.topCutout.height > 0
                              ? Math.max(0, Screen.topCutout.height - Theme.paddingLarge)
                              : 0
    /// Set on the first tap. Silica gets this free by gating its Behaviors on
    /// `_tabView.moving` (TabBar.qml:115); without something equivalent the
    /// underline slides in from x=0 on the first frame.
    property bool _animate: false

    implicitHeight: root._topMargin + row.height

    Flickable {
        id: strip

        y: root._topMargin
        width: root.width
        height: row.height
        contentWidth: row.width
        boundsBehavior: Flickable.StopAtBounds          // TabBar.qml:90
        // Nothing to flick when the tabs fit, which for three fixed labels is
        // the normal case. Also keeps a stray horizontal drag from stealing
        // from the list underneath.
        interactive: strip.contentWidth > strip.width

        // Keep the active tab centred when the strip is wider than the screen.
        // TabBar.qml:92-110, minus the drag interpolation, which has no
        // meaning without a TabView.
        contentX: {
            var button = root._currentButton
            if (!button) {
                return 0
            }
            return Math.max(0, Math.min(strip.contentWidth - strip.width,
                                        button.x + (button.width - strip.width) / 2))
        }
        // Silica animates this recentring with a SmoothedAnimation
        // (TabBar.qml:112-121). Not copied, for a reason worth writing down:
        // `Behavior on contentX` puts the bare camelCase word `contentX` in a
        // value position, and qml_api_contract.rs treats every such word as a
        // model role unless the QML itself declares it -- so it would fail the
        // build. It is only reachable when the titles overflow the screen,
        // which the font-shrink rule below makes rare, and dropping it also
        // means `Theme.pixelRatio` never has to enter the stub.

        Row {
            id: row

            /// TabBar.qml:129-136. Reads `tabContentWidth`, which is
            /// deliberately extraMargin-free, so this is not a binding loop.
            /// The `|| 0` is for the Repeater, which is itself a child.
            property real extraMargin: {
                var total = 0
                for (var i = 0; i < row.children.length; ++i) {
                    total += row.children[i].tabContentWidth || 0
                }
                return Math.max(0, strip.width - total) / 2
            }

            /// Drop one font size when the titles will not fit. TabBar.qml:137-154.
            ///
            /// Measured with FontMetrics rather than by reading the buttons'
            /// widths back, because those widths depend on this value. Reading
            /// them back is the binding loop `make qml-load` fails on.
            property int titleFontSize: {
                var total = 0
                for (var i = 0; i < root.titles.length; ++i) {
                    total += largeMetrics.advanceWidth(root.titles[i]) + Theme.paddingLarge * 2
                }
                return total > root.width ? Theme.fontSizeMedium : Theme.fontSizeLarge
            }

            Repeater {
                id: tabs

                // `titles.length` rather than `titles`, so the delegate reads
                // `root.titles[index]` instead of `modelData`. Same result, and
                // it keeps a bare camelCase word out of the delegate, which
                // qml_api_contract.rs would otherwise have to be told about.
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
                        // Animate only in response to a tap: the underline
                        // should snap into place on first layout, and slide
                        // only when the user moved it.
                        root._animate = true
                        root.tabClicked(index)
                    }
                }
            }
        }

        // THE UNDERLINE. TabBar.qml:176-224, vanilla branch: a Theme._lineWidth
        // rule in the highlight colour, exactly as wide as the active tab's
        // TEXT, slid across on a 200 ms SmoothedAnimation. A child of the
        // Flickable's content, so on a narrow screen it scrolls with the tabs.
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

    // TO ADD SWIPE LATER: put a public `PagedView` (exported as
    // "Sailfish.Silica/PagedView 1.0", plugins.qmltypes:1252) between this
    // strip and the lists, bind currentIndex both ways, and read
    // `view.itemAt(i).x / (view.width + view.horizontalSpacing)` to reproduce
    // TabButton's drag cross-fade (TabButton.qml:86-92) -- PagedView's
    // `_distance` is not exported, but `itemAt()` is, so the strip can still
    // follow the finger. Three things must be true first:
    //   1. the delegate must NOT use `PagedView.isCurrentItem`. It is an
    //      ATTACHED property (plugins.qmltypes:1332-1341), attached types
    //      cannot be written in a QML stub, and `make qml-load` would lose all
    //      coverage of the file. Compare `view.currentIndex === index` instead.
    //   2. each tab needs its OWN EntryModel, because neighbours are live
    //      mid-drag and one shared model would paint identical rows in both.
    //   3. and that is the blocker: local mutations never bump the sync
    //      generation. `apply_local_status`/`apply_local_starred`
    //      (worker.rs:390-402) only queue to the outbox, the sole `bump()` is
    //      in the worker loop (worker.rs:179), and `setRead`/`setStarred`
    //      compensate with `mark_row` on THEMSELVES (models.rs:214, :231). So
    //      per-tab models would disagree about read and starred state until a
    //      network sync. Tap-switching has no such problem: `setScope` always
    //      calls `reload()` (models.rs:332-335).
}
