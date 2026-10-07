import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One image from an article, on its own, to zoom into.
 *
 * Opened by a tap on an image in the article view -- unless the image is a
 * link, where a tap keeps meaning "go there" (see ArticlePage.qml). Pinch to
 * zoom, drag to pan, double-tap to zoom in on a spot or back out to the whole
 * image. Swiping back is the way out, as everywhere on the platform, and it is
 * only offered at the fitted size: zoomed in, a horizontal drag is a pan.
 *
 * `source` is the URL the article already showed, so it went through the same
 * §9.3 gate in Rust -- proxied, or from an origin the reader agreed to -- and
 * this page cannot be made to contact anything the article view would not.
 */
Page {
    id: page

    /// The image, exactly as the article view loaded it.
    property string source: ""
    /// The image's alt text. FOREIGN TEXT, so PlainText below.
    property string alt: ""

    /// Magnification over the fitted size: 1 shows the whole image.
    property real zoom: 1.0
    /// How far in a pinch may go. Past this the decode bound below means the
    /// reader is looking at upscaled pixels, which shows nothing new.
    readonly property real maxZoom: 4.0
    /// Where a double-tap takes the zoom from the fitted size.
    readonly property real doubleTapZoom: 2.5

    allowedOrientations: Orientation.All
    backNavigation: page.zoom <= 1.0

    /// Set the zoom, keeping the content point (`cx`, `cy`) where it is on
    /// screen -- which is what makes a pinch feel anchored under the fingers.
    function zoomAround(target, cx, cy) {
        var next = Math.max(1.0, Math.min(page.maxZoom, target))
        var ratio = next / page.zoom
        var vx = cx - flick.contentX
        var vy = cy - flick.contentY
        page.zoom = next
        flick.contentX = cx * ratio - vx
        flick.contentY = cy * ratio - vy
    }

    // The window is transparent -- the ambience is drawn behind it -- and a
    // photograph is not something to look at through a wallpaper.
    Rectangle {
        anchors.fill: parent
        color: Theme.overlayBackgroundColor
    }

    SilicaFlickable {
        id: flick

        anchors.fill: parent
        contentWidth: Math.max(flick.width, picture.fitWidth * page.zoom)
        contentHeight: Math.max(flick.height, picture.fitHeight * page.zoom)
        // At the fitted size there is nothing to pan, and a Flickable that
        // takes the drag anyway would swallow the swipe back.
        interactive: page.zoom > 1.0
        clip: true

        PinchArea {
            id: pinch

            width: flick.contentWidth
            height: flick.contentHeight

            property real startZoom: 1.0

            onPinchStarted: pinch.startZoom = page.zoom
            onPinchUpdated: page.zoomAround(pinch.startZoom * pinch.scale,
                                            pinch.center.x, pinch.center.y)
            onPinchFinished: flick.returnToBounds()

            Image {
                id: picture

                /// The size that shows the whole image on screen.
                readonly property bool _wide: picture.implicitHeight <= 0
                                              || picture.implicitWidth / picture.implicitHeight
                                                 >= flick.width / Math.max(1, flick.height)
                readonly property real fitWidth: picture._wide
                    ? flick.width
                    : flick.height * picture.implicitWidth / Math.max(1, picture.implicitHeight)
                readonly property real fitHeight: picture._wide
                    ? (picture.implicitWidth > 0
                       ? flick.width * picture.implicitHeight / picture.implicitWidth
                       : flick.height)
                    : flick.height

                width: picture.fitWidth * page.zoom
                height: picture.fitHeight * page.zoom
                x: (flick.contentWidth - picture.width) / 2
                y: (flick.contentHeight - picture.height) / 2

                fillMode: Image.PreserveAspectFit
                asynchronous: true
                cache: true
                smooth: !flick.moving
                // A bound on the DECODE, in both dimensions, for the same
                // reason the article view has one: Qt only scales down a
                // source larger than the size asked for, so this is what keeps
                // a 30-megapixel image from being held at full size. Twice the
                // screen leaves zooming in something real to show.
                sourceSize.width: Screen.width * 2
                sourceSize.height: Screen.height * 2
                source: page.source
            }

            MouseArea {
                anchors.fill: parent
                onDoubleClicked: {
                    if (page.zoom > 1.0) {
                        page.zoom = 1.0
                        flick.returnToBounds()
                    } else {
                        page.zoomAround(page.doubleTapZoom, mouse.x, mouse.y)
                        flick.returnToBounds()
                    }
                }
            }
        }
    }

    BusyIndicator {
        anchors.centerIn: parent
        size: BusyIndicatorSize.Large
        running: picture.status === Image.Loading
    }

    Label {
        visible: picture.status === Image.Error
        anchors.centerIn: parent
        width: parent.width - Theme.horizontalPageMargin * 2
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        color: Theme.secondaryHighlightColor
        text: qsTr("The image could not be loaded.")
    }

    Label {
        visible: page.alt.length > 0 && page.zoom <= 1.0
                 && picture.status === Image.Ready
        anchors {
            left: parent.left
            right: parent.right
            bottom: parent.bottom
            margins: Theme.horizontalPageMargin
        }
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        maximumLineCount: 4
        elide: Text.ElideRight
        // §9.3: the feed's words.
        textFormat: Text.PlainText
        text: page.alt
        font.pixelSize: Theme.fontSizeExtraSmall
        color: Theme.secondaryColor
    }
}
