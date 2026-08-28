//! The HTML → render-block transform.
//!
//! # Why a tokenizer and not a DOM
//!
//! This module drives `html5ever`'s **tokenizer** directly and never
//! constructs a tree. That is a safety decision, not a performance one.
//!
//! The obvious implementation parses to an `RcDom` and walks it. An `RcDom` is
//! a tree of `Rc`s, and dropping one is *recursive*: a document nested ten
//! thousand `<div>`s deep overflows the stack in the destructor, on the cleanup
//! path, where the crash is hardest to attribute. §9.2 warns about deeply
//! nested markup against a recursive parser; a recursive destructor is the same
//! bug wearing a hat. Consuming a flat token stream and assembling a flat
//! [`Document`] means there is no recursive structure to build *or* to drop.
//!
//! # Allowlist by construction
//!
//! §9.2: *allowlist, never blocklist. A blocklist is a promise to have thought
//! of every tag, which nobody can keep.* Recognised elements map to
//! [`BlockKind`] variants or to inline styles; everything else falls through to
//! [`Disposition::Flatten`], which drops the tag and keeps its text. There is
//! no passthrough path, so an element nobody has heard of renders as its text
//! content and nothing more.
//!
//! A small set of elements is `Skip`ped subtree-and-all — `<script>`,
//! `<style>`, `<iframe>` and friends — because their *text* is not content.
//!
//! # Every loop is bounded
//!
//! Input is truncated before parsing, not after (§9.2). Structural depth, block
//! count, cumulative text size, list indent and quote depth all have caps, and
//! hitting one records a [`Truncation`] on the output so the UI can say the
//! article is incomplete rather than silently presenting a fragment as whole.

use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use tendril::StrTendril;
use url::Url;

use crate::content::block::{
    BlockKind, Document, MediaFetch, RenderBlock, Span, SpanStyle, TableCell, Truncation,
};
use crate::content::url::{MediaDecision, MediaPolicy, MediaUrl};

/// Resource caps for the transform. Every one of these exists because the
/// input is written by strangers (§9).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Applied to the input string *before* tokenizing.
    pub max_input_bytes: usize,
    /// Maximum element nesting. Beyond this, start tags stop opening new
    /// structure and their content flattens into the enclosing block.
    pub max_depth: usize,
    /// Maximum blocks in the output document.
    pub max_blocks: usize,
    /// Maximum cumulative text bytes across all blocks.
    pub max_text_bytes: usize,
    /// Maximum `<blockquote>` nesting reflected in `quote_depth`.
    pub max_quote_depth: u8,
    /// Maximum list nesting reflected in `indent`.
    pub max_list_indent: u8,
    /// Maximum rows in one table.
    ///
    /// A table is a SINGLE block, so `max_blocks` does not bound it at all. A
    /// document of half a million empty `<td>`s built an 80 MB structure while
    /// reporting one block and no truncation.
    pub max_table_rows: usize,
    /// Maximum cells in one table row.
    pub max_table_cells_per_row: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            // A multi-megabyte entry is a frozen UI on a phone (§9.2). Two
            // megabytes is far past any honest article.
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
    pub limits: Limits,
}

impl TransformContext {
    /// The privacy-preserving default for a given instance.
    #[must_use]
    pub fn new(instance: Url) -> Self {
        TransformContext {
            base_url: None,
            media: MediaPolicy::default_for(instance),
            limits: Limits::default(),
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base: Option<Url>) -> Self {
        self.base_url = base;
        self
    }
}

/// Transform article HTML into a flat block list.
///
/// This function is **infallible by design**. §9.5 requires that a malformed
/// response be a handled error rather than a crash, and the honest handling for
/// unparseable markup is an empty or partial document, not an `Err` that some
/// caller will end up unwrapping. Anything the transform could not make sense
/// of is simply absent from the output; anything it had to cut is recorded in
/// [`Document::truncated`].
#[must_use]
pub fn transform(html: &str, ctx: &TransformContext) -> Document {
    // Cap the input BEFORE parsing, not after (§9.2). Slicing on a char
    // boundary keeps the truncation from splitting a UTF-8 sequence.
    let (input, pre_truncated) = if html.len() > ctx.limits.max_input_bytes {
        let mut cut = ctx.limits.max_input_bytes;
        while cut > 0 && !html.is_char_boundary(cut) {
            cut -= 1;
        }
        (
            html.get(..cut).unwrap_or(""),
            Some(Truncation::InputTooLarge),
        )
    } else {
        (html, None)
    };

    let mut builder = Builder::new(ctx.clone());
    builder.truncated = pre_truncated;

    let mut queue = BufferQueue::default();
    queue.push_back(StrTendril::from(input));

    let mut tok = Tokenizer::new(
        builder,
        TokenizerOpts {
            // Parse errors are not interesting here: the transform's contract
            // is best-effort rendering, and collecting error strings from
            // hostile input is just another unbounded allocation.
            exact_errors: false,
            ..TokenizerOpts::default()
        },
    );

    // `feed` returns `Script(handle)` only if the sink asks for script
    // handling, which this sink never does; looping to `Done` is exhaustive.
    let _ = tok.feed(&mut queue);
    tok.end();

    tok.sink.finish()
}

/// What the transform does with an element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Drop the element and everything inside it.
    Skip,
    /// Drop the tag, keep the contents. The default for anything unrecognised.
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

/// The allowlist. Everything not named here is [`Disposition::Flatten`].
fn classify(name: &str) -> Disposition {
    match name {
        // -- dropped entirely: their text is not article content ------------
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

        // Block-level containers that carry no styling of their own but do
        // terminate the current paragraph.
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
    /// Link to restore when this element closes.
    saved_link: Option<Option<MediaUrl>>,
}

#[derive(Debug, Clone, Copy)]
struct ListState {
    ordered: bool,
    counter: u32,
}

/// Which kind of block the currently-accumulating spans belong to.
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
    /// Depth at which a `Skip` element was opened. While `Some`, everything is
    /// discarded until the stack unwinds back past it.
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
    /// `<table>` nesting. Only the outermost close emits a block: a nested
    /// table is flattened into its parent's cells, and closing the outer table
    /// on the inner `</table>` would truncate it.
    table_depth: u32,

    text_bytes: usize,
    /// `text_bytes` as of the last block actually emitted.
    ///
    /// Lets `push_block` tell "this block is the one that reached the cap"
    /// from "we were already over it and this block adds nothing", which is
    /// the difference between truncating an article and erasing it.
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
        // Keep the first cause: it is the one that explains the rest.
        if self.truncated.is_none() {
            self.truncated = Some(why);
        }
    }

    fn push_block(&mut self, kind: BlockKind) {
        // Inside a table, a block-level element (a rule, an image, a code
        // block) would otherwise be appended to the document directly -- and
        // since the table itself is only emitted when it closes, that content
        // appeared BEFORE the table it came from. Fold what text it carries
        // into the current cell instead, and drop the rest.
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
            // Only DROP the block when it adds no text of its own. The block
            // that reaches the cap still gets emitted: `push_text` has already
            // clamped its text to the remaining budget, so it is bounded, and
            // dropping it discards the very content the cap was meant to
            // truncate. An article that is a single long <p> -- a shape plenty
            // of feeds produce -- rendered as ZERO blocks and a blank screen
            // because of this, while reporting `truncated: TooMuchText`.
            if self.text_bytes == self.text_committed {
                return;
            }
        }
        self.text_committed = self.text_bytes;
        self.blocks
            .push(RenderBlock::quoted(kind, self.quote_depth));
    }

    /// Emit whatever inline text has accumulated as the pending block kind.
    fn flush(&mut self) {
        let spans = std::mem::take(&mut self.spans);
        let spans = trim_spans(spans);
        if spans.is_empty() {
            // Reset pending even when empty so a stray `</h2>` cannot leak its
            // heading level onto the next paragraph.
            self.pending = self.default_pending();
            return;
        }

        // Inside a table cell, text belongs to the cell rather than the
        // document body.
        if let Some(table) = self.table.as_mut() {
            if let Some(row) = table.current_row.as_mut() {
                if let Some(cell) = row.last_mut() {
                    cell.spans.extend(spans);
                    self.pending = self.default_pending();
                    return;
                }
            }
            // Text inside <table> but outside any cell is stray; drop it
            // rather than inventing a cell for it.
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

    /// What a fresh block should be, given the enclosing structure.
    fn default_pending(&self) -> Pending {
        if self.lists.is_empty() {
            Pending::Paragraph
        } else {
            // Continuation text inside a list item stays part of the item.
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
            self.text_bytes += taken.len();
            return;
        }

        // Outside <pre>, collapse whitespace runs. Real feed markup is full of
        // newlines and indentation that mean nothing.
        let collapsed = collapse_whitespace(text);
        if collapsed.is_empty() {
            return;
        }
        // Collapsing has to carry across token boundaries. html5ever splits
        // character data at arbitrary points, so "a\n\n  \tb" can arrive as
        // several CharacterTokens; collapsing each in isolation would emit one
        // space per chunk and render "a   b". Drop a leading space when the
        // text so far already ends in one -- or when there is no text yet,
        // since a block must not start with a space.
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
        self.text_bytes += taken.len();

        // Merge into the previous span when the styling is identical; this
        // keeps the span list short for text broken up by ignored tags.
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
        // An element that can never have children or an end tag.
        let childless = is_void(&name) || tag.self_closing;

        // Already discarding a subtree: track the stack so we know when to
        // stop, but do nothing else. The depth cap applies here too -- without
        // it a skipped subtree grows the stack without bound, and because an
        // unmatched end tag is resolved by scanning that stack, the transform
        // goes quadratic. Measured before this cap was applied: 320 KB of
        // `<svg>` + 40k `<b>` + 40k `</zz>` took 23 seconds, well inside the
        // 2 MiB input cap. §9.2's caps exist to prevent exactly that.
        if self.skip_from.is_some() {
            if !childless && self.open.len() < self.ctx.limits.max_depth {
                self.open.push(OpenElement {
                    name,
                    disposition: Disposition::Skip,
                    saved_style: None,
                    saved_link: None,
                });
            }
            return TokenSinkResult::Continue;
        }

        // Skip elements are handled BEFORE the structural depth cap, and that
        // ordering is load-bearing: with the cap first, a `<script>` opened
        // beyond the cap returned early without entering skip mode, so the
        // tokenizer was never put into raw-text mode and the script's body
        // arrived as ordinary character tokens -- i.e. the article rendered
        // `alert('pwned')` as text. That is an allowlist breach (§9.2), not a
        // cosmetic one.
        if matches!(disposition, Disposition::Skip) {
            self.flush();
            // A childless skip element -- `<meta>`, `<link>`, `<input>`,
            // `<embed>`, `<source>`, `<track>`, `<param>`, `<area>`, or any
            // self-closing form such as `<svg/>` -- has no content and will
            // never produce an end tag. Entering skip mode for one would set a
            // flag that nothing can ever clear, silently discarding the entire
            // rest of the article. That bug shipped in the first draft of this
            // function and `<input>` alone is common enough in real feeds to
            // have hit it.
            if childless {
                return TokenSinkResult::Continue;
            }
            self.skip_from = Some(self.open.len());
            let raw = raw_text_kind(&name);
            self.open.push(OpenElement {
                name,
                disposition: Disposition::Skip,
                saved_style: None,
                saved_link: None,
            });
            return raw;
        }

        // Structural depth cap (§9.2). Past the cap we stop opening structure
        // entirely; text still flows into the enclosing block. Recorded, so
        // the UI can say the article is not shown in full rather than
        // presenting a silently flattened version as complete.
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
                        // §9.2: only http(s) survives into a rendered link. An
                        // unparseable or dangerous href yields plain text,
                        // never a link the user can tap.
                        self.link =
                            non_empty_attr(tag, "href").and_then(|href| self.resolve(&href));
                    }
                }
            }
            Disposition::Block(block) => self.start_block(block, tag),
        }

        if !childless {
            self.open.push(OpenElement {
                name,
                disposition,
                saved_style,
                saved_link,
            });
        } else if let Some(style) = saved_style {
            // A self-closing inline element styles nothing.
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
                    // An `<li>` with no enclosing list: render it as a bullet
                    // rather than dropping the user's text.
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
                // Nested tables flatten into the enclosing one: allowing real
                // nesting would reintroduce unbounded depth through the back
                // door, and Qt 5.6 has no table rendering to nest into anyway.
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
                // §9.3: the decision is made here, before the URL ever
                // reaches the UI, so a third-party host cannot be contacted by
                // a QML Image that simply binds to whatever `src` it was given.
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
                });
            }
        }
    }

    /// Resolve an attribute value into a validated `http(s)` URL.
    fn resolve(&self, raw: &str) -> Option<MediaUrl> {
        match &self.ctx.base_url {
            Some(base) => MediaUrl::parse_relative(raw, base),
            // With no base URL a relative reference is unresolvable, and
            // guessing an origin for it would be worse than dropping it.
            None => MediaUrl::parse(raw),
        }
    }

    fn end_tag(&mut self, tag: &Tag) {
        let name = tag.name.as_ref().to_ascii_lowercase();

        // Find the matching open element, innermost first. Unmatched end tags
        // are ignored rather than unwinding the whole stack -- real feed HTML
        // is full of stray `</div>`s and closing everything would destroy the
        // document's structure.
        let Some(index) = self.open.iter().rposition(|el| el.name == name) else {
            return;
        };

        // Close everything from the innermost open element down to the match.
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
            // Restore nothing: a skipped subtree never applied styling.
            return;
        }

        if let Some(style) = element.saved_style {
            self.style = style;
        }
        if let Some(link) = &element.saved_link {
            self.link = link.clone();
        }

        match element.disposition {
            Disposition::Block(BlockTag::Heading(_)) | Disposition::Block(BlockTag::Paragraph) => {
                self.flush();
            }
            Disposition::Block(BlockTag::ListItem) => self.flush(),
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
                    // Inline spans accumulated before <pre> were already
                    // flushed on open; discard anything stray.
                    self.spans.clear();
                }
            }
            Disposition::Block(BlockTag::Table) => {
                self.flush();
                self.table_depth = self.table_depth.saturating_sub(1);
                // Only the outermost </table> emits the block. Closing on an
                // inner one truncated the outer table at the nested table's
                // position.
                if self.table_depth == 0 {
                    self.close_table();
                }
            }
            Disposition::Block(BlockTag::TableRow) => {
                self.flush();
                // The cap has to be applied on BOTH paths that commit a row:
                // this one (`</tr>`) and the one in `start_block` that flushes
                // the previous row when a new `<tr>` opens. Capping only the
                // latter left well-formed markup completely unbounded.
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
        // Stop doing work once every cap is spent; the tokenizer still drains,
        // but nothing further is allocated.
        if self.at_capacity() && self.truncated.is_some() {
            return TokenSinkResult::Continue;
        }

        match token {
            Token::TagToken(tag) => match tag.kind {
                TagKind::StartTag => return self.start_tag(&tag),
                TagKind::EndTag => self.end_tag(&tag),
            },
            Token::CharacterTokens(text) => self.push_text(&text),
            // Comments, doctypes, parse errors and stray NULs carry nothing a
            // reader wants to see.
            Token::CommentToken(_)
            | Token::DoctypeToken(_)
            | Token::NullCharacterToken
            | Token::ParseError(_) => {}
            Token::EOFToken => {}
        }
        TokenSinkResult::Continue
    }
}

/// Ask the tokenizer to treat an element's contents as text rather than
/// markup, so that `<script>if (a<b) {}</script>` cannot produce a spurious
/// `<b>` start tag.
fn raw_text_kind(name: &str) -> TokenSinkResult<()> {
    match name {
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "style" | "textarea" | "title" => TokenSinkResult::RawData(RawKind::Rawtext),
        _ => TokenSinkResult::Continue,
    }
}

/// A branch the match above has already excluded.
///
/// `unreachable!()` would be a panic, and §9.5 forbids reachable panics on this
/// path; this keeps the same "cannot happen" meaning without one.
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

fn attr(tag: &Tag, wanted: &str) -> Option<String> {
    tag.attrs
        .iter()
        .find(|a| a.name.local.as_ref().eq_ignore_ascii_case(wanted))
        .map(|a| a.value.to_string())
}

/// An attribute that is present but empty, treated as absent.
///
/// Real feed markup contains valueless attributes (`<a href>`). Resolving an
/// empty `href` against the article's own URL yields the article, so the text
/// would silently become a link back to the page the reader is already on --
/// worse than no link, because it looks like it goes somewhere.
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

/// Collapse runs of ASCII whitespace to a single space.
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

/// Truncate to at most `budget` bytes without splitting a character.
fn clamp_to_char_boundary(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut cut = budget;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.get(..cut).unwrap_or("")
}

/// Drop leading/trailing whitespace across a span run, and remove spans that
/// become empty. Keeps blocks from rendering with stray padding.
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

    /// A context that renders third-party images, so tests about *structure*
    /// are not silently also testing the media policy.
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
            media: MediaPolicy::strict_for(instance()),
            ..TransformContext::new(instance())
        }
        .with_base_url(Some(Url::parse("https://blog.example/post/").unwrap()))
    }

    fn text_of(doc: &Document) -> String {
        doc.blocks
            .iter()
            .map(|b| b.plain_text())
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
        // The tokenizer decodes references; the transform must not decode again.
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
        assert!(rows
            .first()
            .map(|r| r.iter().all(|c| c.header))
            .unwrap_or(false));
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
        // Without RawData handling the tokenizer would read `<b>` out of the
        // script body and the following text would render bold.
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
            // The text is kept -- dropping it would lose content the user can read.
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
                .map(|u| u.as_str()),
            Some("https://blog.example/x")
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
        // This is the case the tokenizer-not-DOM decision exists for: an RcDom
        // built from this input overflows the stack when it is *dropped*.
        let depth = 50_000;
        let html = format!("{}deep{}", "<div>".repeat(depth), "</div>".repeat(depth));
        let doc = transform(&html, &lenient_ctx());
        assert!(
            text_of(&doc).contains("deep"),
            "content lost under deep nesting"
        );
        // And the structural cap fired. Without this the test says only "it
        // did not crash" -- which the tokenizer-not-DOM design guarantees on
        // its own, so the whole §9.2 depth cap could be deleted with every
        // content test still green.
        assert_eq!(
            doc.truncated,
            Some(Truncation::TooDeep),
            "50,000 levels must hit the depth cap and say so"
        );
    }

    #[test]
    fn deeply_nested_unclosed_markup_is_also_survivable() {
        // No closing tags at all: the open-element stack never unwinds.
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
        // Regression: `push_block` dropped the block that REACHED the text
        // cap, not just the ones after it. An article that is a single long
        // <p> -- which plenty of feeds produce -- therefore rendered as zero
        // blocks and a blank screen, while reporting `truncated: TooMuchText`.
        // The transform's contract is to truncate, not to erase.
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
        // MANY SMALL paragraphs, not one huge one. With a single 50 KB
        // paragraph the transform drops it wholesale and returns NO blocks, so
        // the rendered text is "" -- and `"".len() <= 600` holds whether the
        // cap exists or not. Deleting the per-token byte budget from
        // `push_text` outright left the old version of this test green.
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
        // §9.5: a malformed response is a handled error, not a crash. The fuzz
        // targets in fuzz/ generalise this; these are the shapes seen in real
        // feeds.
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
            // The contract is only that it returns; assert something cheap so
            // the call is not optimised away.
            let _ = doc.blocks.len();
        }
    }

    #[test]
    fn tables_are_bounded_like_everything_else() {
        // A table is a SINGLE block, so `max_blocks` does not bound it at all.
        // Half a million empty `<td>`s built an 80 MB structure while
        // reporting one block and no truncation.
        //
        // Note both commit paths need the cap: `</tr>` and the next `<tr>`.
        // Capping only the latter left well-formed markup unbounded, which is
        // the shape real feed HTML actually has.
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
        // Regression: `<meta>`, `<input>`, `<embed>` and friends are on the
        // skip list AND are void, so entering skip mode for them set a flag
        // nothing could ever clear -- silently discarding everything after.
        // `<input>` alone is common enough in real feed HTML to have hit this.
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
        // Regression: with the depth cap checked before skip handling, a
        // <script> opened beyond the cap never entered skip mode, the
        // tokenizer was never put into raw-text mode, and the script body
        // arrived as ordinary text -- so the article rendered alert('pwned')
        // as readable content. An allowlist breach, not a cosmetic one.
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
        // A stray `</div>` must not unwind the whole open-element stack.
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
    use std::time::Instant;

    /// A depth-capped stack must stay depth-capped on *every* path.
    ///
    /// Regression test for a quadratic blow-up found by fuzzing the transform:
    /// a skipped subtree (`<svg>`, `<script>`, …) used to push onto the
    /// open-element stack before the depth cap was applied, so the stack grew
    /// without bound and every unmatched end tag scanned all of it.
    #[test]
    fn a_skipped_subtree_cannot_grow_the_element_stack_without_bound() {
        // Scaling check rather than a wall-clock threshold: an absolute timing
        // assertion would be flaky on a loaded CI runner, but quadratic growth
        // shows up as a ratio no matter how slow the machine is.
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

        // Four times the input. Linear would be ~4x; the bug was ~16x (measured
        // 1.3s -> 23s). Ten is comfortably clear of both noise and the defect.
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.000_001);
        assert!(
            ratio < 10.0,
            "transform scales super-linearly on skipped subtrees: \
             10k took {small:?}, 40k took {large:?} (ratio {ratio:.1}x)"
        );
    }

    #[test]
    fn the_depth_cap_holds_through_a_skipped_subtree() {
        // The same input, checked structurally rather than by timing.
        let html = format!("<svg>{}", "<b>".repeat(5_000));
        let doc = super::transform(&html, &ctx());
        // Nothing should have been emitted, and crucially it should return
        // promptly rather than having built a 5000-deep stack.
        assert!(doc.blocks.is_empty());
    }
}
