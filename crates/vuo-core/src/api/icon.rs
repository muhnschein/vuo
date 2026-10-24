//! Feed icons: decode, validate by content, not claim.
//!
//! §9.3: validate images by content, not claimed type; check magic bytes, cap
//! decoded dimensions, decode failures non-fatal.
//!
//! Icons = most attacker-controlled bytes in app: from feed's website, fetched
//! automatically, decoded by C++ image lib on phone. So paranoid and cheap:
//! header parsed by hand, no decoder, unrecognised refused.
//!
//! Response looks like `data:` URI but isn't: `"image/png;base64,iVBOR..."` (no
//! `data:` prefix).

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
    /// "Cap decoded dimensions" rule, from *header* before any decoder
    /// allocates: only point it prevents allocation. 65535x65535 PNG = few
    /// hundred bytes on wire, 17 GB decoded.
    pub max_pixels_per_side: u32,
}

impl Default for IconLimits {
    fn default() -> Self {
        // Icon drawn at ~32 device px. > 512 per side isn't an icon.
        IconLimits {
            max_bytes: 512 * 1024,
            max_pixels_per_side: 512,
        }
    }
}

/// Decode and validate body of `GET /v1/feeds/{id}/icon`.
///
/// [`Error::Item`] on any problem: bad icon costs feed its icon, nothing else.
///
/// # Errors
///
/// [`Error::Item`] when payload empty, not base64, over `limits.max_bytes`,
/// unrecognised format, SVG, or a dimension zero or over
/// `limits.max_pixels_per_side`.
pub fn decode_icon(w: &wire::Icon, limits: IconLimits) -> Result<Icon> {
    let reject = |why: &str| Error::item("icon", Some(w.id), why.to_owned());

    // Payload = everything after first ";base64,". Some servers may omit mime
    // prefix: fall back to raw string.
    let payload = match w.data.split_once(";base64,") {
        Some((_claimed_mime, b64)) => b64,
        None => w.data.as_str(),
    };
    if payload.is_empty() {
        return Err(reject("empty icon payload"));
    }

    // Cap before decoding: encoded length bounds decoded allocation. Base64: 4
    // chars → 3 bytes, decoded ≤ 3/4 encoded. Truncating division rounds DOWN:
    // only loosens pre-check; exact post-decode check enforces cap. This one
    // avoids allocating for obviously huge payload.
    #[allow(clippy::integer_division)]
    let decoded_upper_bound = (payload.len() / 4).saturating_mul(3);
    if decoded_upper_bound > limits.max_bytes {
        // Wording differs from post-decode check so tests can tell WHICH gate
        // refused.
        return Err(reject("icon exceeds the size cap (declared)"));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| reject(&format!("icon payload is not valid base64: {e}")))?;

    if bytes.len() > limits.max_bytes {
        return Err(reject("icon exceeds the size cap (decoded)"));
    }

    // Claimed `mime_type` ignored: foreign text; believing it is §9.3's bug.
    let format = sniff_format(&bytes).ok_or_else(|| reject("unrecognised image format"))?;

    if format == ImageFormat::Svg {
        // Refused on purpose. SVG = XML: parser, entity expansion, external
        // refs exposed to feed operator's bytes, for 32-px favicon. Not close.
        // SVG-only feeds get default icon.
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
    // Sniff SVG only to refuse it explicitly, not as "unrecognised".
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
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([
        *s.first()?,
        *s.get(1)?,
        *s.get(2)?,
        *s.get(3)?,
    ]))
}

fn be_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
}

/// Read pixel dimensions from header without decoding.
///
/// `None` when header not understood: no dimension info, not error; format
/// whose size isn't cheap to read isn't malicious.
fn read_dimensions(b: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    match format {
        // IHDR always first chunk: 8-byte signature, 4-byte length, 4-byte
        // type, then width, height.
        ImageFormat::Png => Some((be_u32(b, 16)?, be_u32(b, 20)?)),
        ImageFormat::Gif => Some((u32::from(le_u16(b, 6)?), u32::from(le_u16(b, 8)?))),
        ImageFormat::Bmp => {
            // BITMAPINFOHEADER width/height signed 32-bit LE; negative height =
            // top-down.
            let w = i32::from_le_bytes([*b.get(18)?, *b.get(19)?, *b.get(20)?, *b.get(21)?]);
            let h = i32::from_le_bytes([*b.get(22)?, *b.get(23)?, *b.get(24)?, *b.get(25)?]);
            Some((w.unsigned_abs(), h.unsigned_abs()))
        }
        ImageFormat::Ico => {
            // First directory entry's width/height; 0 = 256.
            let w = *b.get(6)?;
            let h = *b.get(7)?;
            Some((
                if w == 0 { 256 } else { u32::from(w) },
                if h == 0 { 256 } else { u32::from(h) },
            ))
        }
        ImageFormat::Jpeg => jpeg_dimensions(b),
        // VP8X has 24-bit canvas size; VP8/VP8L not worth hand-parsing for
        // favicon.
        ImageFormat::WebP => {
            if b.get(12..16) == Some(b"VP8X") {
                // 24 bits plus one: can't overflow u32.
                let w = u32::from_le_bytes([*b.get(24)?, *b.get(25)?, *b.get(26)?, 0]);
                let h = u32::from_le_bytes([*b.get(27)?, *b.get(28)?, *b.get(29)?, 0]);
                Some((w.checked_add(1)?, h.checked_add(1)?))
            } else {
                None
            }
        }
        ImageFormat::Svg => None,
    }
}

/// Walk JPEG segments for Start-Of-Frame marker.
///
/// Bounded by construction: every branch advances `i` ≥ 1, so O(len); `len`
/// already capped by `IconLimits::max_bytes`.
///
/// Iteration cap = cheap second ceiling, not termination argument. Not
/// observable (cap and end both return `None`); test below says so.
fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize; // skip SOI
    let mut guard = 0u32;
    while i.checked_add(3)? < b.len() {
        guard = guard.checked_add(1)?;
        if guard > 4096 {
            return None;
        }
        if *b.get(i)? != 0xFF {
            // Not at marker: resync, don't trust offset.
            i = i.checked_add(1)?;
            continue;
        }
        let marker = *b.get(i.checked_add(1)?)?;
        // Standalone markers have no length.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 || marker == 0xFF {
            i = i.checked_add(2)?;
            continue;
        }
        let len = usize::from(be_u16(b, i.checked_add(2)?)?);
        if len < 2 {
            return None;
        }
        // SOF0..SOF15 except DHT/JPG/DAC at C4, C8, CC.
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let h = be_u16(b, i.checked_add(5)?)?;
            let w = be_u16(b, i.checked_add(7)?)?;
            return Some((u32::from(w), u32::from(h)));
        }
        i = i.checked_add(len)?.checked_add(2)?;
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
        // §9.3: by content, not claim. PNG labelled JPEG is still PNG; script
        // labelled PNG still refused.
        let icon = decode_icon(
            &icon_body(&png_header(16, 16), "image/jpeg"),
            IconLimits::default(),
        )
        .unwrap();
        assert_eq!(icon.format, ImageFormat::Png, "content wins over the label");

        let hostile = icon_body(b"<html><script>alert(1)</script>", "image/png");
        decode_icon(&hostile, IconLimits::default()).unwrap_err();
    }

    #[test]
    fn a_decompression_bomb_is_refused_from_the_header() {
        // Few hundred bytes on wire, ~17 GB decoded. Dimension cap must hold
        // before any decoder sees it.
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
        decode_icon(&bad, IconLimits::default()).unwrap_err();
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
        // BEFORE, not merely refused: `is_err()` can't tell gates apart
        // (post-decode check gives same Err).
        assert!(
            err.to_string().contains("(declared)"),
            "the pre-decode gate must be the one that refused: {err}"
        );
    }

    #[test]
    fn a_payload_that_only_the_pre_check_can_catch_is_refused() {
        // Valid base64 far past cap. Post-decode check would allocate whole
        // body first; pre-check avoids that on phone.
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
        decode_icon(&empty, IconLimits::default()).unwrap_err();
    }

    #[test]
    fn gif_and_jpeg_dimensions_are_read() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&64u16.to_le_bytes());
        gif.extend_from_slice(&48u16.to_le_bytes());
        assert_eq!(read_dimensions(&gif, ImageFormat::Gif), Some((64, 48)));

        // SOI, then SOF0 segment: FFC0, length 17, precision, height, width.
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        jpg.extend_from_slice(&100u16.to_be_bytes()); // height
        jpg.extend_from_slice(&200u16.to_be_bytes()); // width
        assert_eq!(jpeg_dimensions(&jpg), Some((200, 100)));
    }

    #[test]
    fn a_truncated_header_does_not_panic() {
        // Every accessor bounds-checked; `indexing_slicing` denied crate-wide.
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
        // Covers TERMINATION on hostile input: every branch advances `i`, both
        // walk out in O(len).
        //
        // Doesn't cover iteration cap, can't: cap and end both return `None`.
        // Deleting cap leaves this green; cap unobservable, not a hole.
        let pathological = vec![0xFFu8; 100_000];
        assert!(jpeg_dimensions(&pathological).is_none());

        // Long run of well-formed empty two-byte segments, none SOF.
        let mut many_segments = vec![0xFFu8, 0xD8]; // SOI
        for _ in 0..8192 {
            // APP0 with length exactly 2: no payload.
            many_segments.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x02]);
        }
        assert!(jpeg_dimensions(&many_segments).is_none());

        // Truncated segment header, and length past end: neither may loop or
        // index out of bounds.
        assert!(jpeg_dimensions(&[0xFF, 0xD8, 0xFF]).is_none());
        assert!(jpeg_dimensions(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0x00]).is_none());
    }
}
