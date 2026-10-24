//! Fuzz HTML → render-block transform.
//!
//! §8.3: fuzz both parsers, seeded from snapshot corpus. They eat
//! attacker-influenced bytes (§9) and are pure functions: cheap to fuzz.
//!
//! Property (§9.5): malformed input = handled error, not crash. `transform` is
//! infallible, so any panic, abort, stack overflow is a bug.
//!
//! Assertions after call find caps that silently fail to hold, not just
//! crashes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vuo_core::content::{transform, Limits, TransformContext};

fuzz_target!(|data: &[u8]| {
    let Ok(html) = std::str::from_utf8(data) else { return };

    let instance = match url::Url::parse("https://miniflux.example/") {
        Ok(u) => u,
        Err(_) => return,
    };
    // `..Default::default()` on purpose: exhaustive literal breaks fuzz build
    // on each new `Limits` cap. New caps run at production value.
    let limits = Limits {
        // Smaller than production: fuzzer hits truncation paths often.
        max_input_bytes: 64 * 1024,
        max_depth: 64,
        max_blocks: 256,
        max_text_bytes: 64 * 1024,
        max_quote_depth: 4,
        max_list_indent: 4,
        ..Limits::default()
    };
    let ctx = TransformContext {
        base_url: url::Url::parse("https://blog.example/post/").ok(),
        limits,
        ..TransformContext::new(instance)
    };

    let document = transform(html, &ctx);

    // Every cap must hold, whatever input.
    assert!(document.blocks.len() <= limits.max_blocks, "block cap breached");
    for block in &document.blocks {
        assert!(block.quote_depth <= limits.max_quote_depth, "quote depth cap breached");
        if let vuo_core::content::BlockKind::ListItem { indent, .. } = &block.kind {
            assert!(*indent <= limits.max_list_indent, "list indent cap breached");
        }
        // §9.2: only http(s) may survive into rendered link or image.
        if let vuo_core::content::BlockKind::Image { src, .. } = &block.kind {
            let scheme = src.as_url().scheme();
            assert!(scheme == "http" || scheme == "https", "a {scheme}: image survived");
        }
    }

    // Rendering must not panic; every emitted tag from renderer's closed set.
    //
    // Don't test for scary substrings ("onerror=", "javascript:"): foreign text
    // is ESCAPED, so literal `onerror=` text renders harmlessly. Invariant is
    // structural: after escaping, every `<` starts one of our tags.
    for block in &document.blocks {
        if let vuo_core::content::BlockKind::Paragraph { spans }
        | vuo_core::content::BlockKind::Heading { spans, .. }
        | vuo_core::content::BlockKind::ListItem { spans, .. } = &block.kind
        {
            let styled = vuo_core::content::Span::render_styled_text(spans);
            assert_no_foreign_tags(&styled);
            let _ = vuo_core::content::Span::render_plain_text(spans);
        }
    }
});

/// Every `<...>` in rendered `StyledText` must be a tag this renderer emits.
///
/// Anything else = foreign markup survived escaping.
fn assert_no_foreign_tags(styled: &str) {
    const ALLOWED: &[&str] = &[
        "b", "/b", "i", "/i", "s", "/s", "sup", "/sup", "sub", "/sub", "/a",
    ];

    let bytes = styled.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes.get(i) != Some(&b'<') {
            i += 1;
            continue;
        }
        let rest = styled.get(i + 1..).unwrap_or_default();
        let Some(end) = rest.find('>') else {
            panic!("unterminated tag in rendered output: {styled:?}");
        };
        let tag = rest.get(..end).unwrap_or_default();

        let ok = ALLOWED.contains(&tag)
            // Only tag with attribute. href is `MediaUrl` (http(s) by
            // construction); assert anyway: only place URL reaches markup
            // context.
            || (tag.starts_with("a href=\"")
                && tag.ends_with('"')
                && {
                    let href = tag
                        .strip_prefix("a href=\"")
                        .and_then(|t| t.strip_suffix('"'))
                        .unwrap_or_default();
                    // Escaped: compare against escaped forms.
                    (href.starts_with("http://") || href.starts_with("https://"))
                        && !href.contains('<')
                        && !href.contains('>')
                });

        assert!(ok, "rendered output contains a tag the renderer must never emit: <{tag}> in {styled:?}");
        i += 1 + end + 1;
    }
}
