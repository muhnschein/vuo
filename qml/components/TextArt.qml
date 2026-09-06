import QtQuick 2.6
import Sailfish.Silica 1.0

/*
 * Vuo's texture: lines of tiny filler text laid along nested, flowing
 * curves, after Jolla's own packaging.
 *
 * The pattern is an IMAGE, painted ahead of time by tools/textart/ and
 * shipped in qml/art/ -- it is not drawn here. It was, once, and two device
 * reports between them retired that: fourteen seconds of a pinned core
 * before the onboarding page's texture appeared, and then, once it was fast
 * enough to watch, that watching it arrive was not wanted either. A picture
 * costs nothing, cannot half-arrive, and looks the same on every phone.
 *
 * It also cannot vanish. The live version painted into a `Canvas`, whose
 * buffer the scene graph drops when the window is hidden; nothing asked for
 * it again on the way back, so leaving the app and returning to it left the
 * page bare. An `Image` reloads itself.
 *
 * # One mask, every ambience
 *
 * What ships is a COVERAGE MASK: one grayscale channel saying how much ink
 * is at each pixel, and no colour at all. This tints it with the theme's
 * own colour, so one file is right on every ambience -- a light one included,
 * where a white picture would be invisible -- and there is nothing to
 * regenerate when Sailfish gains another. The same shader dims the mask to
 * `ink` and cuts the two holes the app needs in it, both of which are
 * geometry only the app knows:
 *
 *   - a band at the top the texture fades in beneath (`fadeFrom`..`fadeTo`),
 *     for a heading to sit in;
 *   - a disc it fades out of (`clearRadius` around `clearX`, `clearY`), for
 *     a title in the middle.
 *
 * Both are off by default. Cutting them here rather than baking them in is
 * what lets one mask serve a cover and a page, and keeps the fade exactly
 * where the heading actually ends rather than where it was guessed to.
 *
 * The mask keeps its own proportions and is centred, never stretched: the
 * shader crops it to whatever shape it is asked to fill, which is why one
 * master covers every screen from 9:16 to 9:21.
 */
Item {
    id: art

    /// The mask to draw, as a URL relative to the file that sets it.
    /// qml/art/ holds one per density: `cover.png` is drawn at about
    /// sixty-four glyph heights across, `onboarding.png` at a hundred and
    /// fifteen, which is the whole of the difference between them.
    property url source

    /// What colour the ink is, and how much of it there is.
    property color colour: Theme.primaryColor
    property real ink: 0.55

    /// The band at the top: nothing above `fadeFrom`, full strength from
    /// `fadeTo` down. Off while `fadeTo <= fadeFrom`.
    property real fadeFrom: 0
    property real fadeTo: 0

    /// The disc: nothing within `clearRadius` of `clearX`, `clearY`, full
    /// strength `clearFeather` beyond it. Off while `clearRadius` is 0.
    property real clearX: 0
    property real clearY: 0
    property real clearRadius: 0
    property real clearFeather: 0

    /// The mask itself, which is never drawn -- only sampled. Loaded at its
    /// full size deliberately: capping it would need the master's own
    /// proportions, which are not known until it has loaded, and a page
    /// shows this once while a cover keeps it in Qt's pixmap cache between
    /// covers.
    Image {
        id: mask

        source: art.source
        visible: false
        asynchronous: true
    }

    ShaderEffect {
        anchors.fill: parent
        // Nothing to sample until it is there; a shader over an empty
        // texture draws a block of colour.
        visible: mask.status === Image.Ready

        property variant source: mask
        property color tint: art.colour
        property real ink: art.ink
        property variant size: Qt.size(Math.max(1, art.width), Math.max(1, art.height))
        property real srcAspect: mask.implicitHeight > 0
                                 ? mask.implicitWidth / mask.implicitHeight
                                 : 1
        property real fadeFrom: art.fadeFrom
        property real fadeTo: art.fadeTo
        property variant clearAt: Qt.point(art.clearX, art.clearY)
        property real clearRadius: art.clearRadius
        property real clearFeather: art.clearFeather

        // Fixed text, as every shader in this app is: nothing foreign is
        // anywhere near it (§9.3). Qt hands `tint` over premultiplied, and
        // the theme's colours are opaque, so its rgb is the colour itself.
        fragmentShader: "
            varying highp vec2 qt_TexCoord0;
            uniform sampler2D source;
            uniform lowp vec4 tint;
            uniform lowp float ink;
            uniform highp vec2 size;
            uniform highp float srcAspect;
            uniform highp float fadeFrom;
            uniform highp float fadeTo;
            uniform highp vec2 clearAt;
            uniform highp float clearRadius;
            uniform highp float clearFeather;
            uniform lowp float qt_Opacity;

            void main() {
                // Cover the item with the mask, keeping the mask's own
                // proportions and centring it, so the pattern is cropped
                // rather than stretched.
                highp float itemAspect = size.x / size.y;
                highp vec2 span = vec2(1.0, 1.0);
                if (itemAspect > srcAspect) {
                    span.y = srcAspect / itemAspect;
                } else {
                    span.x = itemAspect / srcAspect;
                }
                highp vec2 uv = (qt_TexCoord0 - 0.5) * span + 0.5;

                lowp float a = texture2D(source, uv).r * ink;
                highp vec2 px = qt_TexCoord0 * size;
                if (fadeTo > fadeFrom) {
                    a *= clamp((px.y - fadeFrom) / (fadeTo - fadeFrom), 0.0, 1.0);
                }
                if (clearRadius > 0.0) {
                    a *= clamp((distance(px, clearAt) - clearRadius)
                               / max(1.0, clearFeather), 0.0, 1.0);
                }
                // Premultiplied, which is what the scene graph composites.
                gl_FragColor = vec4(tint.rgb, 1.0) * (a * qt_Opacity);
            }"
    }
}
