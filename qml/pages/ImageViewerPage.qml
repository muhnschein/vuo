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

    /// The image, exactly as the article view loaded it.
    property string source: ""
    /// The image's alt text. FOREIGN TEXT, so PlainText below.
    property string alt: ""
    /// How wide the article drew this image, 0 when it is not known. The
    /// article's decode at that size is still in Qt's cache -- the article is
    /// the page underneath -- and asking for the same size is what makes the
    /// first frame here cost nothing.
    property real previewWidth: 0

    /// Whether the page is in place: its transition is over. Until then a
    /// decode and a texture upload are the frames the transition drops, so
    /// the full image waits for this and the preview carries the page there.
    property bool settled: false
    onStatusChanged: if (page.status === PageStatus.Active) page.settled = true
    Component.onCompleted: if (page.status === PageStatus.Active) page.settled = true

    /// The image's own size, which the page is laid out from: the full
    /// image's once it is decoded, the preview's until then -- same shape
    /// either way. 0 until either has one. (Plain properties, so the viewer's
    /// test can give the page an image without decoding one.)
    property real naturalWidth: picture.implicitWidth > 0 && picture.implicitHeight > 0
                                ? picture.implicitWidth : preview.implicitWidth
    property real naturalHeight: picture.implicitWidth > 0 && picture.implicitHeight > 0
                                 ? picture.implicitHeight : preview.implicitHeight

    /// How much bigger than fitted the image is drawn. 1 fits it.
    property real zoom: 1
    readonly property real maximumZoom: 5

    /// The size the image is drawn at when it is fitted to the page. Falls
    /// back to the page until the image has been decoded, which is also what
    /// keeps this from dividing by zero.
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

    /// The most pixels an image is decoded to on either edge: twice the
    /// screen's, so a zoom of two is still sharp. Qt only scales down a
    /// source larger than the size asked for, so this is also what keeps a
    /// 30-megapixel image from being held at full size.
    readonly property int decodeBound: 2 * Math.max(Screen.width, Screen.height)

    /// Kept inside the content. The flickable would do this itself, but only
    /// once it has been laid out, and this runs before that.
    function within(value, limit) {
        return Math.max(0, Math.min(Math.max(0, limit), value))
    }

    /// Where the image's leading edge sits when it is smaller than the view.
    function inset(size, viewport) {
        return Math.max(0, (Math.max(size, viewport) - size) / 2)
    }

    /// Change the zoom, keeping whatever is under (`viewX`, `viewY`) under it
    /// afterwards. The coordinates are the view's, not the image's: mixing
    /// the two, or leaving out the inset that centres a small image, is what
    /// throws the reader somewhere else on a double tap.
    function zoomAt(target, viewX, viewY) {
        var next = Math.max(1, Math.min(page.maximumZoom, target))
        var wide = page.fittedWidth * page.zoom
        var high = page.fittedHeight * page.zoom
        if (wide <= 0 || high <= 0 || next === page.zoom) {
            return
        }
        // Where that point sits in the image, 0..1, before the change.
        var across = (flick.contentX + viewX - page.inset(wide, flick.width)) / wide
        var down = (flick.contentY + viewY - page.inset(high, flick.height)) / high

        page.zoom = next

        var wideAfter = page.fittedWidth * next
        var highAfter = page.fittedHeight * next
        // And where it has to be for that point to stay put on the screen.
        flick.contentX = page.within(
            page.inset(wideAfter, flick.width) + across * wideAfter - viewX,
            Math.max(wideAfter, flick.width) - flick.width)
        flick.contentY = page.within(
            page.inset(highAfter, flick.height) + down * highAfter - viewY,
            Math.max(highAfter, flick.height) - flick.height)
    }

    /// What a double tap does: in on what was tapped, or back out.
    function toggleZoom(viewX, viewY) {
        page.zoomAt(page.zoom > 1 ? 1 : 3, viewX, viewY)
    }

    allowedOrientations: Orientation.All

    // The window is transparent -- the ambience is drawn behind it -- and a
    // photograph is not something to look at through a wallpaper.
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

            /// Where the zoom was when the fingers went down: a pinch reports
            /// its scale relative to its own start, not to the image.
            property real startZoom: 1
            /// And where on the screen they went down, which is the point the
            /// image has to stay still under.
            property real focusX: 0
            property real focusY: 0

            onPinchStarted: {
                pincher.startZoom = page.zoom
                // This area is the flickable's content, so its coordinates
                // are content coordinates; zoomAt wants the view's.
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
                // Centred while the image is smaller than the view, and at
                // the origin once it is bigger and the flickable pans it.
                x: Math.max(0, (flick.contentWidth - frame.width) / 2)
                y: Math.max(0, (flick.contentHeight - frame.height) / 2)

                // The article's decode of the same image, at the size the
                // article asked for (ArticlePage.qml): a cache hit, under the
                // full image and gone once that is there.
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
                    // Once the page is in place when a preview carries it
                    // there; at once when none does, since an empty page is
                    // worse than a dropped frame.
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

    // Only while the image is still coming and nothing stands in for it.
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
        // §9.3: the feed's words.
        textFormat: Text.PlainText
        text: page.alt
        font.pixelSize: Theme.fontSizeExtraSmall
        color: Theme.secondaryColor
    }
}
