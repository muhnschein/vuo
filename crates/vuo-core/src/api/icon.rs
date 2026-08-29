//! Feed icons: decoding, and validating them by content rather than by claim.
//!
//! §9.3: *validate images by content, not by claimed type. Check magic bytes,
//! cap decoded dimensions, and let decode failures be non-fatal. A feed icon
//! is not a reason to kill the process.*
//!
//! Icons are the most attacker-controlled bytes in the whole application. They
//! originate at whatever website the feed points to, they are fetched
//! automatically without the user ever choosing to view them, and they get
//! decoded by a C++ image library on the phone. Everything here is therefore
//! deliberately paranoid and deliberately cheap: the header is parsed by hand,
//! no decoder is invoked, and anything unrecognised is refused.
//!
//! Note the response shape is not a `data:` URI even though it looks like one.
//! Miniflux sends `"image/png;base64,iVBOR..."` -- mime type, semicolon,
//! encoding, comma, payload, but no `data:` scheme prefix.

use base64::Engine as _;

use crate::api::wire;
use crate::error::{Error, Result};
use crate::model::{Icon, IconId, ImageFormat};

/// Caps applied to every icon before it reaches Qt's image decoder.
#[derive(Debug, Clone, Copy)]
pub struct IconLimits {
    /// Maximum decoded byte length.
    pub max_bytes: usize,
    /// Maximum width or height in pixels.
    ///
    /// This is the "cap decoded dimensions" rule. It is enforced from the
    /// *header*, before any decoder allocates a pixel buffer, which is the
    /// only point at which it actually prevents the allocation: a 65535x65535
    /// PNG is a few hundred bytes on the wire and 17 GB decoded.
    pub max_pixels_per_side: u32,
}

impl Default for IconLimits {
    fn default() -> Self {
        // A feed icon is drawn at roughly 32 device pixels. Anything beyond
        // 512 on a side is not an icon.
        IconLimits {
            max_bytes: 512 * 1024,
            max_pixels_per_side: 512,
        }
    }
}

/// Decode and validate the body of `GET /v1/feeds/{id}/icon`.
///
/// Returns an [`Error::Item`] on any problem, so a bad icon costs the feed its
/// icon and nothing else.
pub fn decode_icon(w: &wire::Icon, limits: IconLimits) -> Result<Icon> {
    let reject = |why: &str| Error::item("icon", Some(w.id), why.to_owned());

    // The payload is everything after the first ";base64," separator. Some
    // servers may omit the mime prefix entirely, so fall back to the raw
    // string rather than failing.
    let payload = match w.data.split_once(";base64,") {
        Some((_claimed_mime, b64)) => b64,
        None => w.data.as_str(),
    };
    if payload.is_empty() {
        return Err(reject("empty icon payload"));
    }

    // Cap before decoding: base64 inflates by 4/3, so a length check on the
    // encoded form bounds the decoded allocation.
    // Base64 encodes 3 bytes as 4 characters, so the decoded length is at
    // most 3/4 of the encoded length. Truncating division rounds this estimate
    // DOWN, which is the safe direction: it can only make the pre-check more
    // permissive, and the exact check on the decoded bytes below is the one
    // that actually enforces the cap. The point of this one is to avoid
    // allocating for a payload that is obviously far too big.
    #[allow(clippy::integer_division)]
    let decoded_upper_bound = payload.len() / 4 * 3;
    if decoded_upper_bound > limits.max_bytes {
        // Distinct wording from the post-decode check below, so a test can
        // tell WHICH gate refused. With one shared message the pre-check could
        // be deleted outright and every icon test stayed green -- the check
        // whose whole job is to avoid allocating for an obviously huge payload.
        return Err(reject("icon exceeds the size cap (declared)"));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| reject("icon payload is not valid base64"))?;

    if bytes.len() > limits.max_bytes {
        return Err(reject("icon exceeds the size cap (decoded)"));
    }

    // The claimed `mime_type` is deliberately ignored here. It is foreign
    // text, and believing it is exactly the bug §9.3 names.
    let format = sniff_format(&bytes).ok_or_else(|| reject("unrecognised image format"))?;

    if format == ImageFormat::Svg {
        // Refused on purpose. SVG is an XML dialect: supporting it means
        // exposing an XML parser, entity expansion and an external-reference
        // resolver to bytes chosen by a feed operator, in exchange for a
        // 32-pixel favicon. The cost/benefit is not close. Feeds whose only
        // icon is SVG simply render with the default icon.
        return Err(reject("SVG icons are not rendered"));
    }

    let dimensions = read_dimensions(&bytes, format);
    if let Some((w_px, h_px)) = dimensions {
        if w_px == 0 || h_px == 0 {
            return Err(reject("icon reports a zero dimension"));
        }
        if w_px > limits.max_pixels_per_side || h_px > limits.max_pixels_per_side {
            return Err(reject("icon dimensions exceed the cap"));
        }
    }

    Ok(Icon {
        id: IconId(w.id),
        format,
        bytes,
        dimensions,
    })
}

/// Identify an image format from its leading bytes.
fn sniff_format(b: &[u8]) -> Option<ImageFormat> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    if b.starts_with(PNG) {
        return Some(ImageFormat::Png);
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if b.len() >= 12 && b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return Some(ImageFormat::WebP);
    }
    if b.starts_with(b"BM") {
        return Some(ImageFormat::Bmp);
    }
    if b.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some(ImageFormat::Ico);
    }
    // Sniff SVG only so it can be refused explicitly rather than falling
    // through to "unrecognised", which would be a less honest message.
    let head = b.get(..256).unwrap_or(b);
    if let Ok(text) = std::str::from_utf8(head) {
        let t = text.trim_start();
        if t.starts_with("<?xml") || t.starts_with("<svg") || t.starts_with("<!DOCTYPE svg") {
            return Some(ImageFormat::Svg);
        }
    }
    None
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 4)?;
    Some(u32::from_be_bytes([
        *s.first()?,
        *s.get(1)?,
        *s.get(2)?,
        *s.get(3)?,
    ]))
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at + 2)?;
    Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
}

/// Read pixel dimensions from an image header without decoding it.
///
/// Returns `None` when the format's header is not understood; the caller then
/// simply has no dimension information rather than an error, since a format
/// whose size cannot be read cheaply is not thereby malicious.
fn read_dimensions(b: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    match format {
        // IHDR is always the first chunk: 8-byte signature, 4-byte length,
        // 4-byte type, then width and height.
        ImageFormat::Png => Some((be_u32(b, 16)?, be_u32(b, 20)?)),
        ImageFormat::Gif => Some((u32::from(le_u16(b, 6)?), u32::from(le_u16(b, 8)?))),
        ImageFormat::Bmp => {
            // BITMAPINFOHEADER width/height are signed 32-bit little-endian;
            // a negative height means a top-down bitmap.
            let w = i32::from_le_bytes([*b.get(18)?, *b.get(19)?, *b.get(20)?, *b.get(21)?]);
            let h = i32::from_le_bytes([*b.get(22)?, *b.get(23)?, *b.get(24)?, *b.get(25)?]);
            Some((w.unsigned_abs(), h.unsigned_abs()))
        }
        ImageFormat::Ico => {
            // Width/height of the first directory entry; 0 means 256.
            let w = *b.get(6)?;
            let h = *b.get(7)?;
            Some((
                if w == 0 { 256 } else { u32::from(w) },
                if h == 0 { 256 } else { u32::from(h) },
            ))
        }
        ImageFormat::Jpeg => jpeg_dimensions(b),
        // VP8X carries a 24-bit canvas size; VP8/VP8L encode it differently
        // and are not worth hand-parsing for a favicon.
        ImageFormat::WebP => {
            if b.get(12..16) == Some(b"VP8X") {
                let w = 1
                    + u32::from(*b.get(24)?)
                    + (u32::from(*b.get(25)?) << 8)
                    + (u32::from(*b.get(26)?) << 16);
                let h = 1
                    + u32::from(*b.get(27)?)
                    + (u32::from(*b.get(28)?) << 8)
                    + (u32::from(*b.get(29)?) << 16);
                Some((w, h))
            } else {
                None
            }
        }
        ImageFormat::Svg => None,
    }
}

/// Walk JPEG segments looking for a Start-Of-Frame marker.
///
/// Bounded by construction: every branch advances `i` by at least one, so the
/// walk is O(len) whatever the bytes say, and `len` is already bounded by
/// `IconLimits::max_bytes` before this is reached.
///
/// The iteration cap below is a cheap second ceiling on top of that, not the
/// termination argument. It is deliberately NOT separately observable -- both
/// "hit the cap" and "walked off the end" return `None` -- so no test here
/// distinguishes them, and the test below says so rather than claiming to
/// cover it.
fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize; // skip SOI
    let mut guard = 0u32;
    while i + 3 < b.len() {
        guard += 1;
        if guard > 4096 {
            return None;
        }
        if *b.get(i)? != 0xFF {
            // Not at a marker: resynchronise rather than trusting the offset.
            i += 1;
            continue;
        }
        let marker = *b.get(i + 1)?;
        // Standalone markers carry no length.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 || marker == 0xFF {
            i += 2;
            continue;
        }
        let len = usize::from(be_u16(b, i + 2)?);
        if len < 2 {
            return None;
        }
        // SOF0..SOF15 except the DHT/JPG/DAC markers at C4, C8, CC.
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let h = be_u16(b, i + 5)?;
            let w = be_u16(b, i + 7)?;
            return Some((u32::from(w), u32::from(h)));
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    fn icon_body(bytes: &[u8], claimed: &str) -> wire::Icon {
        wire::Icon {
            id: 1,
            mime_type: claimed.to_owned(),
            data: format!(
                "{claimed};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
        }
    }

    #[test]
    fn a_real_png_decodes() {
        let icon = decode_icon(
            &icon_body(&png_header(32, 32), "image/png"),
            IconLimits::default(),
        )
        .unwrap();
        assert_eq!(icon.format, ImageFormat::Png);
        assert_eq!(icon.dimensions, Some((32, 32)));
    }

    #[test]
    fn the_claimed_mime_type_is_ignored() {
        // §9.3: validate by content, not by claim. A PNG mislabelled as JPEG
        // is still a PNG, and a script mislabelled as PNG is still refused.
        let icon = decode_icon(
            &icon_body(&png_header(16, 16), "image/jpeg"),
            IconLimits::default(),
        )
        .unwrap();
        assert_eq!(icon.format, ImageFormat::Png, "content wins over the label");

        let hostile = icon_body(b"<html><script>alert(1)</script>", "image/png");
        assert!(decode_icon(&hostile, IconLimits::default()).is_err());
    }

    #[test]
    fn a_decompression_bomb_is_refused_from_the_header() {
        // A few hundred bytes on the wire, ~17 GB decoded. The dimension cap
        // has to be enforced before any decoder sees this.
        let bomb = icon_body(&png_header(65_535, 65_535), "image/png");
        let err = decode_icon(&bomb, IconLimits::default()).unwrap_err();
        assert!(
            err.is_item_local(),
            "a bad icon costs the feed its icon, nothing more"
        );
    }

    #[test]
    fn zero_dimensions_are_refused() {
        let bad = icon_body(&png_header(0, 10), "image/png");
        assert!(decode_icon(&bad, IconLimits::default()).is_err());
    }

    #[test]
    fn svg_is_refused_explicitly() {
        let svg = icon_body(
            br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
            "image/svg+xml",
        );
        let err = decode_icon(&svg, IconLimits::default()).unwrap_err();
        assert!(
            err.to_string().contains("SVG"),
            "the refusal should say why: {err}"
        );
    }

    #[test]
    fn oversized_payloads_are_refused_before_decoding() {
        let limits = IconLimits {
            max_bytes: 1024,
            ..IconLimits::default()
        };
        let big = icon_body(&vec![0x89; 8192], "image/png");
        let err = decode_icon(&big, limits).unwrap_err();
        // BEFORE, not merely refused. `is_err()` alone cannot tell the two
        // gates apart -- the post-decode check produces the same Err for the
        // same input -- so deleting the pre-check left every icon test green.
        assert!(
            err.to_string().contains("(declared)"),
            "the pre-decode gate must be the one that refused: {err}"
        );
    }

    #[test]
    fn a_payload_that_only_the_pre_check_can_catch_is_refused() {
        // Valid base64 far past the cap. Reaching the post-decode check means
        // allocating the whole decoded body first, which is exactly what the
        // pre-check exists to avoid on a phone.
        let limits = IconLimits {
            max_bytes: 4096,
            ..IconLimits::default()
        };
        let huge = icon_body(&vec![0x89; 2 * 1024 * 1024], "image/png");
        let err = decode_icon(&huge, limits).unwrap_err();
        assert!(
            err.to_string().contains("(declared)"),
            "a 2 MB payload must be refused on its length, before decoding: {err}"
        );
    }

    #[test]
    fn malformed_base64_is_an_item_error_not_a_panic() {
        let bad = wire::Icon {
            id: 3,
            mime_type: "image/png".into(),
            data: "image/png;base64,!!!not base64!!!".into(),
        };
        assert!(decode_icon(&bad, IconLimits::default())
            .unwrap_err()
            .is_item_local());
    }

    #[test]
    fn an_empty_payload_is_refused() {
        let empty = wire::Icon {
            id: 4,
            mime_type: String::new(),
            data: String::new(),
        };
        assert!(decode_icon(&empty, IconLimits::default()).is_err());
    }

    #[test]
    fn gif_and_jpeg_dimensions_are_read() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&64u16.to_le_bytes());
        gif.extend_from_slice(&48u16.to_le_bytes());
        assert_eq!(read_dimensions(&gif, ImageFormat::Gif), Some((64, 48)));

        // SOI, then a SOF0 segment: FFC0, length 17, precision, height, width.
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        jpg.extend_from_slice(&100u16.to_be_bytes()); // height
        jpg.extend_from_slice(&200u16.to_be_bytes()); // width
        assert_eq!(jpeg_dimensions(&jpg), Some((200, 100)));
    }

    #[test]
    fn a_truncated_header_does_not_panic() {
        // Every accessor is bounds-checked; indexing_slicing is denied crate-wide.
        for len in 0..40 {
            let truncated = png_header(32, 32).get(..len).unwrap_or_default().to_vec();
            let _ = sniff_format(&truncated).map(|f| read_dimensions(&truncated, f));
        }
        for len in 0..12 {
            let jpg = vec![0xFF; len];
            let _ = jpeg_dimensions(&jpg);
        }
    }

    #[test]
    fn a_jpeg_marker_loop_cannot_spin() {
        // What this covers is TERMINATION on hostile input, which is the
        // property that matters: every branch advances `i`, so both of these
        // walk out in O(len) whatever the bytes claim.
        //
        // It does not cover the iteration cap, and cannot: hitting the cap and
        // walking off the end both return `None`, so the two are
        // indistinguishable from outside. Deleting the cap leaves this test
        // green, and that is a fact about the cap being unobservable rather
        // than a hole worth papering over with a timing assertion.
        let pathological = vec![0xFFu8; 100_000];
        assert!(jpeg_dimensions(&pathological).is_none());

        // A long run of well-formed, empty two-byte segments, none of them the
        // SOF marker the walk is looking for.
        let mut many_segments = vec![0xFFu8, 0xD8]; // SOI
        for _ in 0..8192 {
            // APP0 with a length of exactly 2, i.e. no payload.
            many_segments.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x02]);
        }
        assert!(jpeg_dimensions(&many_segments).is_none());

        // A truncated segment header, and a length field that points past the
        // end: neither may loop and neither may index out of bounds.
        assert!(jpeg_dimensions(&[0xFF, 0xD8, 0xFF]).is_none());
        assert!(jpeg_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0x00]).is_none());
    }
}
