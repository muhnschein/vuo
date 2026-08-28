//! Fuzzing the HTML → render-block transform.
//!
//! §8.3: *fuzzing on the two parsers. `cargo-fuzz` targets over the content
//! transform and the JSON response deserialiser, seeded from the snapshot
//! corpus. These are the code paths that eat attacker-influenced bytes (§9),
//! and they are pure functions, which makes them unusually cheap to fuzz.*
//!
//! The property under test is the one §9.5 states behaviourally: *a malformed
//! response is a handled error, not a crash.* `transform` is infallible, so
//! any panic, abort or stack overflow is a bug — there is no "expected
//! failure" case to filter out.
//!
//! The assertions after the call are not decoration. Without them the fuzzer
//! only finds crashes; with them it can also find a cap that silently fails to
//! hold, which is the quieter and more dangerous bug.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vuo_core::content::{transform, Limits, TransformContext};

fuzz_target!(|data: &[u8]| {
    let Ok(html) = std::str::from_utf8(data) else { return };

    let instance = match url::Url::parse("https://miniflux.example/") {
        Ok(u) => u,
        Err(_) => return,
    };
    let limits = Limits {
        // Smaller than production so the fuzzer explores the truncation paths
        // often rather than once in a thousand runs.
        max_input_bytes: 64 * 1024,
        max_depth: 64,
        max_blocks: 256,
        max_text_bytes: 64 * 1024,
        max_quote_depth: 4,
        max_list_indent: 4,
    };
    let ctx = TransformContext {
        base_url: url::Url::parse("https://blog.example/post/").ok(),
        limits,
        ..TransformContext::new(instance)
    };

    let document = transform(html, &ctx);

    // Every cap must actually hold, whatever the input.
    assert!(document.blocks.len() <= limits.max_blocks, "block cap breached");
    for block in &document.blocks {
        assert!(block.quote_depth <= limits.max_quote_depth, "quote depth cap breached");
        if let vuo_core::content::BlockKind::ListItem { indent, .. } = &block.kind {
            assert!(*indent <= limits.max_list_indent, "list indent cap breached");
        }
        // §9.2: only http(s) may survive into a rendered link or image.
        if let vuo_core::content::BlockKind::Image { src, .. } = &block.kind {
            let scheme = src.as_url().scheme();
            assert!(scheme == "http" || scheme == "https", "a {scheme}: image survived");
        }
    }

    // Rendering must not panic either, and must never emit an unescaped tag
    // that did not come from our own closed set.
    for block in &document.blocks {
        if let vuo_core::content::BlockKind::Paragraph { spans }
        | vuo_core::content::BlockKind::Heading { spans, .. } = &block.kind
        {
            let styled = vuo_core::content::Span::render_styled_text(spans);
            for forbidden in ["<script", "<img", "<iframe", "onerror=", "javascript:"] {
                assert!(
                    !styled.contains(forbidden),
                    "rendered markup contains {forbidden}: {styled}"
                );
            }
            let _ = vuo_core::content::Span::render_plain_text(spans);
        }
    }
});
