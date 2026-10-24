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
 * `ink` and cuts the three shapes the app needs in it, all of which are
 * geometry only the app knows:
 *
 *   - a band at the top the texture fades in beneath (`fadeFrom`..`fadeTo`),
 *     for a heading to sit in;
 *   - a disc it fades out of (`clearRadius` around `clearX`, `clearY`), for
 *     a title in the middle;
 *   - a band at the foot it sinks away into (`fadeOutFrom`..`fadeOutTo`),
 *     for the cover's status line.
 *
 * All three are off by default. Cutting them here rather than baking them
 * in keeps the disc exactly where the page's title actually is rather than
 * where it was guessed to, and lets the cover's foot come and go with the
 * thing it is making room for.
 *
 * The mask keeps its own proportions and is centred, never stretched: the
 * shader crops it to whatever shape it is asked to fill, which is why one
 * master covers every screen from 9:16 to 9:21.
 */
Item {
    id: art

    /// Mask to draw, URL relative to setting file. qml/art/: `onboarding.png`
    /// (~115 glyph heights across) and `cover/` (64): one mask per unread
    /// count; on cover count = negative space lines flow around, whole pattern
    /// depends on it.
    property url source

    /// Ink colour and amount.
    property color colour: Theme.primaryColor
    property real ink: 0.55

    /// Top band: nothing above `fadeFrom`, full from `fadeTo` down. Off while
    /// `fadeTo <= fadeFrom`.
    property real fadeFrom: 0
    property real fadeTo: 0

    /// Foot band: full above `fadeOutFrom`, nothing from `fadeOutTo` down. Off
    /// while `fadeOutTo <= fadeOutFrom`.
    ///
    /// Ramp EASED, not linear: keeps most strength through top of band, drops
    /// fast near bottom = texture running out. Linear reads as flat wash over
    /// pattern.
    property real fadeOutFrom: 0
    property real fadeOutTo: 0

    /// Disc: nothing within `clearRadius` of `clearX`, `clearY`; full
    /// `clearFeather` beyond. Off while `clearRadius` 0.
    property real clearX: 0
    property real clearY: 0
    property real clearRadius: 0
    property real clearFeather: 0

    /// Second mask OVER first, own colour: outline of shape pattern flows
    /// around. Empty = none.
    ///
    /// Mask, not font-drawn: digits set in face phone lacks (tools/textart/
    /// fetches it; not Sail Sans Pro), sized by painter's one-off fit against
    /// master, cropped with pattern. Drawing here must reproduce all three,
    /// misses silhouette by any error. Same painter, same size: can't miss,
    /// takes crop below texel for texel.
    ///
    /// Beside pattern, not inside: different colours (pattern = theme text
    /// colour at fraction ink; this = ambience highlight full), one coverage
    /// channel can't say both. ~2% of mask bytes; second channel in same file
    /// measured 13% (breaks pattern compression).
    property url edgeSource
    property color edgeColour: Theme.highlightColor
    property real edgeInk: 1.0

    /// Mask itself, never drawn, only sampled. Full size on purpose: capping
    /// needs master proportions (unknown until loaded); page shows once, cover
    /// keeps it in Qt pixmap cache.
    Image {
        id: mask

        source: art.source
        visible: false
        asynchronous: true
    }

    /// Outline loaded SYNCHRONOUSLY; pattern isn't.
    ///
    /// Separate loads; shader shows once PATTERN ready, outline waits own
    /// `Ready`. Async outline = new pattern painted without line until second
    /// load (line pops in frame late on every count change). Inline puts it
    /// there first.
    ///
    /// Affordable: outline ~1.5 KiB vs pattern's 66; cover changes with count,
    /// not per frame.
    Image {
        id: edgeMask

        source: art.edgeSource
        visible: false
        asynchronous: false
    }

    ShaderEffect {
        anchors.fill: parent
        // Nothing to sample until loaded; shader over empty texture draws
        // colour block.
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
        property real fadeOutFrom: art.fadeOutFrom
        property real fadeOutTo: art.fadeOutTo
        property variant clearAt: Qt.point(art.clearX, art.clearY)
        property real clearRadius: art.clearRadius
        property real clearFeather: art.clearFeather

        // Sampler wants texture regardless; no outline → points at pattern,
        // `edgeInk` 0: sampled and multiplied away, not unbound.
        property variant edge: edgeMask.status === Image.Ready ? edgeMask : mask
        property color edgeTint: art.edgeColour
        property real edgeInk: edgeMask.status === Image.Ready ? art.edgeInk : 0

        // Fixed text, like every shader here: nothing foreign near it (§9.3).
        // Qt passes `tint` premultiplied; theme colours opaque, so rgb =
        // colour.
        fragmentShader: "
            varying highp vec2 qt_TexCoord0;
            uniform sampler2D source;
            uniform lowp vec4 tint;
            uniform lowp float ink;
            uniform highp vec2 size;
            uniform highp float srcAspect;
            uniform highp float fadeFrom;
            uniform highp float fadeTo;
            uniform highp float fadeOutFrom;
            uniform highp float fadeOutTo;
            uniform highp vec2 clearAt;
            uniform highp float clearRadius;
            uniform highp float clearFeather;
            uniform sampler2D edge;
            uniform lowp vec4 edgeTint;
            uniform lowp float edgeInk;
            uniform lowp float qt_Opacity;

            void main() {
                // Cover item with mask, keep mask proportions, centred: pattern
                // cropped, not stretched.
                highp float itemAspect = size.x / size.y;
                highp vec2 span = vec2(1.0, 1.0);
                if (itemAspect > srcAspect) {
                    span.y = srcAspect / itemAspect;
                } else {
                    span.x = itemAspect / srcAspect;
                }
                highp vec2 uv = (qt_TexCoord0 - 0.5) * span + 0.5;

                // App's three cut shapes in one factor before either layer
                // drawn. BOTH take it: outline staying while pattern sinks
                // under status line = seam this avoids. highp, not lowp:
                // factors multiply, foot ramp spans hundreds of pixels; lowp
                // rounding bands where eye wants smoothness.
                highp vec2 px = qt_TexCoord0 * size;
                highp float cut = 1.0;
                if (fadeTo > fadeFrom) {
                    cut *= clamp((px.y - fadeFrom) / (fadeTo - fadeFrom), 0.0, 1.0);
                }
                if (fadeOutTo > fadeOutFrom) {
                    highp float sink = clamp((fadeOutTo - px.y)
                                             / (fadeOutTo - fadeOutFrom), 0.0, 1.0);
                    cut *= sink * sink;
                }
                if (clearRadius > 0.0) {
                    cut *= clamp((distance(px, clearAt) - clearRadius)
                                 / max(1.0, clearFeather), 0.0, 1.0);
                }

                lowp float a = texture2D(source, uv).r * ink * cut;
                lowp float e = texture2D(edge, uv).r * edgeInk * cut;

                // Premultiplied (scene graph composites that). Line OVER
                // pattern, not beside: where ring runs under line, line shows,
                // ambience colour unmixed.
                lowp vec4 line = vec4(edgeTint.rgb, 1.0) * e;
                lowp vec4 base = vec4(tint.rgb, 1.0) * a;
                gl_FragColor = (line + base * (1.0 - e)) * qt_Opacity;
            }"
    }
}
