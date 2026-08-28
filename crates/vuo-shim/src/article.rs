//! The article view's model: one render block per row.
//!
//! §5: *article HTML is transformed into a block list in Rust [...] which gives
//! control over rendering, lazy image loading, and font scaling — and keeps the
//! QML dumb.* This is where "keeps the QML dumb" is cashed in: the QML delegate
//! switches on `blockKind` and draws, and does no parsing, no sanitising and no
//! string manipulation of foreign data.
//!
//! # The one place markup crosses into QML
//!
//! `ROLE_TEXT` carries Qt `StyledText` produced by
//! [`vuo_core::content::Span::render_styled_text`], which escapes every
//! character of foreign text and emits a closed tag set. That function is the
//! whole of §9.3's guarantee for body text, and it is fuzzed.
//!
//! Everything else — `ROLE_IMAGE_ALT`, code block contents, table cells — is
//! plain text, and the QML **must** set `textFormat` explicitly on each. §9.3:
//! *set `textFormat` explicitly everywhere; never leave it to the default.*

use std::collections::HashMap;

use qmetaobject::*;
use vuo_core::content::{BlockKind, MediaFetch, RenderBlock, Span, TransformContext};

pub const ROLE_KIND: i32 = USER_ROLE;
pub const ROLE_TEXT: i32 = USER_ROLE + 1;
pub const ROLE_LEVEL: i32 = USER_ROLE + 2;
pub const ROLE_QUOTE_DEPTH: i32 = USER_ROLE + 3;
pub const ROLE_ORDERED: i32 = USER_ROLE + 4;
pub const ROLE_MARKER: i32 = USER_ROLE + 5;
pub const ROLE_INDENT: i32 = USER_ROLE + 6;
pub const ROLE_IMAGE_SRC: i32 = USER_ROLE + 7;
pub const ROLE_IMAGE_ALT: i32 = USER_ROLE + 8;
pub const ROLE_NEEDS_CONSENT: i32 = USER_ROLE + 9;
pub const ROLE_CODE_LANGUAGE: i32 = USER_ROLE + 10;
pub const ROLE_PLAIN_TEXT: i32 = USER_ROLE + 11;

/// A flattened block, ready for a delegate.
#[derive(Debug, Clone, Default)]
pub struct BlockRow {
    /// One of: `heading`, `paragraph`, `list_item`, `code`, `image`, `table`,
    /// `rule`. A string rather than an enum because this `qmetaobject` version
    /// has no `qml_register_enum` on Qt 5.6.
    pub kind: String,
    /// Qt StyledText for text blocks; empty otherwise.
    pub text: String,
    /// The same content with no markup at all, for accessibility and search.
    pub plain: String,
    pub level: i32,
    pub quote_depth: i32,
    pub ordered: bool,
    pub marker: String,
    pub indent: i32,
    pub image_src: String,
    pub image_alt: String,
    /// True when the image is third-party and un-proxied: the delegate shows a
    /// tap-to-load placeholder naming the host and fetches nothing (§9.3).
    pub needs_consent: bool,
    pub code_language: String,
}

fn row_for(block: &RenderBlock) -> BlockRow {
    let mut row = BlockRow { quote_depth: i32::from(block.quote_depth), ..BlockRow::default() };
    row.plain = block.plain_text();

    match &block.kind {
        BlockKind::Heading { level, spans } => {
            row.kind = "heading".to_owned();
            row.level = i32::from(*level);
            row.text = Span::render_styled_text(spans);
        }
        BlockKind::Paragraph { spans } => {
            row.kind = "paragraph".to_owned();
            row.text = Span::render_styled_text(spans);
        }
        BlockKind::ListItem { ordered, number, indent, spans } => {
            row.kind = "list_item".to_owned();
            row.ordered = *ordered;
            row.indent = i32::from(*indent);
            row.marker = match number {
                Some(n) => format!("{n}."),
                None => "\u{2022}".to_owned(),
            };
            row.text = Span::render_styled_text(spans);
        }
        BlockKind::Code { language, text } => {
            row.kind = "code".to_owned();
            // Code is plain text by definition; the delegate uses a monospace
            // font and Text.PlainText.
            row.text = text.clone();
            row.code_language = language.clone().unwrap_or_default();
        }
        BlockKind::Image { src, alt, fetch, .. } => {
            row.kind = "image".to_owned();
            row.image_src = src.as_str().to_owned();
            row.image_alt = alt.clone();
            row.needs_consent = matches!(fetch, MediaFetch::NeedsConsent);
        }
        BlockKind::Table { rows } => {
            row.kind = "table".to_owned();
            // Tables render as pre-formatted plain text. Qt 5.6's Text has no
            // usable table support, and a QML-side grid built from foreign
            // data is a lot of machinery for something that appears in a small
            // fraction of articles.
            let mut out = String::new();
            for cells in rows {
                for (i, cell) in cells.iter().enumerate() {
                    if i > 0 {
                        out.push('\t');
                    }
                    out.push_str(&Span::render_plain_text(&cell.spans));
                }
                out.push('\n');
            }
            row.text = out;
        }
        BlockKind::Rule => row.kind = "rule".to_owned(),
        // BlockKind is #[non_exhaustive]: a future core release can add a block
        // type this shim predates. Rendering nothing is the right default --
        // the alternative is a shim that fails to build against a newer core
        // for a block it could simply have skipped.
        _ => row.kind = "unsupported".to_owned(),
    }
    row
}

#[derive(QObject, Default)]
pub struct ArticleModel {
    base: qt_base_class!(trait QAbstractListModel),

    count: qt_property!(i32; READ row_count NOTIFY count_changed),
    count_changed: qt_signal!(),

    /// True when the transform had to cut the article short. The UI says so
    /// rather than presenting a fragment as the whole thing.
    truncated: qt_property!(bool; NOTIFY truncated_changed),
    truncated_changed: qt_signal!(),

    /// How many images are held back awaiting consent, so the UI can offer a
    /// single "load images from example.com" affordance rather than N.
    blocked_images: qt_property!(i32; NOTIFY blocked_images_changed),
    blocked_images_changed: qt_signal!(),

    clear: qt_method!(fn(&mut self)),

    rows: Vec<BlockRow>,
}

impl ArticleModel {
    /// Transform an article's HTML and show it.
    pub fn set_html(&mut self, html: &str, ctx: &TransformContext) {
        let document = vuo_core::content::transform(html, ctx);
        let rows: Vec<BlockRow> = document.blocks.iter().map(row_for).collect();
        let blocked = rows.iter().filter(|r| r.needs_consent).count();

        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows = rows;
        (self as &mut dyn QAbstractListModel).end_reset_model();

        self.truncated = document.truncated.is_some();
        self.blocked_images = i32::try_from(blocked).unwrap_or(i32::MAX);
        self.count_changed();
        self.truncated_changed();
        self.blocked_images_changed();
    }

    fn clear(&mut self) {
        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows.clear();
        (self as &mut dyn QAbstractListModel).end_reset_model();
        self.count_changed();
    }

    #[must_use]
    pub fn rows(&self) -> &[BlockRow] {
        &self.rows
    }

    /// Whether the transform had to cut the article short.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// How many images are held back awaiting the user's consent.
    #[must_use]
    pub fn blocked_image_count(&self) -> i32 {
        self.blocked_images
    }
}

impl QAbstractListModel for ArticleModel {
    fn row_count(&self) -> i32 {
        i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
    }

    fn data(&self, index: QModelIndex, role: i32) -> QVariant {
        let Ok(i) = usize::try_from(index.row()) else { return QVariant::default() };
        let Some(row) = self.rows.get(i) else { return QVariant::default() };
        match role {
            ROLE_KIND => QString::from(row.kind.clone()).into(),
            ROLE_TEXT => QString::from(row.text.clone()).into(),
            ROLE_PLAIN_TEXT => QString::from(row.plain.clone()).into(),
            ROLE_LEVEL => row.level.into(),
            ROLE_QUOTE_DEPTH => row.quote_depth.into(),
            ROLE_ORDERED => row.ordered.into(),
            ROLE_MARKER => QString::from(row.marker.clone()).into(),
            ROLE_INDENT => row.indent.into(),
            ROLE_IMAGE_SRC => QString::from(row.image_src.clone()).into(),
            ROLE_IMAGE_ALT => QString::from(row.image_alt.clone()).into(),
            ROLE_NEEDS_CONSENT => row.needs_consent.into(),
            ROLE_CODE_LANGUAGE => QString::from(row.code_language.clone()).into(),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> HashMap<i32, QByteArray> {
        let mut names = HashMap::new();
        names.insert(ROLE_KIND, "blockKind".into());
        names.insert(ROLE_TEXT, "styledText".into());
        names.insert(ROLE_PLAIN_TEXT, "plainText".into());
        names.insert(ROLE_LEVEL, "level".into());
        names.insert(ROLE_QUOTE_DEPTH, "quoteDepth".into());
        names.insert(ROLE_ORDERED, "ordered".into());
        names.insert(ROLE_MARKER, "marker".into());
        names.insert(ROLE_INDENT, "indent".into());
        names.insert(ROLE_IMAGE_SRC, "imageSource".into());
        names.insert(ROLE_IMAGE_ALT, "imageAlt".into());
        names.insert(ROLE_NEEDS_CONSENT, "needsConsent".into());
        names.insert(ROLE_CODE_LANGUAGE, "codeLanguage".into());
        names
    }
}
