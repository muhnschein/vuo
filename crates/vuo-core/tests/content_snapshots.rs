//! Snapshot tests for the HTML → block transform.
//!
//! §8.3: *`insta` over a corpus of real article HTML — malformed markup, deep
//! nesting, tables, `<pre>`, figures. Cheap to write, and the failures are
//! legible diffs rather than a rendering bug reported months later.*
//!
//! The corpus in `tests/corpus/` is hand-written to cover the shapes that
//! paragraph names, plus a hostile sample. It is a stand-in for samples pulled
//! from real feeds, not a replacement: when a real article renders wrongly,
//! the fix is to add it here as a new corpus file, watch the snapshot capture
//! the bug, and then fix the transform.
//!
//! Review a diff with `cargo insta review`.

// Test code: see the note in vuo-core's lib.rs. The unwrap/panic denials
// guard foreign-input paths in production, not assertions in tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
use std::path::Path;

use vuo_core::content::{transform, MediaPolicy, TransformContext};

fn context() -> TransformContext {
    let instance = url::Url::parse("https://miniflux.example/").expect("instance url");
    TransformContext {
        // Strict, so the snapshots also record which media is refused.
        media: MediaPolicy::ProxyThroughInstance {
            instance: instance.clone(),
            extra_trusted: Vec::new(),
            fallback: vuo_core::content::UnproxiedMedia::Strict,
        },
        base_url: url::Url::parse("https://blog.example/posts/1/").ok(),
        ..TransformContext::new(instance)
    }
}

/// Render a document as compact text, so a snapshot diff is readable rather
/// than being a wall of derived-Debug output.
fn render(document: &vuo_core::content::Document) -> String {
    use vuo_core::content::BlockKind;

    let mut out = String::new();
    if let Some(t) = document.truncated {
        out.push_str(&format!("[truncated: {t:?}]\n"));
    }
    for block in &document.blocks {
        let indent = "  ".repeat(usize::from(block.quote_depth));
        let line = match &block.kind {
            BlockKind::Heading { level, spans } => format!(
                "h{level}: {}",
                vuo_core::content::Span::render_styled_text(spans)
            ),
            BlockKind::Paragraph { spans } => {
                format!("p: {}", vuo_core::content::Span::render_styled_text(spans))
            }
            BlockKind::ListItem {
                ordered,
                number,
                indent: i,
                spans,
            } => format!(
                "li[{}{}, indent {i}]: {}",
                if *ordered { "ordered" } else { "bullet" },
                number.map(|n| format!(" {n}")).unwrap_or_default(),
                vuo_core::content::Span::render_styled_text(spans)
            ),
            BlockKind::Code { language, text } => format!(
                "code[{}]: {:?}",
                language.clone().unwrap_or_else(|| "none".to_owned()),
                text
            ),
            BlockKind::Image {
                src, alt, fetch, ..
            } => {
                format!("img[{fetch:?}] {src} alt={alt:?}")
            }
            BlockKind::Table { rows } => {
                // Every cell, not just the row count. `format!("table: {} rows")`
                // put each cell's text, styling, links and header flag OUTSIDE
                // the snapshot entirely: tables.html, whose whole point is
                // <th>Implementation</th> and "1.20 s", recorded as the single
                // line "table: 3 rows", so truncating every cell to its first
                // character was a change the corpus could not see.
                //
                // What it records is what the transform DOES, warts included:
                // malformed.snap shows "A cell with no rowTrailing text..."
                // running together, because an unclosed <table> leaves the
                // following paragraph's text in the same span run as the
                // cell's. That is worth seeing rather than hiding; it is
                // cosmetic, on malformed input, and fixing it belongs with the
                // span-merge logic rather than here.
                let mut s = format!("table: {} rows", rows.len());
                for row in rows {
                    let cells: Vec<String> = row
                        .iter()
                        .map(|c| {
                            format!(
                                "{}{}",
                                if c.header { "th:" } else { "td:" },
                                vuo_core::content::Span::render_styled_text(&c.spans)
                            )
                        })
                        .collect();
                    s.push_str(&format!("\n{indent}  | {}", cells.join(" | ")));
                }
                s
            }
            BlockKind::Rule => "rule".to_owned(),
            other => format!("unhandled: {other:?}"),
        };
        out.push_str(&indent);
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[test]
fn corpus_renders_stably() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("corpus directory")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("html"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "the corpus is empty; the test proves nothing"
    );

    let ctx = context();
    for file in files {
        let html = std::fs::read_to_string(&file).expect("corpus file");
        let document = transform(&html, &ctx);
        let name = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unnamed")
            .to_owned();
        insta::assert_snapshot!(name, render(&document));
    }
}

/// The hostile sample gets assertions as well as a snapshot.
///
/// A snapshot records what the transform *does*; these record what it must
/// *never* do. A snapshot alone would happily bless a regression the moment
/// someone ran `cargo insta accept` without reading the diff.
#[test]
fn the_hostile_corpus_sample_leaks_nothing() {
    let html = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/hostile.html"),
    )
    .expect("hostile.html");

    let document = transform(&html, &context());
    let rendered = render(&document);

    for forbidden in [
        "evil.example",    // script/iframe targets
        "tracker.example", // the beacon in <style> and the tracking pixel
        "javascript:",
        "data:image",
        "document.cookie",
        "onclick",
        "steal()",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "{forbidden:?} survived the transform:\n{rendered}"
        );
    }

    // The text that was only ever text must still be readable.
    assert!(
        rendered.contains("this one is only text"),
        "escaped text should still render as text:\n{rendered}"
    );
    assert!(rendered.contains("Ordinary opening line"));
}
