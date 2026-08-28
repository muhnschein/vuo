//! The render-block model.
//!
//! §5: *article HTML is transformed into a block list in Rust, not rendered as
//! one rich-text blob.* Sailfish's Qt vintage supports only a subset of HTML in
//! `Text`, and a `WebView` is heavy and awkward inside a list.
//!
//! # Why the list is flat
//!
//! [`RenderBlock`] contains no `Vec<RenderBlock>`. Nesting would be the
//! obvious encoding for blockquotes and nested lists, and it is rejected on
//! purpose for two reasons:
//!
//! 1. **Drop safety.** A recursively nested tree is dropped recursively. §9.2
//!    warns that deeply nested markup against a recursive parser is a stack
//!    overflow; a recursive *destructor* has exactly the same failure mode, and
//!    it fires on the cleanup path where it is hardest to attribute. A flat
//!    `Vec` cannot overflow on drop however deep the source markup was.
//! 2. **It is what the UI wants.** The consumer is a Silica `SilicaListView`,
//!    which wants a flat, indexable model. A tree would have to be flattened
//!    in QML, which is precisely the logic the scope says to keep out of QML.
//!
//! Structure that would have been nesting is carried as *scalars* instead:
//! [`RenderBlock::quote_depth`] and [`BlockKind::ListItem::indent`].
//!
//! # Why inline spans are structured rather than markup
//!
//! §9.3 forbids interpolating foreign strings into a rich-text context. The
//! transform therefore never emits markup as a string that the UI then trusts.
//! It produces [`Span`]s, and the single function that renders them into Qt
//! `StyledText` ([`Span::render_styled_text`]) escapes every character of
//! foreign text and emits only a fixed, closed tag set. Safety is a property
//! of that one tested, fuzzed function rather than a convention every call
//! site has to remember.

use crate::content::url::MediaUrl;

/// Inline styling. A bitfield rather than a tree, so a `Span` never nests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    /// `<sup>`/`<sub>` collapse to this; the UI may ignore it.
    pub superscript: bool,
    pub subscript: bool,
}

impl SpanStyle {
    #[must_use]
    pub fn is_plain(&self) -> bool {
        *self == SpanStyle::default()
    }
}

/// A run of text with uniform styling, optionally hyperlinked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
    /// `None` for non-link text. Always `http`/`https`: other schemes are
    /// dropped during parsing (§9.2), so a `javascript:` URL can never reach
    /// this field.
    pub link: Option<MediaUrl>,
}

impl Span {
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Span { text: text.into(), style: SpanStyle::default(), link: None }
    }

    /// Render a span sequence as Qt `StyledText`.
    ///
    /// This is the *only* place in Vuo that produces markup for a rich-text
    /// context, and it is the reason §9.3 holds. Every character of foreign
    /// text is escaped; the emitted tag set is closed and fixed
    /// (`b`, `i`, `s`, `sup`, `sub`, `a href`); and the `href` is a
    /// [`MediaUrl`], which by construction is `http` or `https`.
    ///
    /// The UI must set `textFormat: Text.StyledText` explicitly on whatever
    /// renders the result -- never leave it at the default (§9.3).
    #[must_use]
    pub fn render_styled_text(spans: &[Span]) -> String {
        let mut out = String::new();
        for span in spans {
            // Ordering matters only for readability of the output; tags are
            // balanced by construction because we close in reverse.
            let mut open: Vec<&str> = Vec::new();
            if let Some(link) = &span.link {
                out.push_str("<a href=\"");
                escape_into(link.as_str(), &mut out);
                out.push_str("\">");
                open.push("a");
            }
            for (on, tag) in [
                (span.style.bold, "b"),
                (span.style.italic, "i"),
                (span.style.strike, "s"),
                (span.style.superscript, "sup"),
                (span.style.subscript, "sub"),
                // Qt's StyledText has no <code>; monospace is conveyed by the
                // delegate choosing a font, so `code` intentionally emits no
                // tag here and is exposed to QML through the style flags.
            ] {
                if on {
                    out.push('<');
                    out.push_str(tag);
                    out.push('>');
                    open.push(tag);
                }
            }

            escape_into(&span.text, &mut out);

            for tag in open.iter().rev() {
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
        out
    }

    /// The concatenated text of a span sequence, with no markup at all.
    ///
    /// Used for anything rendered as `Text.PlainText` and for accessibility.
    #[must_use]
    pub fn render_plain_text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Escape text for inclusion in Qt `StyledText` (an HTML subset).
///
/// `&` must be escaped first by virtue of being handled in the same pass;
/// quotes are escaped so the function is equally safe inside an attribute
/// value, which is how the `href` above uses it.
fn escape_into(raw: &str, out: &mut String) {
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

/// Whether a media reference may be loaded without asking the user first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaFetch {
    /// Server-proxied or same-origin: no third party learns anything.
    Allowed,
    /// Third-party and un-proxied. Fetching leaks the device IP, so the user
    /// decides (per origin) before anything is requested.
    NeedsConsent,
}

/// A table cell: a span run plus whether it came from `<th>`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TableCell {
    pub spans: Vec<Span>,
    pub header: bool,
}

/// The closed set of things Vuo knows how to draw.
///
/// This enum *is* the allowlist (§9.2, §3). An element that does not map to a
/// variant here is dropped or flattened to text -- there is no "unknown block"
/// variant and no passthrough, so unrecognised markup cannot survive the
/// transform by default. Adding a rendering capability means adding a variant
/// here deliberately.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockKind {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph { spans: Vec<Span> },
    ListItem {
        ordered: bool,
        /// The rendered marker number for ordered lists, `None` for bullets.
        number: Option<u32>,
        /// Nesting depth, already clamped to the parser's list-depth cap.
        indent: u8,
        spans: Vec<Span>,
    },
    /// `<pre>` / `<pre><code>`. Text is verbatim and is *not* span-formatted:
    /// inline markup inside a code block is flattened away rather than honoured.
    Code { language: Option<String>, text: String },
    Image {
        src: MediaUrl,
        alt: String,
        title: Option<String>,
        /// Whether the UI may load this image immediately.
        ///
        /// Un-proxied third-party media is the common case on a stock Miniflux
        /// (`MEDIA_PROXY_MODE` defaults to `http-only`), so the transform keeps
        /// such images in the document as placeholders rather than dropping
        /// them. The UI renders a tap-to-load affordance naming the host, and
        /// nothing is fetched until the user agrees.
        fetch: MediaFetch,
    },
    /// Fixed three-level nesting: rows of cells of spans. Not self-referential,
    /// so the drop-recursion argument above does not apply.
    Table { rows: Vec<Vec<TableCell>> },
    Rule,
}

/// One entry in the flat render list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RenderBlock {
    pub kind: BlockKind,
    /// How many `<blockquote>`s enclosed this block, clamped to the depth cap.
    /// Carried as a scalar so the block list stays flat -- see the module docs.
    pub quote_depth: u8,
}

impl RenderBlock {
    #[must_use]
    pub fn new(kind: BlockKind) -> Self {
        RenderBlock { kind, quote_depth: 0 }
    }

    #[must_use]
    pub fn quoted(kind: BlockKind, quote_depth: u8) -> Self {
        RenderBlock { kind, quote_depth }
    }

    /// The block's text with no markup, for search indexing and previews.
    #[must_use]
    pub fn plain_text(&self) -> String {
        match &self.kind {
            BlockKind::Heading { spans, .. }
            | BlockKind::Paragraph { spans }
            | BlockKind::ListItem { spans, .. } => Span::render_plain_text(spans),
            BlockKind::Code { text, .. } => text.clone(),
            BlockKind::Image { alt, .. } => alt.clone(),
            BlockKind::Table { rows } => {
                let mut out = String::new();
                for row in rows {
                    for (i, cell) in row.iter().enumerate() {
                        if i > 0 {
                            out.push('\t');
                        }
                        out.push_str(&Span::render_plain_text(&cell.spans));
                    }
                    out.push('\n');
                }
                out
            }
            BlockKind::Rule => String::new(),
        }
    }
}

/// The transform's output: a flat block list plus what it had to throw away.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Document {
    pub blocks: Vec<RenderBlock>,
    /// Set when a cap was hit and the document is therefore incomplete. The UI
    /// should say so rather than silently showing a truncated article -- a
    /// silent truncation reads as "this is the whole article" when it is not.
    pub truncated: Option<Truncation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    /// Input exceeded the byte cap and was cut before parsing.
    InputTooLarge,
    /// The block count cap was reached.
    TooManyBlocks,
    /// The cumulative text cap was reached.
    TooMuchText,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_text_escapes_foreign_text() {
        let spans = vec![Span::plain("<script>alert('xss')</script> & \"quoted\"")];
        let out = Span::render_styled_text(&spans);
        assert!(!out.contains("<script>"), "raw tag survived: {out}");
        assert_eq!(
            out,
            "&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt; &amp; &quot;quoted&quot;"
        );
    }

    #[test]
    fn styled_text_balances_and_nests_tags() {
        let spans = vec![Span {
            text: "hi".into(),
            style: SpanStyle { bold: true, italic: true, ..Default::default() },
            link: None,
        }];
        assert_eq!(Span::render_styled_text(&spans), "<b><i>hi</i></b>");
    }

    #[test]
    fn plain_text_renderer_emits_no_markup() {
        let spans = vec![Span {
            text: "a<b>c".into(),
            style: SpanStyle { bold: true, ..Default::default() },
            link: None,
        }];
        assert_eq!(Span::render_plain_text(&spans), "a<b>c");
    }

    #[test]
    fn code_style_emits_no_tag() {
        // Qt StyledText has no <code>; the flag is surfaced to QML instead.
        let spans = vec![Span {
            text: "x".into(),
            style: SpanStyle { code: true, ..Default::default() },
            link: None,
        }];
        assert_eq!(Span::render_styled_text(&spans), "x");
    }
}
