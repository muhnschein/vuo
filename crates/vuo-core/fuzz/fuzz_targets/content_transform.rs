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
    // `..Default::default()` on purpose: an exhaustive literal here means that
    // adding a cap to `Limits` breaks the fuzz build, and the fuzz crate is a
    // separate workspace that `make check` does not compile — so the breakage
    // only surfaces in CI. Spreading the default keeps new caps working
    // immediately at their production value.
    let limits = Limits {
        // Smaller than production so the fuzzer explores the truncation paths
        // often rather than once in a thousand runs.
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

    // Rendering must not panic, and every tag it emits must come from the
    // closed set the renderer is allowed to produce.
    //
    // NOTE: do NOT test this by searching for scary substrings like "onerror="
    // or "javascript:". All foreign text is ESCAPED, so a document whose text
    // content happens to be the literal string `onerror=` renders it as exactly
    // that -- correctly and harmlessly. An earlier version of this target
    // asserted on substrings and the fuzzer duly "found" that input within
    // seconds. The real invariant is structural: after escaping, every `<` in
    // the output must begin one of our own tags.
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

/// Every `<...>` in rendered StyledText must be a tag this renderer emits.
///
/// Anything else means foreign markup survived escaping.
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
            // The only tag with an attribute. The href is a MediaUrl, so it is
            // http(s) by construction; assert that here too since this is the
            // one place a URL reaches a markup context.
            || (tag.starts_with("a href=\"")
                && tag.ends_with('"')
                && {
                    let href = tag
                        .strip_prefix("a href=\"")
                        .and_then(|t| t.strip_suffix('"'))
                        .unwrap_or_default();
                    // Escaped, so compare against the escaped forms.
                    (href.starts_with("http://") || href.starts_with("https://"))
                        && !href.contains('<')
                        && !href.contains('>')
                });

        assert!(ok, "rendered output contains a tag the renderer must never emit: <{tag}> in {styled:?}");
        i += 1 + end + 1;
    }
}
