import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * One image from an article, on its own, to zoom into.
 *
 * Opened by a tap on an image in the article view -- unless the image is a
 * link, where a tap keeps meaning "go there" (see ArticlePage.qml). Pinch to
 * zoom, drag to pan, double-tap to zoom in on a spot or back out to the whole
 * image.
 *
 * The image is fitted at zoom 1 and multiplied from there, and the
 * flickable's content is the larger of the image and the view -- so panning
 * does nothing until there is something to pan, and the image stays centred
 * while there is not. The flickable stays interactive throughout: a
 * Flickable that is not interactive does not pass the second finger of a
 * pinch on to the PinchArea inside it either.
 *
 * It arrives in two steps. The article has already decoded the image at the
 * width it drew it, and that decode is what this page draws first: a cache
 * hit, so nothing decodes behind the transition. The full-size decode starts
 * once the page is in place.
 *
 * `source` is the URL the article already showed, so it went through the same
 * §9.3 gate in Rust -- proxied, or from an origin the reader agreed to -- and
 * this page cannot be made to contact anything the article view would not.
 */
Page {
    id: page

    /// Image exactly as article view loaded it.
    property string source: ""
    /// Image alt text. FOREIGN TEXT, PlainText below.
    property string alt: ""
    /// Width article drew image at, 0 if unknown. Article (page underneath)
    /// decode at that size still in Qt cache; same size = free first frame.
    property real previewWidth: 0

    /// Page in place: transition over. Before that, decode + texture upload =
    /// dropped transition frames; full image waits, preview carries page.
    property bool settled: false
    onStatusChanged: if (page.status === PageStatus.Active) page.settled = true
    Component.onCompleted: if (page.status === PageStatus.Active) page.settled = true

    /// Image's own size, layout base: full image's once decoded, preview's
    /// until then (same shape). 0 until either has one. Plain properties:
    /// viewer test can give image without decoding.
    property real naturalWidth: picture.implicitWidth > 0 && picture.implicitHeight > 0
                                ? picture.implicitWidth : preview.implicitWidth
    property real naturalHeight: picture.implicitWidth > 0 && picture.implicitHeight > 0
                                 ? picture.implicitHeight : preview.implicitHeight

    /// Zoom over fitted size. 1 = fits.
    property real zoom: 1
    readonly property real maximumZoom: 5

    /// Drawn size when fitted to page. Falls back to page until decoded; also
    /// avoids divide by zero.
    readonly property real fittedWidth: {
        if (page.naturalWidth <= 0 || page.naturalHeight <= 0) {
            return page.width
        }
        return Math.min(page.width, page.height * page.naturalWidth / page.naturalHeight)
    }
    readonly property real fittedHeight:
        page.naturalWidth > 0 && page.naturalHeight > 0
        ? page.fittedWidth * page.naturalHeight / page.naturalWidth
        : page.height

    /// Max decoded pixels per edge: twice screen's, zoom 2 still sharp. Qt only
    /// downscales source larger than requested; also stops 30-megapixel image
    /// held at full size.
    readonly property int decodeBound: 2 * Math.max(Screen.width, Screen.height)

    /// Clamp inside content. Flickable does it only after layout; this runs
    /// before.
    function within(value, limit) {
        return Math.max(0, Math.min(Math.max(0, limit), value))
    }

    /// Image leading edge when smaller than view.
    function inset(size, viewport) {
        return Math.max(0, (Math.max(size, viewport) - size) / 2)
    }

    /// Change zoom, keep what's under (`viewX`, `viewY`) there. View coords,
    /// not image's: mixing them, or dropping small-image centring inset, throws
    /// reader elsewhere on double tap.
    function zoomAt(target, viewX, viewY) {
        var next = Math.max(1, Math.min(page.maximumZoom, target))
        var wide = page.fittedWidth * page.zoom
        var high = page.fittedHeight * page.zoom
        if (wide <= 0 || high <= 0 || next === page.zoom) {
            return
        }
        // Point's position in image, 0..1, before change.
        var across = (flick.contentX + viewX - page.inset(wide, flick.width)) / wide
        var down = (flick.contentY + viewY - page.inset(high, flick.height)) / high

        page.zoom = next

        var wideAfter = page.fittedWidth * next
        var highAfter = page.fittedHeight * next
        // Where it must be to stay put on screen.
        flick.contentX = page.within(
            page.inset(wideAfter, flick.width) + across * wideAfter - viewX,
            Math.max(wideAfter, flick.width) - flick.width)
        flick.contentY = page.within(
            page.inset(highAfter, flick.height) + down * highAfter - viewY,
            Math.max(highAfter, flick.height) - flick.height)
    }

    /// Double tap: zoom in on tapped point, or back out.
    function toggleZoom(viewX, viewY) {
        page.zoomAt(page.zoom > 1 ? 1 : 3, viewX, viewY)
    }

    allowedOrientations: Orientation.All

    // Window transparent (ambience behind); photo shouldn't show through
    // wallpaper.
    Rectangle {
        anchors.fill: parent
        color: "black"
    }

    SilicaFlickable {
        id: flick
        objectName: "imageFlick"

        anchors.fill: parent
        contentWidth: Math.max(flick.width, frame.width)
        contentHeight: Math.max(flick.height, frame.height)
        clip: true

        PinchArea {
            id: pincher

            width: flick.contentWidth
            height: flick.contentHeight
            pinch.minimumScale: 1
            pinch.maximumScale: page.maximumZoom

            /// Zoom at fingers down: pinch scale relative to own start, not
            /// image.
            property real startZoom: 1
            /// Screen point where fingers went down: image stays still under
            /// it.
            property real focusX: 0
            property real focusY: 0

            onPinchStarted: {
                pincher.startZoom = page.zoom
                // Area = flickable content, coords are content coords; zoomAt
                // wants view's.
                pincher.focusX = pinch.startCenter.x - flick.contentX
                pincher.focusY = pinch.startCenter.y - flick.contentY
            }
            onPinchUpdated: page.zoomAt(pincher.startZoom * pinch.scale,
                                        pincher.focusX, pincher.focusY)

            Item {
                id: frame
                objectName: "imageFrame"

                width: page.fittedWidth * page.zoom
                height: page.fittedHeight * page.zoom
                // Centred while smaller than view; at origin once bigger
                // (flickable pans).
                x: Math.max(0, (flick.contentWidth - frame.width) / 2)
                y: Math.max(0, (flick.contentHeight - frame.height) / 2)

                // Article's decode of same image at article's requested size
                // (ArticlePage.qml): cache hit, under full image, gone once
                // that arrives.
                Image {
                    id: preview
                    anchors.fill: parent
                    fillMode: Image.PreserveAspectFit
                    asynchronous: true
                    sourceSize.width: page.previewWidth
                    sourceSize.height: page.previewWidth * 4
                    source: page.previewWidth > 0 ? page.source : ""
                    visible: picture.status !== Image.Ready
                }

                Image {
                    id: picture
                    anchors.fill: parent
                    fillMode: Image.PreserveAspectFit
                    asynchronous: true
                    smooth: !flick.moving
                    sourceSize.width: page.decodeBound
                    sourceSize.height: page.decodeBound
                    // After page in place when preview carries it; at once when
                    // none (empty page worse than dropped frame).
                    source: page.settled || page.previewWidth <= 0 ? page.source : ""
                }

                MouseArea {
                    id: tap
                    anchors.fill: parent
                    onDoubleClicked: {
                        var point = tap.mapToItem(flick, mouse.x, mouse.y)
                        page.toggleZoom(point.x, point.y)
                    }
                }
            }
        }
    }

    // Only while image loading and nothing stands in.
    BusyIndicator {
        anchors.centerIn: parent
        size: BusyIndicatorSize.Large
        running: picture.status === Image.Loading && preview.status !== Image.Ready
    }

    Label {
        visible: picture.status === Image.Error && preview.status !== Image.Ready
        anchors.centerIn: parent
        width: parent.width - Theme.horizontalPageMargin * 2
        horizontalAlignment: Text.AlignHCenter
        wrapMode: Text.Wrap
        textFormat: Text.PlainText
        color: Theme.secondaryHighlightColor
        text: qsTr("The image could not be loaded.")
    }

    Label {
        visible: page.alt.length > 0 && page.zoom <= 1
                 && (picture.status === Image.Ready || preview.status === Image.Ready)
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
        // §9.3: feed's words.
        textFormat: Text.PlainText
        text: page.alt
        font.pixelSize: Theme.fontSizeExtraSmall
        color: Theme.secondaryColor
    }
}
