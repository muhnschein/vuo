//! Render-block model.
//!
//! §5: article HTML becomes a block list in Rust, not one rich-text blob.
//! Sailfish Qt `Text` supports only HTML subset; `WebView` heavy and awkward in
//! a list.
//!
//! # Why flat
//!
//! [`RenderBlock`] holds no `Vec<RenderBlock>`. Nesting rejected for
//! blockquotes and nested lists on purpose:
//!
//! 1. **Drop safety.** Recursive tree drops recursively. §9.2: deep markup vs
//!    recursive parser = stack overflow; recursive *destructor* same, on
//!    cleanup path, hardest to attribute. Flat `Vec` can't overflow on drop.
//! 2. **UI wants it.** `SilicaListView` wants flat indexable model; tree would
//!    need flattening in QML, logic scope keeps out of QML.
//!
//! Would-be nesting carried as *scalars*: [`RenderBlock::quote_depth`],
//! [`BlockKind::ListItem::indent`].
//!
//! # Why spans, not markup
//!
//! §9.3 forbids interpolating foreign strings into rich text. Transform never
//! emits markup string UI trusts; it emits [`Span`]s. One function renders them
//! to Qt `StyledText` ([`Span::render_styled_text`]): escapes all foreign text,
//! emits only fixed closed tag set. Safety = property of that one tested,
//! fuzzed function, not convention at every call site.

use crate::content::url::MediaUrl;

/// Inline styling. Bitfield, not tree: `Span` never nests.
///
/// Independent flags, any combination valid: `struct_excessive_bools` doesn't
/// apply.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpanStyle {
    /// Bold: `<b>`, `<strong>`.
    pub bold: bool,
    /// Italic: `<i>`, `<em>` and similar.
    pub italic: bool,
    /// Monospace: `<code>`, `<kbd>` and similar. No tag in `StyledText`; UI
    /// picks font.
    pub code: bool,
    /// Struck through: `<s>`, `<del>`, `<strike>`.
    pub strike: bool,
    /// `<sup>`; the UI may ignore it.
    pub superscript: bool,
    /// `<sub>`; the UI may ignore it.
    pub subscript: bool,
}

impl SpanStyle {}

/// A run of text with uniform styling, optionally hyperlinked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    /// Text. Foreign, unescaped: render via [`Span::render_styled_text`] or as
    /// plain text, never markup.
    pub text: String,
    /// How the text is styled.
    pub style: SpanStyle,
    /// `None` for non-link text. Always `http`/`https`: other schemes dropped
    /// while parsing (§9.2); `javascript:` can't reach this field.
    pub link: Option<MediaUrl>,
}

impl Span {
    /// An unstyled, unlinked span.
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            style: SpanStyle::default(),
            link: None,
        }
    }

    /// Render span sequence as Qt `StyledText`.
    ///
    /// *Only* place Vuo makes markup for rich-text context; why §9.3 holds. All
    /// foreign text escaped; tag set closed (`b`, `i`, `s`, `sup`, `sub`, `a
    /// href`); `href` is [`MediaUrl`], `http`/`https` by construction.
    ///
    /// UI must set `textFormat: Text.StyledText` explicitly on whatever renders
    /// it, never default (§9.3).
    #[must_use]
    pub fn render_styled_text(spans: &[Span]) -> String {
        let mut out = String::new();
        for span in spans {
            // Order only for readable output; tags balanced since closed in
            // reverse.
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
                // Qt `StyledText` has no `<code>`; delegate picks font, so
                // `code` emits no tag, exposed to QML via style flags.
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

    /// Concatenated text of span sequence, no markup.
    ///
    /// For `Text.PlainText` rendering and accessibility.
    #[must_use]
    pub fn render_plain_text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Escape text for Qt `StyledText` (HTML subset).
///
/// `&` safe since handled in same pass; quotes escaped so safe inside attribute
/// value (as `href` above uses it).
pub(crate) fn escape_into(raw: &str, out: &mut String) {
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
    /// Third-party, un-proxied. Fetch leaks device IP: user decides (per
    /// origin) before any request.
    NeedsConsent,
}

/// A table cell: a span run plus whether it came from `<th>`.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TableCell {
    /// The cell's contents.
    pub spans: Vec<Span>,
    /// `true` for a `<th>` cell.
    pub header: bool,
}

/// Closed set of things Vuo draws.
///
/// This enum *is* the allowlist (§9.2, §3). Element with no variant is dropped
/// or flattened to text: no "unknown" variant, no passthrough. New rendering
/// capability = new variant, deliberately.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockKind {
    /// A heading, `<h1>` to `<h6>`.
    Heading {
        /// The heading level, 1 to 6.
        level: u8,
        /// The heading text.
        spans: Vec<Span>,
    },
    /// Paragraph. Unstyled block containers (`<div>`, `<figure>`) become
    /// paragraphs too.
    Paragraph {
        /// The paragraph text.
        spans: Vec<Span>,
    },
    /// One list item, or continuation text inside one.
    ListItem {
        /// `true` inside an `<ol>`.
        ordered: bool,
        /// Marker number on first block of ordered item; `None` for bullets and
        /// continuation text.
        number: Option<u32>,
        /// Nesting depth, already clamped to the parser's list-depth cap.
        indent: u8,
        /// The item text.
        spans: Vec<Span>,
    },
    /// `<pre>` / `<pre><code>`. Verbatim, *not* span-formatted: inline markup
    /// inside flattened away.
    Code {
        /// Language hint from `<pre>`'s `class` (`language-rust`, `lang-`,
        /// `highlight-`, `brush:`), if any.
        language: Option<String>,
        /// The code, verbatim, with leading and trailing newlines trimmed.
        text: String,
    },
    /// `<img>` whose `src` resolved to `http`/`https` URL media policy kept.
    Image {
        /// Where to load the image from, as the media policy decided.
        src: MediaUrl,
        /// The `alt` text, empty when there was none. Foreign text.
        alt: String,
        /// The `title` attribute, when non-empty. Foreign text.
        title: Option<String>,
        /// Whether UI may load image immediately.
        ///
        /// Un-proxied third-party media is common on stock Miniflux
        /// (`MEDIA_PROXY_MODE` defaults to `http-only`): kept as placeholders,
        /// nothing fetched until user agrees.
        fetch: MediaFetch,
        /// `width`/`height` attributes from `<img>`, when usable.
        ///
        /// So UI reserves room before pixels arrive; otherwise rows re-lay-out
        /// under reader's thumb as each image lands. Advisory, untrusted: ratio
        /// hint only, never fetch or decode bound.
        intrinsic: Option<(u32, u32)>,
        /// Link the image sits inside, when wrapped in `<a>`.
        ///
        /// Linked image = author's button: tap means "go there", not "zoom". UI
        /// picks between the two. Always `http`/`https`, like [`Span::link`].
        link: Option<MediaUrl>,
    },
    /// Fixed three-level nesting: rows of cells of spans. Not self-referential,
    /// so drop-recursion argument above doesn't apply.
    Table {
        /// The table's rows, each a list of cells.
        rows: Vec<Vec<TableCell>>,
    },
    /// A horizontal rule, `<hr>`.
    Rule,
}

/// One entry in the flat render list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RenderBlock {
    /// What to draw.
    pub kind: BlockKind,
    /// `<blockquote>`s enclosing this block, clamped to depth cap. Scalar so
    /// list stays flat (module docs).
    pub quote_depth: u8,
}

impl RenderBlock {
    /// A block outside any blockquote.
    #[must_use]
    pub fn new(kind: BlockKind) -> Self {
        RenderBlock {
            kind,
            quote_depth: 0,
        }
    }

    /// Block inside `quote_depth` nested blockquotes. Not clamped here.
    #[must_use]
    pub fn quoted(kind: BlockKind, quote_depth: u8) -> Self {
        RenderBlock { kind, quote_depth }
    }

    /// Block text without markup, for search indexing and previews.
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

/// Transform output: flat block list plus what it had to throw away.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Document {
    /// The blocks, in document order.
    pub blocks: Vec<RenderBlock>,
    /// Set when a cap was hit, document incomplete. UI should say so: silent
    /// truncation reads as whole article.
    pub truncated: Option<Truncation>,
}

/// The cap that cut a [`Document`] short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Truncation {
    /// Input exceeded the byte cap and was cut before parsing.
    InputTooLarge,
    /// The block count cap was reached.
    TooManyBlocks,
    /// The cumulative text cap was reached.
    TooMuchText,
    /// Markup nested past structural depth cap; some structure (maybe content
    /// inside) dropped.
    TooDeep,
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
            style: SpanStyle {
                bold: true,
                italic: true,
                ..Default::default()
            },
            link: None,
        }];
        assert_eq!(Span::render_styled_text(&spans), "<b><i>hi</i></b>");
    }

    #[test]
    fn plain_text_renderer_emits_no_markup() {
        let spans = vec![Span {
            text: "a<b>c".into(),
            style: SpanStyle {
                bold: true,
                ..Default::default()
            },
            link: None,
        }];
        assert_eq!(Span::render_plain_text(&spans), "a<b>c");
    }

    #[test]
    fn code_style_emits_no_tag() {
        // Qt `StyledText` has no `<code>`; flag surfaced to QML instead.
        let spans = vec![Span {
            text: "x".into(),
            style: SpanStyle {
                code: true,
                ..Default::default()
            },
            link: None,
        }];
        assert_eq!(Span::render_styled_text(&spans), "x");
    }
}
