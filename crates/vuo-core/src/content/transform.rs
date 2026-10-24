//! HTML → render-block transform.
//!
//! # Tokenizer, not DOM
//!
//! Drives `html5ever` **tokenizer** directly, never builds a tree. Safety
//! decision, not performance.
//!
//! `RcDom` is tree of `Rc`s; dropping is *recursive*: 10 000 nested `<div>`s
//! overflow stack in destructor, on cleanup path, hardest to attribute (§9.2).
//! Flat token stream → flat [`Document`]: nothing recursive to build *or* drop.
//!
//! # Allowlist by construction
//!
//! §9.2: allowlist, never blocklist. Recognised elements map to [`BlockKind`]
//! variants or inline styles; rest falls to `Disposition::Flatten` (drop tag,
//! keep text). No passthrough: unknown element renders as its text only.
//!
//! Small set `Skip`ped subtree-and-all (`<script>`, `<style>`, `<iframe>`,
//! ...): their *text* isn't content.
//!
//! # Every loop bounded
//!
//! Input truncated before parsing, not after (§9.2). Depth, block count,
//! cumulative text, list indent, quote depth all capped; hitting one records a
//! [`Truncation`] so UI says article incomplete.

use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    TokenizerResult,
};
use tendril::StrTendril;
use url::Url;

use crate::content::block::{
    BlockKind, Document, MediaFetch, RenderBlock, Span, SpanStyle, TableCell, Truncation,
};
use crate::content::url::{MediaDecision, MediaPolicy, MediaUrl};

/// Resource caps for transform. Each exists because input is written by
/// strangers (§9).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Applied to input string *before* tokenizing.
    pub max_input_bytes: usize,
    /// Max element nesting. Beyond it, start tags open no new structure;
    /// content flattens into enclosing block.
    pub max_depth: usize,
    /// Maximum blocks in the output document.
    pub max_blocks: usize,
    /// Maximum cumulative text bytes across all blocks.
    pub max_text_bytes: usize,
    /// Maximum `<blockquote>` nesting reflected in `quote_depth`.
    pub max_quote_depth: u8,
    /// Maximum list nesting reflected in `indent`.
    pub max_list_indent: u8,
    /// Max rows in one table.
    ///
    /// Table = SINGLE block, so `max_blocks` doesn't bound it: half a million
    /// empty `<td>`s built 80 MB reporting one block.
    pub max_table_rows: usize,
    /// Maximum cells in one table row.
    pub max_table_cells_per_row: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            // Multi-MB entry = frozen UI on phone (§9.2). 2 MB far past any
            // honest article.
            max_input_bytes: 2 * 1024 * 1024,
            max_depth: 128,
            max_blocks: 4096,
            max_text_bytes: 1024 * 1024,
            max_quote_depth: 8,
            max_list_indent: 8,
            max_table_rows: 512,
            max_table_cells_per_row: 64,
        }
    }
}

/// Everything the transform needs besides the markup itself.
#[derive(Debug, Clone)]
pub struct TransformContext {
    /// The article's own URL, used to resolve relative `src`/`href`.
    pub base_url: Option<Url>,
    /// What to do about remote media (§9.3).
    pub media: MediaPolicy,
    /// The resource caps the transform enforces.
    pub limits: Limits,
}

impl TransformContext {
    /// Privacy-preserving default for an instance.
    #[must_use]
    pub fn new(instance: Url) -> Self {
        TransformContext {
            base_url: None,
            media: MediaPolicy::default_for(instance),
            limits: Limits::default(),
        }
    }
}

/// Transform article HTML into flat block list.
///
/// **Infallible by design.** §9.5: malformed response = handled error, not
/// crash; honest handling of unparseable markup = empty or partial document,
/// not an `Err` someone unwraps. Unparsed parts absent; cuts recorded in
/// [`Document::truncated`].
#[must_use]
pub fn transform(html: &str, ctx: &TransformContext) -> Document {
    // Cap input BEFORE parsing (§9.2). Slice on char boundary: no split UTF-8.
    let (input, pre_truncated) = if html.len() > ctx.limits.max_input_bytes {
        (
            clamp_to_char_boundary(html, ctx.limits.max_input_bytes),
            Some(Truncation::InputTooLarge),
        )
    } else {
        (html, None)
    };

    drive(input, ctx, pre_truncated).finish()
}

/// Tokenize into [`Builder`] without finishing.
///
/// Separate so a test can inspect builder state: §9.2 depth cap inside SKIPPED
/// subtree has no observable output, so test checks open-element stack depth.
fn drive(input: &str, ctx: &TransformContext, pre_truncated: Option<Truncation>) -> Builder {
    let mut builder = Builder::new(ctx.clone());
    builder.truncated = pre_truncated;

    let mut queue = BufferQueue::default();
    queue.push_back(StrTendril::from(input));

    let mut tok = Tokenizer::new(
        builder,
        TokenizerOpts {
            // Parse errors uninteresting: best-effort rendering; collecting
            // error strings from hostile input = unbounded allocation.
            exact_errors: false,
            ..TokenizerOpts::default()
        },
    );

    // `feed` returns `Script(handle)` only if sink asks for scripts (never);
    // looping to `Done` is exhaustive.
    let fed = tok.feed(&mut queue);
    debug_assert!(
        matches!(fed, TokenizerResult::Done),
        "the sink never asks for script handling"
    );
    tok.end();

    tok.sink
}

/// What the transform does with an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Drop the element and everything inside it.
    Skip,
    /// Drop tag, keep contents. Default for anything unrecognised.
    Flatten,
    /// Recognised structure.
    Block(BlockTag),
    /// Recognised inline styling.
    Inline(InlineTag),
    /// Recognised void element.
    Void(VoidTag),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockTag {
    Paragraph,
    Heading(u8),
    List { ordered: bool },
    ListItem,
    Quote,
    Pre,
    Table,
    TableRow,
    TableCell { header: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineTag {
    Bold,
    Italic,
    Code,
    Strike,
    Superscript,
    Subscript,
    Anchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoidTag {
    Break,
    Rule,
    Image,
}

/// Allowlist. Anything unnamed = [`Disposition::Flatten`].
fn classify(name: &str) -> Disposition {
    match name {
        // -- dropped entirely: text isn't article content ------------------
        "script" | "style" | "noscript" | "iframe" | "object" | "embed" | "applet" | "svg"
        | "math" | "template" | "form" | "button" | "input" | "select" | "option" | "textarea"
        | "canvas" | "map" | "area" | "audio" | "video" | "source" | "track" | "param" | "head"
        | "meta" | "link" | "title" | "base" => Disposition::Skip,

        // -- block structure ------------------------------------------------
        "p" => Disposition::Block(BlockTag::Paragraph),
        "h1" => Disposition::Block(BlockTag::Heading(1)),
        "h2" => Disposition::Block(BlockTag::Heading(2)),
        "h3" => Disposition::Block(BlockTag::Heading(3)),
        "h4" => Disposition::Block(BlockTag::Heading(4)),
        "h5" => Disposition::Block(BlockTag::Heading(5)),
        "h6" => Disposition::Block(BlockTag::Heading(6)),
        "ul" | "menu" => Disposition::Block(BlockTag::List { ordered: false }),
        "ol" => Disposition::Block(BlockTag::List { ordered: true }),
        "li" => Disposition::Block(BlockTag::ListItem),
        "blockquote" => Disposition::Block(BlockTag::Quote),
        "pre" => Disposition::Block(BlockTag::Pre),
        "table" => Disposition::Block(BlockTag::Table),
        "tr" => Disposition::Block(BlockTag::TableRow),
        "td" => Disposition::Block(BlockTag::TableCell { header: false }),
        "th" => Disposition::Block(BlockTag::TableCell { header: true }),

        // Block containers with no styling that still end current paragraph.
        "div" | "section" | "article" | "aside" | "main" | "header" | "footer" | "nav"
        | "figure" | "figcaption" | "dl" | "dt" | "dd" | "address" | "fieldset" | "details"
        | "summary" | "hgroup" | "thead" | "tbody" | "tfoot" | "caption" | "colgroup" | "col" => {
            Disposition::Block(BlockTag::Paragraph)
        }

        // -- inline ---------------------------------------------------------
        "b" | "strong" => Disposition::Inline(InlineTag::Bold),
        "i" | "em" | "cite" | "dfn" | "var" => Disposition::Inline(InlineTag::Italic),
        "code" | "kbd" | "samp" | "tt" => Disposition::Inline(InlineTag::Code),
        "s" | "del" | "strike" => Disposition::Inline(InlineTag::Strike),
        "sup" => Disposition::Inline(InlineTag::Superscript),
        "sub" => Disposition::Inline(InlineTag::Subscript),
        "a" => Disposition::Inline(InlineTag::Anchor),

        // -- void -----------------------------------------------------------
        "br" => Disposition::Void(VoidTag::Break),
        "hr" => Disposition::Void(VoidTag::Rule),
        "img" => Disposition::Void(VoidTag::Image),

        _ => Disposition::Flatten,
    }
}

/// An element on the open-element stack.
#[derive(Debug)]
struct OpenElement {
    name: String,
    disposition: Disposition,
    /// Inline style to restore when this element closes.
    saved_style: Option<SpanStyle>,
    /// Link to restore on close. Two levels on purpose: outer `None` = element
    /// didn't touch link, inner = restore no link.
    #[allow(clippy::option_option)]
    saved_link: Option<Option<MediaUrl>>,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    ordered: bool,
    counter: u32,
}

/// Block kind current spans belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Paragraph,
    Heading(u8),
    ListItem {
        ordered: bool,
        number: Option<u32>,
        indent: u8,
    },
}

struct Builder {
    ctx: TransformContext,
    blocks: Vec<RenderBlock>,
    truncated: Option<Truncation>,

    open: Vec<OpenElement>,
    /// Depth where `Skip` element opened. While `Some`, discard everything
    /// until stack unwinds past it.
    skip_from: Option<usize>,

    spans: Vec<Span>,
    style: SpanStyle,
    link: Option<MediaUrl>,
    pending: Pending,

    quote_depth: u8,
    lists: Vec<ListState>,

    /// `<pre>` nesting; text is taken verbatim while non-zero.
    pre_depth: u32,
    pre_text: String,
    pre_language: Option<String>,

    /// Table accumulation. `None` when not inside a `<table>`.
    table: Option<TableBuild>,
    /// `<table>` nesting. Only outermost close emits block: nested table
    /// flattens into parent's cells; closing outer on inner `</table>` would
    /// truncate it.
    table_depth: u32,

    text_bytes: usize,
    /// Deepest `open` ever got. See [`drive`].
    peak_open: usize,
    /// `text_bytes` as of last emitted block.
    ///
    /// Lets `push_block` tell "this block reached cap" from "already over, adds
    /// nothing": truncate vs erase article.
    text_committed: usize,
}

#[derive(Debug, Default)]
struct TableBuild {
    rows: Vec<Vec<TableCell>>,
    current_row: Option<Vec<TableCell>>,
    current_header: bool,
}

impl Builder {
    fn new(ctx: TransformContext) -> Self {
        Builder {
            ctx,
            blocks: Vec::new(),
            truncated: None,
            open: Vec::new(),
            skip_from: None,
            spans: Vec::new(),
            style: SpanStyle::default(),
            link: None,
            pending: Pending::Paragraph,
            quote_depth: 0,
            lists: Vec::new(),
            pre_depth: 0,
            pre_text: String::new(),
            pre_language: None,
            table: None,
            table_depth: 0,
            text_bytes: 0,
            peak_open: 0,
            text_committed: 0,
        }
    }

    fn finish(mut self) -> Document {
        self.flush();
        self.close_table();
        Document {
            blocks: self.blocks,
            truncated: self.truncated,
        }
    }

    fn at_capacity(&self) -> bool {
        self.blocks.len() >= self.ctx.limits.max_blocks
            || self.text_bytes >= self.ctx.limits.max_text_bytes
    }

    fn record_truncation(&mut self, why: Truncation) {
        // Keep first cause: explains the rest.
        if self.truncated.is_none() {
            self.truncated = Some(why);
        }
    }

    fn push_block(&mut self, kind: BlockKind) {
        // Block-level element inside table (rule, image, code) would go
        // straight to document, BEFORE its table (table emits on close). Fold
        // its text into current cell; drop rest.
        if self.table.is_some() {
            let text = RenderBlock::new(kind).plain_text();
            if !text.trim().is_empty() {
                if let Some(table) = self.table.as_mut() {
                    if let Some(row) = table.current_row.as_mut() {
                        if let Some(cell) = row.last_mut() {
                            cell.spans.push(Span {
                                text,
                                style: SpanStyle::default(),
                                link: None,
                            });
                        }
                    }
                }
            }
            return;
        }
        if self.blocks.len() >= self.ctx.limits.max_blocks {
            self.record_truncation(Truncation::TooManyBlocks);
            return;
        }
        if self.text_bytes >= self.ctx.limits.max_text_bytes {
            self.record_truncation(Truncation::TooMuchText);
            // DROP block only when it adds no text. Block reaching cap still
            // emitted: `push_text` already clamped its text. Else single long
            // `<p>` article renders zero blocks (blank screen).
            if self.text_bytes == self.text_committed {
                return;
            }
        }
        self.text_committed = self.text_bytes;
        self.blocks
            .push(RenderBlock::quoted(kind, self.quote_depth));
    }

    /// Emit accumulated inline text as pending block kind.
    fn flush(&mut self) {
        let spans = std::mem::take(&mut self.spans);
        let spans = trim_spans(spans);
        if spans.is_empty() {
            // Reset pending even when empty: stray `</h2>` can't leak heading
            // level onto next paragraph.
            self.pending = self.default_pending();
            return;
        }

        // Inside table cell: text belongs to cell, not document body.
        if let Some(table) = self.table.as_mut() {
            if let Some(row) = table.current_row.as_mut() {
                if let Some(cell) = row.last_mut() {
                    cell.spans.extend(spans);
                    self.pending = self.default_pending();
                    return;
                }
            }
            // Text in `<table>` outside any cell is stray: drop, don't invent
            // cell.
            self.pending = self.default_pending();
            return;
        }

        let kind = match self.pending {
            Pending::Paragraph => BlockKind::Paragraph { spans },
            Pending::Heading(level) => BlockKind::Heading { level, spans },
            Pending::ListItem {
                ordered,
                number,
                indent,
            } => BlockKind::ListItem {
                ordered,
                number,
                indent,
                spans,
            },
        };
        self.push_block(kind);
        self.pending = self.default_pending();
    }

    /// What fresh block should be, given enclosing structure.
    fn default_pending(&self) -> Pending {
        if self.lists.is_empty() {
            Pending::Paragraph
        } else {
            // Continuation text in list item stays part of item.
            match self.lists.last() {
                Some(list) => Pending::ListItem {
                    ordered: list.ordered,
                    number: None,
                    indent: self.list_indent(),
                },
                None => Pending::Paragraph,
            }
        }
    }

    fn list_indent(&self) -> u8 {
        let depth = self.lists.len().saturating_sub(1);
        u8::try_from(depth)
            .unwrap_or(u8::MAX)
            .min(self.ctx.limits.max_list_indent)
    }

    fn push_text(&mut self, text: &str) {
        if self.skip_from.is_some() {
            return;
        }

        if self.pre_depth > 0 {
            let budget = self
                .ctx
                .limits
                .max_text_bytes
                .saturating_sub(self.text_bytes);
            if budget == 0 {
                self.record_truncation(Truncation::TooMuchText);
                return;
            }
            let taken = clamp_to_char_boundary(text, budget);
            self.pre_text.push_str(taken);
            self.text_bytes = self.text_bytes.saturating_add(taken.len());
            return;
        }

        // Outside `<pre>`: collapse whitespace runs; feed markup full of
        // meaningless newlines and indentation.
        let collapsed = collapse_whitespace(text);
        if collapsed.is_empty() {
            return;
        }
        // Collapse across token boundaries: html5ever splits character data
        // anywhere ("a\n\n  \tb" as several tokens); per-chunk collapse renders
        // "a   b". Drop leading space when text so far ends in one, or no text
        // yet (block mustn't start with space).
        let trailing_space = match self.spans.last() {
            Some(prev) => prev.text.ends_with(' '),
            None => true,
        };
        let collapsed = if trailing_space && collapsed.starts_with(' ') {
            collapsed.trim_start().to_owned()
        } else {
            collapsed
        };
        if collapsed.is_empty() {
            return;
        }

        let budget = self
            .ctx
            .limits
            .max_text_bytes
            .saturating_sub(self.text_bytes);
        if budget == 0 {
            self.record_truncation(Truncation::TooMuchText);
            return;
        }
        let taken = clamp_to_char_boundary(&collapsed, budget).to_owned();
        self.text_bytes = self.text_bytes.saturating_add(taken.len());

        // Merge into previous span when styling identical: short span list for
        // text split by ignored tags.
        match self.spans.last_mut() {
            Some(prev) if prev.style == self.style && prev.link == self.link => {
                prev.text.push_str(&taken);
            }
            _ => self.spans.push(Span {
                text: taken,
                style: self.style,
                link: self.link.clone(),
            }),
        }
    }

    fn close_table(&mut self) {
        let max_rows = self.ctx.limits.max_table_rows;
        if let Some(mut table) = self.table.take() {
            if let Some(row) = table.current_row.take() {
                if !row.is_empty() && table.rows.len() < max_rows {
                    table.rows.push(row);
                }
            }
            if !table.rows.is_empty() {
                self.push_block(BlockKind::Table { rows: table.rows });
            }
        }
    }

    fn start_tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name = tag.name.as_ref().to_ascii_lowercase();
        let disposition = classify(&name);
        // Element that never has children or end tag.
        let childless = is_void(&name) || tag.self_closing;

        // Already discarding subtree: track stack to know when to stop, nothing
        // else. Depth cap applies here too: else skipped subtree grows stack
        // unbounded and unmatched end tags (stack scan) go quadratic (320 KB
        // took 23 s, well inside 2 MiB cap). §9.2 caps prevent that.
        if self.skip_from.is_some() {
            if !childless && self.open.len() < self.ctx.limits.max_depth {
                self.peak_open = self.peak_open.max(self.open.len().saturating_add(1));
                self.open.push(OpenElement {
                    name,
                    disposition: Disposition::Skip,
                    saved_style: None,
                    saved_link: None,
                });
            }
            return TokenSinkResult::Continue;
        }

        // Skip elements BEFORE structural depth cap; order load-bearing. Cap
        // first: `<script>` past cap returns without skip mode, tokenizer never
        // enters raw-text, script body renders as text (`alert('pwned')`).
        // Allowlist breach (§9.2).
        if matches!(disposition, Disposition::Skip) {
            self.flush();
            // Childless skip element (`<meta>`, `<link>`, `<input>`, `<embed>`,
            // `<source>`, `<track>`, `<param>`, `<area>`, self-closing like
            // `<svg/>`) never gets end tag. Skip mode for one would never
            // clear, discarding rest of article.
            if childless {
                return TokenSinkResult::Continue;
            }
            self.skip_from = Some(self.open.len());
            let raw = raw_text_kind(&name);
            self.peak_open = self.peak_open.max(self.open.len().saturating_add(1));
            self.open.push(OpenElement {
                name,
                disposition: Disposition::Skip,
                saved_style: None,
                saved_link: None,
            });
            return raw;
        }

        // Structural depth cap (§9.2). Past it, open no structure; text flows
        // into enclosing block. Recorded so UI says article not shown in full.
        if self.open.len() >= self.ctx.limits.max_depth {
            self.record_truncation(Truncation::TooDeep);
            return TokenSinkResult::Continue;
        }

        if let Disposition::Void(void) = disposition {
            self.void_element(void, tag);
            return TokenSinkResult::Continue;
        }

        let mut saved_style = None;
        let mut saved_link = None;

        match disposition {
            Disposition::Skip | Disposition::Void(_) => unreachable_disposition(),
            Disposition::Flatten => {}
            Disposition::Inline(inline) => {
                saved_style = Some(self.style);
                match inline {
                    InlineTag::Bold => self.style.bold = true,
                    InlineTag::Italic => self.style.italic = true,
                    InlineTag::Code => self.style.code = true,
                    InlineTag::Strike => self.style.strike = true,
                    InlineTag::Superscript => self.style.superscript = true,
                    InlineTag::Subscript => self.style.subscript = true,
                    InlineTag::Anchor => {
                        saved_link = Some(self.link.clone());
                        // §9.2: only http(s) becomes rendered link. Unparseable
                        // or dangerous href → plain text, never tappable link.
                        self.link =
                            non_empty_attr(tag, "href").and_then(|href| self.resolve(&href));
                    }
                }
            }
            Disposition::Block(block) => self.start_block(block, tag),
        }

        if !childless {
            self.peak_open = self.peak_open.max(self.open.len().saturating_add(1));
            self.open.push(OpenElement {
                name,
                disposition,
                saved_style,
                saved_link,
            });
        } else if let Some(style) = saved_style {
            // Self-closing inline element styles nothing.
            self.style = style;
            if let Some(link) = saved_link {
                self.link = link;
            }
        }

        TokenSinkResult::Continue
    }

    fn start_block(&mut self, block: BlockTag, tag: &Tag) {
        match block {
            BlockTag::Paragraph => self.flush(),
            BlockTag::Heading(level) => {
                self.flush();
                self.pending = Pending::Heading(level);
            }
            BlockTag::List { ordered } => {
                self.flush();
                if self.lists.len() < usize::from(self.ctx.limits.max_list_indent) {
                    self.lists.push(ListState {
                        ordered,
                        counter: 0,
                    });
                }
            }
            BlockTag::ListItem => {
                self.flush();
                let indent = self.list_indent();
                let (ordered, number) = match self.lists.last_mut() {
                    Some(list) => {
                        list.counter = list.counter.saturating_add(1);
                        (list.ordered, list.ordered.then_some(list.counter))
                    }
                    // `<li>` without list: render as bullet, don't drop user's
                    // text.
                    None => (false, None),
                };
                self.pending = Pending::ListItem {
                    ordered,
                    number,
                    indent,
                };
            }
            BlockTag::Quote => {
                self.flush();
                self.quote_depth = self
                    .quote_depth
                    .saturating_add(1)
                    .min(self.ctx.limits.max_quote_depth);
            }
            BlockTag::Pre => {
                self.flush();
                self.pre_depth = self.pre_depth.saturating_add(1);
                if self.pre_depth == 1 {
                    self.pre_text.clear();
                    self.pre_language = class_language(tag);
                }
            }
            BlockTag::Table => {
                self.flush();
                // Nested tables flatten into enclosing one: real nesting
                // reintroduces unbounded depth; Qt 5.6 has no table rendering
                // to nest into anyway.
                self.table_depth = self.table_depth.saturating_add(1);
                if self.table.is_none() {
                    self.table = Some(TableBuild::default());
                }
            }
            BlockTag::TableRow => {
                self.flush();
                let max_rows = self.ctx.limits.max_table_rows;
                let mut hit_cap = false;
                if let Some(table) = self.table.as_mut() {
                    if let Some(row) = table.current_row.take() {
                        if !row.is_empty() {
                            if table.rows.len() < max_rows {
                                table.rows.push(row);
                            } else {
                                hit_cap = true;
                            }
                        }
                    }
                    table.current_row = Some(Vec::new());
                }
                if hit_cap {
                    self.record_truncation(Truncation::TooManyBlocks);
                }
            }
            BlockTag::TableCell { header } => {
                self.flush();
                let max_cells = self.ctx.limits.max_table_cells_per_row;
                let mut hit_cap = false;
                if let Some(table) = self.table.as_mut() {
                    table.current_header = header;
                    if table.current_row.is_none() {
                        table.current_row = Some(Vec::new());
                    }
                    if let Some(row) = table.current_row.as_mut() {
                        if row.len() < max_cells {
                            row.push(TableCell {
                                spans: Vec::new(),
                                header,
                            });
                        } else {
                            hit_cap = true;
                        }
                    }
                }
                if hit_cap {
                    self.record_truncation(Truncation::TooManyBlocks);
                }
            }
        }
    }

    fn void_element(&mut self, void: VoidTag, tag: &Tag) {
        match void {
            VoidTag::Break => {
                if self.pre_depth > 0 {
                    self.pre_text.push('\n');
                } else if !self.spans.is_empty() {
                    self.spans.push(Span {
                        text: "\n".to_owned(),
                        style: self.style,
                        link: self.link.clone(),
                    });
                }
            }
            VoidTag::Rule => {
                self.flush();
                self.push_block(BlockKind::Rule);
            }
            VoidTag::Image => {
                let Some(src) = non_empty_attr(tag, "src").and_then(|s| self.resolve(&s)) else {
                    return;
                };
                // §9.3: decided here, before URL reaches UI: QML `Image`
                // binding to `src` can't contact third-party host.
                let (src, fetch) = match self.ctx.media.decide(&src) {
                    MediaDecision::Fetch(src) => (src, MediaFetch::Allowed),
                    MediaDecision::NeedsConsent(src) => (src, MediaFetch::NeedsConsent),
                    MediaDecision::Drop => return,
                };
                self.flush();
                let alt = attr(tag, "alt").unwrap_or_default();
                let title = attr(tag, "title").filter(|t| !t.is_empty());
                self.push_block(BlockKind::Image {
                    src,
                    alt,
                    title,
                    fetch,
                    intrinsic: intrinsic_size(tag),
                    // Read before anything touches it: `flush` above ends text
                    // block but leaves open `<a>` in force, as `<a><img></a>`
                    // needs.
                    link: self.link.clone(),
                });
            }
        }
    }

    /// Resolve attribute value into validated `http(s)` URL.
    fn resolve(&self, raw: &str) -> Option<MediaUrl> {
        match &self.ctx.base_url {
            Some(base) => MediaUrl::parse_relative(raw, base),
            // No base URL: relative ref unresolvable; guessing origin worse
            // than dropping.
            None => MediaUrl::parse(raw),
        }
    }

    fn end_tag(&mut self, tag: &Tag) {
        let name = tag.name.as_ref().to_ascii_lowercase();

        // Find matching open element, innermost first. Ignore unmatched end
        // tags, don't unwind whole stack: feed HTML full of stray `</div>`s.
        let Some(index) = self.open.iter().rposition(|el| el.name == name) else {
            return;
        };

        // Close innermost open element down to match.
        while self.open.len() > index {
            let Some(element) = self.open.pop() else {
                break;
            };
            self.close_element(&element);
            if let Some(skip_from) = self.skip_from {
                if self.open.len() <= skip_from {
                    self.skip_from = None;
                }
            }
        }
    }

    fn close_element(&mut self, element: &OpenElement) {
        if self.skip_from.is_some() {
            // Restore nothing: skipped subtree applied no styling.
            return;
        }

        if let Some(style) = element.saved_style {
            self.style = style;
        }
        if let Some(link) = &element.saved_link {
            self.link.clone_from(link);
        }

        match element.disposition {
            Disposition::Block(BlockTag::Heading(_) | BlockTag::Paragraph | BlockTag::ListItem) => {
                self.flush();
            }
            Disposition::Block(BlockTag::List { .. }) => {
                self.flush();
                self.lists.pop();
            }
            Disposition::Block(BlockTag::Quote) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            Disposition::Block(BlockTag::Pre) => {
                self.pre_depth = self.pre_depth.saturating_sub(1);
                if self.pre_depth == 0 {
                    let text = std::mem::take(&mut self.pre_text);
                    let language = self.pre_language.take();
                    let trimmed = text.trim_matches('\n');
                    if !trimmed.is_empty() {
                        let owned = trimmed.to_owned();
                        self.push_block(BlockKind::Code {
                            language,
                            text: owned,
                        });
                    }
                    // Inline spans before `<pre>` flushed on open; discard
                    // stray ones.
                    self.spans.clear();
                }
            }
            Disposition::Block(BlockTag::Table) => {
                self.flush();
                self.table_depth = self.table_depth.saturating_sub(1);
                // Only outermost `</table>` emits block; inner one would
                // truncate outer table.
                if self.table_depth == 0 {
                    self.close_table();
                }
            }
            Disposition::Block(BlockTag::TableRow) => {
                self.flush();
                // Cap on BOTH row-commit paths: here (`</tr>`) and
                // `start_block` flushing previous row on new `<tr>`. Else
                // well-formed markup unbounded.
                let max_rows = self.ctx.limits.max_table_rows;
                let mut hit_cap = false;
                if let Some(table) = self.table.as_mut() {
                    if let Some(row) = table.current_row.take() {
                        if !row.is_empty() {
                            if table.rows.len() < max_rows {
                                table.rows.push(row);
                            } else {
                                hit_cap = true;
                            }
                        }
                    }
                }
                if hit_cap {
                    self.record_truncation(Truncation::TooManyBlocks);
                }
            }
            Disposition::Block(BlockTag::TableCell { .. }) => self.flush(),
            Disposition::Skip
            | Disposition::Flatten
            | Disposition::Inline(_)
            | Disposition::Void(_) => {}
        }
    }
}

impl TokenSink for Builder {
    type Handle = ();

    fn process_token(&mut self, token: Token, _line: u64) -> TokenSinkResult<()> {
        // Stop work once every cap spent; tokenizer still drains, nothing
        // allocated.
        if self.at_capacity() && self.truncated.is_some() {
            return TokenSinkResult::Continue;
        }

        match token {
            Token::TagToken(tag) => match tag.kind {
                TagKind::StartTag => return self.start_tag(&tag),
                TagKind::EndTag => self.end_tag(&tag),
            },
            Token::CharacterTokens(text) => self.push_text(&text),
            // Comments, doctypes, parse errors, stray NULs: nothing reader
            // wants.
            Token::CommentToken(_)
            | Token::DoctypeToken(_)
            | Token::NullCharacterToken
            | Token::ParseError(_)
            | Token::EOFToken => {}
        }
        TokenSinkResult::Continue
    }
}

/// Ask tokenizer to treat element contents as text, so `<script>if (a<b)
/// {}</script>` can't yield spurious `<b>` start tag.
fn raw_text_kind(name: &str) -> TokenSinkResult<()> {
    match name {
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "style" | "textarea" | "title" => TokenSinkResult::RawData(RawKind::Rawtext),
        _ => TokenSinkResult::Continue,
    }
}

/// Branch match above already excluded.
///
/// `unreachable!()` would panic; §9.5 forbids reachable panics here. Same
/// meaning, no panic.
fn unreachable_disposition() {}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// `<img>` tag's own `width` and `height`, when both usable.
///
/// Plain integer pixel count only: percentage, `calc()`, unit suffix, zero say
/// nothing about ratio; both needed.
///
/// Capped hard: foreign input shaping a rectangle; 4-billion-pixel claim
/// mustn't become huge reserved space or overflowing division. Cap far above
/// real images, far below harm.
fn intrinsic_size(tag: &Tag) -> Option<(u32, u32)> {
    const MAX_DIMENSION: u32 = 20_000;
    let dimension = |name: &str| -> Option<u32> {
        attr(tag, name)?
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0 && *n <= MAX_DIMENSION)
    };
    Some((dimension("width")?, dimension("height")?))
}

fn attr(tag: &Tag, wanted: &str) -> Option<String> {
    tag.attrs
        .iter()
        .find(|a| a.name.local.as_ref().eq_ignore_ascii_case(wanted))
        .map(|a| a.value.to_string())
}

/// Present-but-empty attribute = absent.
///
/// Feeds contain valueless attributes (`<a href>`). Empty `href` resolves to
/// article itself: link back to current page, worse than no link.
fn non_empty_attr(tag: &Tag, wanted: &str) -> Option<String> {
    attr(tag, wanted).filter(|v| !v.trim().is_empty())
}

/// Extract a language hint from `<pre class="language-rust">` / `highlight-rust`.
fn class_language(tag: &Tag) -> Option<String> {
    let class = attr(tag, "class")?;
    class.split_whitespace().find_map(|c| {
        let c = c.to_ascii_lowercase();
        for prefix in ["language-", "lang-", "highlight-", "brush:"] {
            if let Some(rest) = c.strip_prefix(prefix) {
                if !rest.is_empty() {
                    return Some(rest.to_owned());
                }
            }
        }
        None
    })
}

/// Collapse ASCII whitespace runs to one space.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

/// Truncate to ≤ `budget` bytes without splitting a char.
fn clamp_to_char_boundary(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    // Char ≤ 4 bytes: boundary ≤ 3 back.
    let cut = (0..=budget)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    text.get(..cut).unwrap_or("")
}

/// Trim leading/trailing whitespace across span run, remove emptied spans: no
/// stray padding.
fn trim_spans(mut spans: Vec<Span>) -> Vec<Span> {
    while let Some(first) = spans.first_mut() {
        let trimmed = first.text.trim_start();
        if trimmed.len() != first.text.len() {
            first.text = trimmed.to_owned();
        }
        if first.text.is_empty() {
            spans.remove(0);
        } else {
            break;
        }
    }
    while let Some(last) = spans.last_mut() {
        let trimmed = last.text.trim_end();
        if trimmed.len() != last.text.len() {
            last.text = trimmed.to_owned();
        }
        if last.text.is_empty() {
            spans.pop();
        } else {
            break;
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::url::UnproxiedMedia;

    fn instance() -> Url {
        Url::parse("https://miniflux.example/").unwrap()
    }

    /// Context rendering third-party images, so *structure* tests don't also
    /// test media policy.
    fn lenient_ctx() -> TransformContext {
        TransformContext {
            base_url: Some(Url::parse("https://blog.example/post/").unwrap()),
            media: MediaPolicy::ProxyThroughInstance {
                instance: instance(),
                extra_trusted: Vec::new(),
                fallback: UnproxiedMedia::Allow,
            },
            limits: Limits::default(),
        }
    }

    fn strict_ctx() -> TransformContext {
        TransformContext {
            media: MediaPolicy::ProxyThroughInstance {
                instance: instance(),
                extra_trusted: Vec::new(),
                fallback: UnproxiedMedia::Strict,
            },
            base_url: Some(Url::parse("https://blog.example/post/").unwrap()),
            ..TransformContext::new(instance())
        }
    }

    fn text_of(doc: &Document) -> String {
        doc.blocks
            .iter()
            .map(RenderBlock::plain_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---------------------------------------------------------------- basics

    #[test]
    fn paragraphs_and_headings() {
        let doc = transform("<h2>Title</h2><p>Hello <b>world</b>.</p>", &lenient_ctx());
        assert_eq!(doc.blocks.len(), 2);
        assert!(matches!(
            doc.blocks.first().map(|b| &b.kind),
            Some(BlockKind::Heading { level: 2, .. })
        ));
        assert_eq!(text_of(&doc), "Title\nHello world.");
    }

    #[test]
    fn entities_are_decoded_once() {
        let doc = transform("<p>a &amp; b &lt;tag&gt; &#8212; end</p>", &lenient_ctx());
        // Tokenizer decodes references; transform mustn't decode again.
        assert_eq!(text_of(&doc), "a & b <tag> \u{2014} end");
    }

    #[test]
    fn inline_styles_nest_and_restore() {
        let doc = transform("<p>a<b>b<i>c</i>d</b>e</p>", &lenient_ctx());
        let BlockKind::Paragraph { spans } = doc.blocks.first().map(|b| &b.kind).unwrap() else {
            panic!("expected a paragraph")
        };
        let styled: Vec<_> = spans
            .iter()
            .map(|s| (s.text.as_str(), s.style.bold, s.style.italic))
            .collect();
        assert_eq!(
            styled,
            vec![
                ("a", false, false),
                ("b", true, false),
                ("c", true, true),
                ("d", true, false),
                ("e", false, false)
            ]
        );
    }

    #[test]
    fn lists_number_and_indent() {
        let doc = transform(
            "<ol><li>one</li><li>two<ul><li>inner</li></ul></li></ol>",
            &lenient_ctx(),
        );
        let items: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::ListItem {
                    ordered,
                    number,
                    indent,
                    spans,
                } => Some((*ordered, *number, *indent, Span::render_plain_text(spans))),
                _ => None,
            })
            .collect();
        assert_eq!(items.first(), Some(&(true, Some(1), 0, "one".to_owned())));
        assert_eq!(items.get(1), Some(&(true, Some(2), 0, "two".to_owned())));
        assert_eq!(items.get(2), Some(&(false, None, 1, "inner".to_owned())));
    }

    #[test]
    fn blockquote_depth_is_a_scalar_not_nesting() {
        let doc = transform(
            "<blockquote><p>a</p><blockquote><p>b</p></blockquote></blockquote>",
            &lenient_ctx(),
        );
        let depths: Vec<u8> = doc.blocks.iter().map(|b| b.quote_depth).collect();
        assert_eq!(depths, vec![1, 2]);
    }

    #[test]
    fn pre_preserves_whitespace_and_reads_language() {
        let doc = transform(
            "<pre class=\"language-rust\">fn main() {\n    let x = 1;\n}</pre>",
            &lenient_ctx(),
        );
        let Some(BlockKind::Code { language, text }) = doc.blocks.first().map(|b| b.kind.clone())
        else {
            panic!("expected a code block, got {:?}", doc.blocks)
        };
        assert_eq!(language.as_deref(), Some("rust"));
        assert_eq!(text, "fn main() {\n    let x = 1;\n}");
    }

    #[test]
    fn tables_become_rows_of_cells() {
        let doc = transform(
            "<table><tr><th>h1</th><th>h2</th></tr><tr><td>a</td><td>b</td></tr></table>",
            &lenient_ctx(),
        );
        let Some(BlockKind::Table { rows }) = doc.blocks.first().map(|b| b.kind.clone()) else {
            panic!("expected a table, got {:?}", doc.blocks)
        };
        assert_eq!(rows.len(), 2);
        assert!(rows.first().is_some_and(|r| r.iter().all(|c| c.header)));
        let body: Vec<String> = rows
            .get(1)
            .map(|r| {
                r.iter()
                    .map(|c| Span::render_plain_text(&c.spans))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(body, vec!["a".to_owned(), "b".to_owned()]);
    }

    // ------------------------------------------------------- §9.2 allowlist

    #[test]
    fn script_and_style_contents_never_appear() {
        let doc = transform(
            "<p>before</p><script>alert('pwned'); if (a<b) { evil() }</script>\
             <style>body{display:none}</style><p>after</p>",
            &lenient_ctx(),
        );
        let text = text_of(&doc);
        assert!(!text.contains("pwned"), "script body leaked: {text}");
        assert!(!text.contains("display:none"), "style body leaked: {text}");
        assert_eq!(text, "before\nafter");
    }

    #[test]
    fn a_less_than_inside_script_cannot_forge_a_tag() {
        // Without RawData handling tokenizer reads `<b>` from script body;
        // following text renders bold.
        let doc = transform("<script>if (a<b) {}</script><p>plain</p>", &lenient_ctx());
        let BlockKind::Paragraph { spans } = doc.blocks.first().map(|b| &b.kind).unwrap() else {
            panic!("expected a paragraph")
        };
        assert!(
            spans.iter().all(|s| !s.style.bold),
            "forged <b> applied styling"
        );
    }

    #[test]
    fn unknown_elements_flatten_to_their_text() {
        let doc = transform(
            "<p>a <blink>b</blink> <custom-elem>c</custom-elem> d</p>",
            &lenient_ctx(),
        );
        assert_eq!(text_of(&doc), "a b c d");
    }

    #[test]
    fn dangerous_link_schemes_render_as_plain_text() {
        for href in [
            "javascript:alert(1)",
            "data:text/html,<script>x</script>",
            "file:///etc/passwd",
        ] {
            let html = format!("<p><a href=\"{href}\">click me</a></p>");
            let doc = transform(&html, &lenient_ctx());
            let BlockKind::Paragraph { spans } = doc.blocks.first().map(|b| &b.kind).unwrap()
            else {
                panic!("expected a paragraph")
            };
            assert!(
                spans.iter().all(|s| s.link.is_none()),
                "{href} survived as a tappable link"
            );
            // Text kept: dropping loses readable content.
            assert_eq!(Span::render_plain_text(spans), "click me");
        }
    }

    #[test]
    fn safe_links_survive_and_resolve_relatively() {
        let doc = transform("<p><a href=\"../x\">go</a></p>", &lenient_ctx());
        let BlockKind::Paragraph { spans } = doc.blocks.first().map(|b| &b.kind).unwrap() else {
            panic!("expected a paragraph")
        };
        assert_eq!(
            spans
                .first()
                .and_then(|s| s.link.as_ref())
                .map(MediaUrl::as_str),
            Some("https://blog.example/x")
        );
    }

    /// §the `<img>` tag's own dimensions, carried for layout.
    ///
    /// Device: article content jumped while scrolling; UI can't reserve room
    /// for unknown image. Ratio advisory (shapes rectangle, bounds nothing):
    /// either trustworthy-looking or absent, never half-parsed.
    #[test]
    fn an_images_own_dimensions_are_carried_when_they_are_usable() {
        let intrinsic_of = |html: &str| -> Option<(u32, u32)> {
            transform(html, &lenient_ctx())
                .blocks
                .into_iter()
                .find_map(|b| match b.kind {
                    BlockKind::Image { intrinsic, .. } => Some(intrinsic),
                    _ => None,
                })
                .expect("an image block")
        };

        assert_eq!(
            intrinsic_of(r#"<img src="/a.png" width="800" height="600">"#),
            Some((800, 600))
        );

        // Anything not a whole-pixel ratio is no hint, mustn't become wrong
        // one.
        for html in [
            r#"<img src="/a.png" width="800">"#,  // height missing
            r#"<img src="/a.png" height="600">"#, // width missing
            r#"<img src="/a.png" width="100%" height="50%">"#, // percentages
            r#"<img src="/a.png" width="800px" height="600px">"#, // units
            r#"<img src="/a.png" width="0" height="600">"#, // a zero ratio
            r#"<img src="/a.png" width="-800" height="600">"#, // negative
            r#"<img src="/a.png" width="abc" height="600">"#, // not a number
            r#"<img src="/a.png">"#,              // nothing at all
        ] {
            assert_eq!(intrinsic_of(html), None, "{html}");
        }

        // Foreign input: any claimed size; reserved rectangle mustn't follow it
        // anywhere absurd.
        assert_eq!(
            intrinsic_of(r#"<img src="/a.png" width="4294967295" height="4294967295">"#),
            None,
            "an enormous claim is no hint, not an enormous hint"
        );
        assert_eq!(
            intrinsic_of(r#"<img src="/a.png" width="99999999999999999999" height="1">"#),
            None,
            "and one that does not even fit in the type is not a panic"
        );
    }

    /// §an image inside a link carries the link, and only that one.
    ///
    /// Article view zooms image on tap, unless feed made it a link (tap = "go
    /// there"). Block is only place UI learns which.
    #[test]
    fn an_image_knows_the_link_it_sits_in() {
        let link_of = |html: &str| -> Option<String> {
            transform(html, &lenient_ctx())
                .blocks
                .into_iter()
                .find_map(|b| match b.kind {
                    BlockKind::Image { link, .. } => Some(link),
                    _ => None,
                })
                .expect("an image block")
                .map(|u| u.as_str().to_owned())
        };

        assert_eq!(
            link_of(r#"<p><a href="https://site.example/full"><img src="/a.png"></a></p>"#),
            Some("https://site.example/full".to_owned())
        );
        // Relative, resolved like any other link.
        assert_eq!(
            link_of(r#"<a href="../big.png"><img src="/a.png"></a>"#),
            Some("https://blog.example/big.png".to_owned())
        );
        // Not linked at all.
        assert_eq!(link_of(r#"<p><img src="/a.png"></p>"#), None);
        // Link that CLOSED before image doesn't leak onto it.
        assert_eq!(
            link_of(r#"<p><a href="https://site.example/">x</a> <img src="/a.png"></p>"#),
            None
        );
        // §9.2: never-a-link scheme isn't one here either.
        assert_eq!(
            link_of(r#"<a href="javascript:alert(1)"><img src="/a.png"></a>"#),
            None
        );
    }

    #[test]
    fn data_uri_images_are_dropped() {
        let doc = transform(
            "<p><img src=\"data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=\" alt=\"x\"></p>",
            &lenient_ctx(),
        );
        assert!(
            !doc.blocks
                .iter()
                .any(|b| matches!(b.kind, BlockKind::Image { .. })),
            "a data: URI became an image"
        );
    }

    // --------------------------------------------------------- §9.3 privacy

    #[test]
    fn third_party_images_are_not_fetched_under_the_strict_policy() {
        let doc = transform(
            "<p><img src=\"https://tracker.example/pixel.gif\" alt=\"t\"></p>\
             <p><img src=\"https://miniflux.example/proxy/sig/abc\" alt=\"ok\"></p>",
            &strict_ctx(),
        );
        let srcs: Vec<String> = doc
            .blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::Image { src, .. } => Some(src.as_str().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(
            srcs,
            vec!["https://miniflux.example/proxy/sig/abc".to_owned()],
            "a third-party host would have been contacted from the phone"
        );
    }

    // ------------------------------------------------------------ §9.2 caps

    #[test]
    fn deeply_nested_markup_does_not_overflow_the_stack() {
        // Case tokenizer-not-DOM exists for: RcDom of this input overflows
        // stack on *drop*.
        let depth = 50_000;
        let html = format!("{}deep{}", "<div>".repeat(depth), "</div>".repeat(depth));
        let doc = transform(&html, &lenient_ctx());
        assert!(
            text_of(&doc).contains("deep"),
            "content lost under deep nesting"
        );
        // And structural cap fired. Else test only says "didn't crash", which
        // design guarantees anyway; depth cap could vanish green.
        assert_eq!(
            doc.truncated,
            Some(Truncation::TooDeep),
            "50,000 levels must hit the depth cap and say so"
        );
    }

    #[test]
    fn deeply_nested_unclosed_markup_is_also_survivable() {
        // No closing tags: open-element stack never unwinds.
        let html = format!("{}tail", "<div><span><em>".repeat(20_000));
        let doc = transform(&html, &lenient_ctx());
        assert!(text_of(&doc).contains("tail"));
        assert_eq!(doc.truncated, Some(Truncation::TooDeep));
    }

    #[test]
    fn oversized_input_is_cut_before_parsing() {
        let ctx = TransformContext {
            limits: Limits {
                max_input_bytes: 1024,
                ..Limits::default()
            },
            ..lenient_ctx()
        };
        let html = format!("<p>{}</p>", "x".repeat(64 * 1024));
        let doc = transform(&html, &ctx);
        assert_eq!(doc.truncated, Some(Truncation::InputTooLarge));
        assert!(text_of(&doc).len() <= 1024);
    }

    #[test]
    fn block_count_is_capped_and_reported() {
        let ctx = TransformContext {
            limits: Limits {
                max_blocks: 10,
                ..Limits::default()
            },
            ..lenient_ctx()
        };
        let html = "<p>x</p>".repeat(500);
        let doc = transform(&html, &ctx);
        assert_eq!(doc.blocks.len(), 10);
        assert_eq!(
            doc.truncated,
            Some(Truncation::TooManyBlocks),
            "a silent truncation reads as 'this is the whole article' when it is not"
        );
    }

    #[test]
    fn one_oversized_paragraph_is_truncated_not_erased() {
        // Regression: block REACHING text cap must be emitted. Single long
        // `<p>` article must truncate, not render zero blocks.
        let ctx = TransformContext {
            limits: Limits {
                max_text_bytes: 512,
                ..Limits::default()
            },
            ..lenient_ctx()
        };
        let html = format!("<p>{}</p>", "abcd ".repeat(10_000));
        let doc = transform(&html, &ctx);

        assert_eq!(
            doc.blocks.len(),
            1,
            "the paragraph must survive, truncated: {doc:?}"
        );
        let text = text_of(&doc);
        assert!(text.starts_with("abcd"), "text was {text:?}");
        assert!(
            text.len() <= 600,
            "truncated text is {} bytes for a 512-byte budget",
            text.len()
        );
        assert_eq!(doc.truncated, Some(Truncation::TooMuchText));
    }

    #[test]
    fn text_volume_is_capped() {
        let ctx = TransformContext {
            limits: Limits {
                max_text_bytes: 512,
                ..Limits::default()
            },
            ..lenient_ctx()
        };
        // MANY SMALL paragraphs, not one huge: single 50 KB paragraph gets
        // dropped whole, rendered text "" passes `<= 600` with or without cap.
        let html = "<p>abcd abcd abcd abcd</p>".repeat(500);
        let doc = transform(&html, &ctx);
        let text = text_of(&doc);

        assert!(
            doc.truncated.is_some(),
            "silently dropping most of an article reads as 'this is all of it'"
        );
        assert!(
            !text.is_empty(),
            "the cap must truncate the article, not erase it"
        );
        assert!(
            text.len() <= 600,
            "text cap overshot: {} bytes for a 512-byte budget",
            text.len()
        );
        assert!(
            text.len() >= 256,
            "the transform kept only {} bytes of a 512-byte budget, so this fixture \
             is not actually exercising the cap",
            text.len()
        );
    }

    #[test]
    fn quote_and_list_depth_are_clamped() {
        let ctx = TransformContext {
            limits: Limits {
                max_quote_depth: 3,
                max_list_indent: 2,
                ..Limits::default()
            },
            ..lenient_ctx()
        };
        let html = format!(
            "{}<p>deep</p>{}",
            "<blockquote>".repeat(50),
            "</blockquote>".repeat(50)
        );
        let doc = transform(&html, &ctx);
        assert!(doc.blocks.iter().all(|b| b.quote_depth <= 3));

        let nested_lists = format!("{}<li>x</li>{}", "<ul>".repeat(50), "</ul>".repeat(50));
        let doc = transform(&nested_lists, &ctx);
        assert!(doc.blocks.iter().all(|b| match b.kind {
            BlockKind::ListItem { indent, .. } => indent <= 2,
            _ => true,
        }));
    }

    // ------------------------------------------------------ malformed input

    #[test]
    fn malformed_markup_does_not_panic() {
        // §9.5: malformed response = handled error, not crash. Fuzz targets
        // generalise; these are real-feed shapes.
        let cases = [
            "",
            "<",
            "<<<<>>>>",
            "<p",
            "<p>unclosed",
            "</p></div></span>",
            "<p><b>bold <i>both</p></b></i>",
            "<table><td>orphan cell</td>",
            "<ul><li>a<li>b<li>c",
            "<img src=>",
            "<a href>text</a>",
            "<pre><code>x",
            "<!-- comment only -->",
            "<!DOCTYPE html>",
            "\u{0}\u{0}\u{0}",
            "<p>\u{fffd}</p>",
            "<h7>not a heading</h7>",
            "<blockquote>",
        ];
        for html in cases {
            let doc = transform(html, &lenient_ctx());
            // Contract: it returns. Cheap assert so call isn't optimised away.
            let _ = doc.blocks.len();
        }
    }

    #[test]
    fn tables_are_bounded_like_everything_else() {
        // Table = SINGLE block; `max_blocks` doesn't bound it (half a million
        // empty `<td>`s: 80 MB, one block, no truncation).
        //
        // Both commit paths need cap: `</tr>` and next `<tr>`. Well-formed
        // markup is real feed shape.
        let limits = Limits::default();

        let tall = format!("<table>{}</table>", "<tr><td>x</td></tr>".repeat(50_000));
        let doc = transform(&tall, &lenient_ctx());
        let Some(BlockKind::Table { rows }) = doc.blocks.first().map(|b| b.kind.clone()) else {
            panic!("expected a table")
        };
        assert_eq!(rows.len(), limits.max_table_rows);
        assert!(doc.truncated.is_some(), "a capped table must say so");

        let wide = format!("<table><tr>{}</tr></table>", "<td>y</td>".repeat(20_000));
        let doc = transform(&wide, &lenient_ctx());
        let Some(BlockKind::Table { rows }) = doc.blocks.first().map(|b| b.kind.clone()) else {
            panic!("expected a table")
        };
        assert_eq!(
            rows.first().map(Vec::len),
            Some(limits.max_table_cells_per_row),
            "cells per row must be capped"
        );
        assert!(doc.truncated.is_some());
    }

    #[test]
    fn a_childless_skip_element_does_not_truncate_the_article() {
        // Regression: `<meta>`, `<input>`, `<embed>` etc. are skip-listed AND
        // void; skip mode for them never cleared, discarding everything after.
        // `<input>` common in feeds.
        for tag in [
            "<meta>",
            "<link>",
            "<input>",
            "<area>",
            "<source>",
            "<track>",
            "<param>",
            "<embed>",
            "<base href=\"https://x.example/\">",
            "<svg/>",
            "<template/>",
        ] {
            let html = format!("<p>before</p>{tag}<p>after</p>");
            let doc = transform(&html, &lenient_ctx());
            assert_eq!(
                text_of(&doc),
                "before\nafter",
                "{tag} swallowed the rest of the document"
            );
        }
    }

    #[test]
    fn skipped_content_does_not_leak_past_the_depth_cap() {
        // Regression: depth cap before skip handling let `<script>` past cap
        // skip raw-text mode; body rendered as text (`alert('pwned')`).
        // Allowlist breach.
        let depth = Limits::default().max_depth + 2;
        let html = format!(
            "{}<script>alert('pwned')</script><style>body{{display:none}}</style>{}",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        );
        let text = text_of(&transform(&html, &lenient_ctx()));
        assert!(
            !text.contains("pwned"),
            "script body leaked past the depth cap: {text:?}"
        );
        assert!(
            !text.contains("display:none"),
            "style body leaked: {text:?}"
        );
    }

    #[test]
    fn stray_end_tags_do_not_destroy_structure() {
        // Stray `</div>` mustn't unwind whole open-element stack.
        let doc = transform("<p>a</div> b</p>", &lenient_ctx());
        assert_eq!(text_of(&doc), "a b");
    }

    #[test]
    fn whitespace_between_tags_collapses() {
        let doc = transform("<p>a\n\n   \tb</p>", &lenient_ctx());
        assert_eq!(text_of(&doc), "a b");
    }
}

#[cfg(test)]
mod empty_attribute_tests {
    use super::tests_support::*;

    #[test]
    fn a_valueless_href_is_not_a_link_to_the_article_itself() {
        let doc = super::transform("<p><a href>text</a></p>", &ctx());
        let super::BlockKind::Paragraph { spans } = doc.blocks.first().map(|b| &b.kind).unwrap()
        else {
            panic!("expected a paragraph")
        };
        assert!(
            spans.iter().all(|s| s.link.is_none()),
            "an empty href resolved to the article's own URL"
        );
        assert_eq!(super::Span::render_plain_text(spans), "text");
    }

    #[test]
    fn a_valueless_src_is_not_an_image_of_the_article() {
        let doc = super::transform("<p><img src alt=\"x\"></p>", &ctx());
        assert!(!doc
            .blocks
            .iter()
            .any(|b| matches!(b.kind, super::BlockKind::Image { .. })));
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;

    pub(super) fn ctx() -> TransformContext {
        TransformContext {
            base_url: Some(Url::parse("https://blog.example/post/").unwrap()),
            media: MediaPolicy::ProxyThroughInstance {
                instance: Url::parse("https://miniflux.example/").unwrap(),
                extra_trusted: Vec::new(),
                fallback: crate::content::url::UnproxiedMedia::Allow,
            },
            limits: Limits::default(),
        }
    }
}

#[cfg(test)]
mod resource_exhaustion_tests {
    use super::tests_support::ctx;
    use super::{Limits, TransformContext};
    use std::time::Instant;

    /// Depth-capped stack stays capped on *every* path.
    ///
    /// Regression (fuzzing): skipped subtree (`<svg>`, `<script>`, …) pushed
    /// before depth cap; stack unbounded, each unmatched end tag scanned all of
    /// it.
    #[test]
    fn a_skipped_subtree_cannot_grow_the_element_stack_without_bound() {
        // Scaling ratio, not wall-clock threshold: absolute timing flakes on
        // loaded CI; quadratic shows as ratio on any machine.
        let small = {
            let html = format!("<svg>{}{}", "<b>".repeat(10_000), "</zz>".repeat(10_000));
            let t = Instant::now();
            let _ = super::transform(&html, &ctx());
            t.elapsed()
        };
        let large = {
            let html = format!("<svg>{}{}", "<b>".repeat(40_000), "</zz>".repeat(40_000));
            let t = Instant::now();
            let _ = super::transform(&html, &ctx());
            t.elapsed()
        };

        // 4× input. Linear ~4×, bug ~16× (1.3 s → 23 s). 10 clear of noise and
        // defect.
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.000_001);
        assert!(
            ratio < 10.0,
            "transform scales super-linearly on skipped subtrees: \
             10k took {small:?}, 40k took {large:?} (ratio {ratio:.1}x)"
        );
    }

    #[test]
    fn the_depth_cap_holds_through_a_skipped_subtree() {
        // Same input, checked structurally, not by timing.
        //
        // `assert!(doc.blocks.is_empty())` alone says nothing: all inside
        // skipped `<svg>`, no text, zero blocks either way. Observe stack
        // itself.
        let limits = Limits {
            max_depth: 8,
            ..Limits::default()
        };
        let context = TransformContext { limits, ..ctx() };
        let html = format!("<svg>{}", "<b>".repeat(5_000));

        let builder = super::drive(&html, &context, None);
        let peak = builder.peak_open;
        let doc = builder.finish();

        assert!(doc.blocks.is_empty(), "a skipped subtree emits nothing");
        assert!(
            peak <= limits.max_depth,
            "the open-element stack reached {peak}, past the cap of {}. \
             A skipped subtree must not be a way around the §9.2 depth cap: \
             that is a 5000-deep stack built from attacker-controlled bytes.",
            limits.max_depth
        );
    }
}
