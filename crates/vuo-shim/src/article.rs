//! Article view model: one render block per row.
//!
//! §5: *article HTML is transformed into a block list in Rust [...] which gives
//! control over rendering, lazy image loading, and font scaling — and keeps the
//! QML dumb.* Cashed in here: QML delegate switches on `blockKind` and draws;
//! no parsing, sanitising, or string work on foreign data.
//!
//! # One place markup crosses into QML
//!
//! `ROLE_TEXT` carries Qt `StyledText` from
//! [`vuo_core::content::Span::render_styled_text`]: escapes every char of foreign
//! text, emits closed tag set. Whole of §9.3's guarantee for body text; fuzzed.
//!
//! Everything else (`ROLE_IMAGE_ALT`, code block contents, table cells) plain
//! text; QML **must** set `textFormat` explicitly on each. §9.3: *set
//! `textFormat` explicitly everywhere; never leave it to the default.*

use std::collections::HashMap;

use qmetaobject::{
    qt_base_class, qt_method, qt_property, qt_signal, QAbstractListModel, QByteArray, QModelIndex,
    QObject, QString, QVariant, USER_ROLE,
};
use vuo_core::content::{
    BlockKind, MediaFetch, MediaPolicy, RenderBlock, Span, TransformContext, UnproxiedMedia,
};
use vuo_core::db::store;
use vuo_core::model::{EntryId, EntryStatus};

use crate::context::AppContext;
use crate::worker::Command;

/// Model roles, one per [`BlockRow`] field; [`ArticleModel`]'s `role_names`
/// gives QML names.
///
/// `blockKind`: [`BlockRow::kind`].
pub const ROLE_KIND: i32 = USER_ROLE;
/// `styledText`: [`BlockRow::text`], the only role that carries markup.
pub const ROLE_TEXT: i32 = USER_ROLE + 1;
/// `level`: [`BlockRow::level`].
pub const ROLE_LEVEL: i32 = USER_ROLE + 2;
/// `quoteDepth`: [`BlockRow::quote_depth`].
pub const ROLE_QUOTE_DEPTH: i32 = USER_ROLE + 3;
/// `ordered`: [`BlockRow::ordered`].
pub const ROLE_ORDERED: i32 = USER_ROLE + 4;
/// `marker`: [`BlockRow::marker`].
pub const ROLE_MARKER: i32 = USER_ROLE + 5;
/// `indent`: [`BlockRow::indent`].
pub const ROLE_INDENT: i32 = USER_ROLE + 6;
/// `imageSource`: [`BlockRow::image_src`].
pub const ROLE_IMAGE_SRC: i32 = USER_ROLE + 7;
/// `imageAlt`: [`BlockRow::image_alt`] (plain text).
pub const ROLE_IMAGE_ALT: i32 = USER_ROLE + 8;
/// `needsConsent`: [`BlockRow::needs_consent`].
pub const ROLE_NEEDS_CONSENT: i32 = USER_ROLE + 9;
/// `codeLanguage`: [`BlockRow::code_language`].
pub const ROLE_CODE_LANGUAGE: i32 = USER_ROLE + 10;
/// `plainText`: [`BlockRow::plain`].
pub const ROLE_PLAIN_TEXT: i32 = USER_ROLE + 11;
/// `imageHost`: [`BlockRow::image_host`].
pub const ROLE_IMAGE_HOST: i32 = USER_ROLE + 12;
/// `imageRatio`: [`BlockRow::image_ratio`].
pub const ROLE_IMAGE_RATIO: i32 = USER_ROLE + 13;
/// `imageLink`: [`BlockRow::image_link`].
pub const ROLE_IMAGE_LINK: i32 = USER_ROLE + 14;

/// No scrape has finished (or the last one was acknowledged).
pub const FETCH_IDLE: i32 = 0;
/// The scrape landed and the article now shows the scraped body.
pub const FETCH_OK: i32 = 1;
/// The server answered with an empty body. The stored article is untouched.
pub const FETCH_EMPTY: i32 = 2;
/// The scrape returned exactly what was already stored.
pub const FETCH_UNCHANGED: i32 = 3;
/// The scrape failed. `fetchMessage` carries the server's text (plain text).
pub const FETCH_FAILED: i32 = 4;
/// Server rejected API key. `fetchMessage` empty by design; page supplies own
/// translated line (see `Notice::SyncFailed`).
pub const FETCH_AUTH: i32 = 5;

/// A flattened block, ready for a delegate.
#[derive(Debug, Clone, Default)]
pub struct BlockRow {
    /// One of: `heading`, `paragraph`, `list_item`, `code`, `image`, `table`,
    /// `rule`. String, not enum: this `qmetaobject` has no `qml_register_enum`
    /// on Qt 5.6.
    pub kind: String,
    /// Qt `StyledText` for text blocks; empty otherwise.
    pub text: String,
    /// The same content with no markup at all, for accessibility and search.
    pub plain: String,
    /// A heading's level, from the source tag; 0 for other blocks.
    pub level: i32,
    /// How many block quotes the block sits inside.
    pub quote_depth: i32,
    /// Whether a list item belongs to a numbered list.
    pub ordered: bool,
    /// A list item's marker: `"3."` for a numbered item, a bullet otherwise.
    pub marker: String,
    /// A list item's nesting depth.
    pub indent: i32,
    /// The image's URL, as the core resolved it.
    pub image_src: String,
    /// The image's alt text (foreign text, plain).
    pub image_alt: String,
    /// Host image would be fetched from. Shown in consent placeholder: "load
    /// images" = informed choice.
    pub image_host: String,
    /// True when image third-party and un-proxied: delegate shows tap-to-load
    /// placeholder naming host, fetches nothing (§9.3).
    pub needs_consent: bool,
    /// height / width from `<img>` tag's own attributes, clamped to
    /// [`MAX_IMAGE_RATIO`]; 0 when tag gave none usable.
    ///
    /// Ratio, not pair: delegate only reserves space, renders at own width. 0 =
    /// "no hint": delegate falls back to fixed placeholder, never collapses row
    /// (else article re-flows under reader when image lands).
    pub image_ratio: f64,
    /// Where tap on image goes when feed wrapped it in link; else empty.
    /// Unlinked image opens zoomable viewer; tap on linked one keeps meaning
    /// "go there". Validated `http`/`https` in core like every link.
    pub image_link: String,
    /// A code block's declared language; empty when it gave none.
    pub code_language: String,
}

/// Tallest shape one image may reserve, as multiple of own width.
///
/// Delegate turns this into height: `width * ratio`. Core caps each `<img>`
/// dimension at 20 000 px, not RATIO: `width="1" height="20000"` would ask for
/// 21 million px on 1080 px screen; reservation stands until image `Ready`, so
/// image never arriving leaves article unscrollable all session.
///
/// Four: past any real photo and tall infographics feeds publish. Beyond, wrong
/// either way; wrong by a screen beats wrong by four hundred.
pub const MAX_IMAGE_RATIO: f64 = 4.0;

/// `height / width` from `<img>` attributes, safe to multiply screen width by.
///
/// Rejects non-ratios: zero width (divides to infinity) and NaN; `width * NaN`
/// = block with no height that never reflows.
fn image_ratio((w, h): (u32, u32)) -> f64 {
    let ratio = f64::from(h) / f64::from(w);
    if !ratio.is_finite() || ratio <= 0.0 {
        return 0.0;
    }
    ratio.min(MAX_IMAGE_RATIO)
}

fn row_for(block: &RenderBlock) -> BlockRow {
    let mut row = BlockRow {
        quote_depth: i32::from(block.quote_depth),
        ..BlockRow::default()
    };
    row.plain = block.plain_text();

    let kind = match &block.kind {
        BlockKind::Heading { level, spans } => {
            row.level = i32::from(*level);
            row.text = Span::render_styled_text(spans);
            "heading"
        }
        BlockKind::Paragraph { spans } => {
            row.text = Span::render_styled_text(spans);
            "paragraph"
        }
        BlockKind::ListItem {
            ordered,
            number,
            indent,
            spans,
        } => {
            row.ordered = *ordered;
            row.indent = i32::from(*indent);
            row.marker = match number {
                Some(n) => format!("{n}."),
                None => "\u{2022}".to_owned(),
            };
            row.text = Span::render_styled_text(spans);
            "list_item"
        }
        BlockKind::Code { language, text } => {
            // Code plain text by definition; delegate uses monospace,
            // Text.PlainText.
            row.text.clone_from(text);
            row.code_language = language.clone().unwrap_or_default();
            "code"
        }
        BlockKind::Image {
            src,
            alt,
            fetch,
            intrinsic,
            link,
            ..
        } => {
            src.as_str().clone_into(&mut row.image_src);
            row.image_alt.clone_from(alt);
            src.as_url()
                .host_str()
                .unwrap_or_default()
                .clone_into(&mut row.image_host);
            row.needs_consent = matches!(fetch, MediaFetch::NeedsConsent);
            row.image_ratio = intrinsic.map_or(0.0, image_ratio);
            row.image_link = link
                .as_ref()
                .map(|l| l.as_str().to_owned())
                .unwrap_or_default();
            "image"
        }
        BlockKind::Table { rows } => {
            // Tables render as pre-formatted plain text. Qt 5.6 Text has no
            // usable table support; QML grid from foreign data = much machinery
            // for small fraction of articles.
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
            "table"
        }
        BlockKind::Rule => "rule",
        // BlockKind #[non_exhaustive]: future core can add block type shim
        // predates. Render nothing; else shim fails to build against newer core
        // over skippable block.
        _ => "unsupported",
    };
    kind.clone_into(&mut row.kind);
    row
}

/// Article view list model: open entry's [`BlockRow`]s, plus read, starred,
/// fetch-original state. See module docs.
// Bools are `qt_property!`s, each own setting/state QML binds by name; bitfield
// hides them from QML.
#[allow(clippy::struct_excessive_bools)]
#[derive(QObject, Default)]
pub struct ArticleModel {
    base: qt_base_class!(trait QAbstractListModel),

    count: qt_property!(i32; READ row_count NOTIFY countChanged),
    countChanged: qt_signal!(),

    /// True when transform cut article short. UI says so; fragment not passed
    /// off as whole.
    pub truncated: qt_property!(bool; NOTIFY truncatedChanged),
    truncatedChanged: qt_signal!(),

    /// Images held back awaiting consent: UI offers one "load images from
    /// example.com", not N.
    pub blockedImages: qt_property!(i32; NOTIFY blockedImagesChanged),
    blockedImagesChanged: qt_signal!(),

    /// Whether open article is read.
    ///
    /// Article view shows and changes read and starred state itself, not only
    /// via entry list's context menu.
    pub isRead: qt_property!(bool; NOTIFY entryStateChanged),
    /// Whether the open article is starred (see `isRead`).
    pub isStarred: qt_property!(bool; NOTIFY entryStateChanged),
    /// Article's own link; empty when none.
    ///
    /// Property, not only `openInBrowser` return: page BINDS to it (site page
    /// attached right of article exists exactly when non-empty). Validated
    /// http(s) by core before storing: handing to `WebView` or system browser
    /// can't launch anything else.
    pub articleUrl: qt_property!(QString; NOTIFY entryStateChanged),
    entryStateChanged: qt_signal!(),

    clear: qt_method!(fn(&mut self)),
    /// Load entry from mirror, transform stored HTML.
    ///
    /// Local read: opening article works without network.
    load: qt_method!(fn(&mut self, entry_id: i64)),
    /// Hand the article's own URL to the system browser.
    openInBrowser: qt_method!(fn(&self) -> QString),
    /// Ask the *server* to scrape the original page (§3: no local Readability).
    fetchOriginal: qt_method!(fn(&mut self)),
    /// Consent to images from origin of block at `row`, for this article.
    /// Re-transforms: placeholders become images.
    allowImagesFrom: qt_method!(fn(&mut self, row: i32)),
    /// Re-read open article when worker changed mirror.
    ///
    /// Without poll, "Fetch original content" scrape lands in SQLite but OPEN
    /// page never re-reads it. Same pattern: `EntryModel::pollSync`.
    pollSync: qt_method!(fn(&mut self) -> bool),
    /// True while a scrape is in flight, so the page can say something.
    pub fetching: qt_property!(bool; NOTIFY fetchingChanged),
    fetchingChanged: qt_signal!(),
    /// How the last scrape ended: one of the `FETCH_*` constants.
    pub fetchStatus: qt_property!(i32; NOTIFY fetchStatusChanged),
    /// Foreign text explaining failure; else empty. Render as `Text.PlainText`
    /// (§9.3).
    pub fetchMessage: qt_property!(QString; NOTIFY fetchStatusChanged),
    fetchStatusChanged: qt_signal!(),
    /// Acknowledge the last scrape result, so its banner can be dismissed.
    clearFetchStatus: qt_method!(fn(&mut self)),
    /// How long open article stays on screen before counting read: -1 never, 0
    /// at once, else ms.
    ///
    /// Milliseconds: page drives QML `Timer` with it directly.
    pub markReadDelayMs: qt_property!(i32; READ mark_read_delay_ms NOTIFY entryStateChanged),
    /// Mark open article read. Idempotent, never toggle.
    ///
    /// Not `toggleRead`: fires against whatever is open; toggle would mark read
    /// article UNREAD (re-opening resurrects it).
    markRead: qt_method!(fn(&mut self)),
    /// Flip read/unread, flip starred, for open article.
    ///
    /// Local first, enqueued, like entry list toggles: mirror = source of
    /// truth, outbox carries intent to server when network exists.
    toggleRead: qt_method!(fn(&mut self)),
    toggleStarred: qt_method!(fn(&mut self)),

    rows: Vec<BlockRow>,
    entry_id: i64,
    article_url: String,
    /// Origins the user has agreed to for this article view.
    consented: Vec<url::Url>,
    ctx: Option<std::rc::Rc<AppContext>>,
}

impl ArticleModel {
    /// Give model context explicitly. Tests only; QML-built model uses
    /// installed context.
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
    }

    /// App context: explicitly attached (tests), else global.
    fn context(&self) -> Option<std::rc::Rc<AppContext>> {
        self.ctx.clone().or_else(crate::context::current)
    }

    fn load(&mut self, entry_id: i64) {
        let Some(ctx) = self.context() else { return };
        self.entry_id = entry_id;
        self.consented.clear();

        let Some(Some(entry)) =
            ctx.read(|db| store::entry(db.conn(), EntryId(entry_id)).ok().flatten())
        else {
            self.clear();
            return;
        };
        self.article_url = entry
            .url
            .as_ref()
            .map(|u| u.as_str().to_owned())
            .unwrap_or_default();
        self.articleUrl = QString::from(self.article_url.clone());
        self.isRead = entry.status == EntryStatus::Read;
        self.isStarred = entry.starred;
        self.entryStateChanged();

        let stored_consent = ctx.read(|db| store::media_consent(db.conn()).unwrap_or_default());
        self.consented = stored_consent.unwrap_or_default();

        let content = entry.content;
        let ctx_for_transform = self.transform_context(&ctx);
        self.set_html(&content, &ctx_for_transform);
    }

    /// Build the transform context, honouring consent already granted.
    fn transform_context(&self, ctx: &AppContext) -> TransformContext {
        TransformContext {
            base_url: url::Url::parse(&self.article_url).ok(),
            media: MediaPolicy::ProxyThroughInstance {
                instance: ctx.instance().clone(),
                // Consented origins folded in as trusted: transform yields
                // Fetch, not NeedsConsent.
                extra_trusted: self.consented.clone(),
                // User's Images setting, not hardcoded default: Strict and
                // Allow must reach transform (§9.3).
                fallback: match ctx.media_policy() {
                    crate::settings::MEDIA_STRICT => UnproxiedMedia::Strict,
                    crate::settings::MEDIA_ALLOW => UnproxiedMedia::Allow,
                    _ => UnproxiedMedia::Ask,
                },
            },
            limits: vuo_core::content::Limits::default(),
        }
    }

    fn openInBrowser(&self) -> QString {
        // Returns URL, launches nothing: QML owns Qt.openUrlExternally; crate
        // free of platform launching. Empty = no link.
        QString::from(self.article_url.clone())
    }

    fn fetchOriginal(&mut self) {
        let Some(ctx) = self.context() else { return };
        if self.entry_id == 0 {
            return;
        }
        if ctx.send(Command::FetchOriginal {
            entry_id: self.entry_id,
        }) {
            self.fetching = true;
            self.fetchingChanged();
            self.fetchStatus = FETCH_IDLE;
            self.fetchMessage = QString::from("");
            self.fetchStatusChanged();
        }
    }

    fn pollSync(&mut self) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };
        let open = self.entry_id;
        if open == 0 {
            return false;
        }
        let Some(outcome) = ctx.signal().take_fetch_outcome(open) else {
            // Scrape resolved only by own outcome. Reacting to generation
            // counter would let mark-read-after-N-seconds bump clear `fetching`
            // and re-read mirror before scrape wrote.
            return false;
        };

        if self.fetching {
            self.fetching = false;
            self.fetchingChanged();
        }
        self.fetchStatus = outcome.status;
        self.fetchMessage = QString::from(outcome.message);
        self.fetchStatusChanged();

        // Only stored body changes screen. Re-load for empty or unchanged
        // scrape resets block list (and scroll position) to show same thing.
        if outcome.status == FETCH_OK {
            self.load(open);
        }
        true
    }

    fn clearFetchStatus(&mut self) {
        if self.fetchStatus != FETCH_IDLE {
            self.fetchStatus = FETCH_IDLE;
            self.fetchMessage = QString::from("");
            self.fetchStatusChanged();
        }
    }

    /// Grant consent for ORIGIN of image at `row`.
    ///
    /// That origin only, not third-party media generally: §9.3's point =
    /// agreeing to one host ≠ agreeing to every host feed references.
    fn allowImagesFrom(&mut self, row: i32) {
        let Some(ctx) = self.context() else { return };
        let Some(origin) = usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .and_then(|r| url::Url::parse(&r.image_src).ok())
            .and_then(|u| u.join("/").ok())
        else {
            return;
        };

        if !self.consented.contains(&origin) {
            self.consented.push(origin.clone());
        }
        // Remember across articles: agreeing to host once shouldn't repeat per
        // entry from same site.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        ctx.write(|db| db.with_tx(|tx| store::grant_media_consent(tx, origin.as_str(), now)));

        // Re-transform: placeholders become images.
        let entry_id = self.entry_id;
        self.load(entry_id);
    }

    /// Transform an article's HTML and show it.
    pub fn set_html(&mut self, html: &str, ctx: &TransformContext) {
        let document = vuo_core::content::transform(html, ctx);
        let rows: Vec<BlockRow> = document.blocks.iter().map(row_for).collect();
        let blocked = rows.iter().filter(|r| r.needs_consent).count();

        QAbstractListModel::begin_reset_model(self);
        self.rows = rows;
        QAbstractListModel::end_reset_model(self);

        self.truncated = document.truncated.is_some();
        self.blockedImages = i32::try_from(blocked).unwrap_or(i32::MAX);
        self.countChanged();
        self.truncatedChanged();
        self.blockedImagesChanged();
    }

    fn mark_read_delay_ms(&self) -> i32 {
        let index = self
            .context()
            .map_or(crate::settings::MARK_READ_NEVER, |ctx| {
                ctx.mark_read_delay_index()
            });
        match crate::settings::mark_read_delay_seconds(index) {
            None => -1,
            Some(seconds) => seconds.saturating_mul(1000),
        }
    }

    fn markRead(&mut self) {
        if self.isRead {
            return;
        }
        if self.apply_local(|db, id| crate::worker::apply_local_status(db, id, EntryStatus::Read)) {
            self.isRead = true;
            self.entryStateChanged();
        }
    }

    fn toggleRead(&mut self) {
        let want_read = !self.isRead;
        let status = if want_read {
            EntryStatus::Read
        } else {
            EntryStatus::Unread
        };
        if self.apply_local(|db, id| crate::worker::apply_local_status(db, id, status)) {
            self.isRead = want_read;
            self.entryStateChanged();
        }
    }

    fn toggleStarred(&mut self) {
        let want_starred = !self.isStarred;
        if self.apply_local(|db, id| crate::worker::apply_local_starred(db, id, want_starred)) {
            self.isStarred = want_starred;
            self.entryStateChanged();
        }
    }

    /// Queue local mutation for open entry, nudge outbox.
    ///
    /// `false` when no context, no entry open, or write refused: caller leaves
    /// displayed state alone, doesn't show change that didn't happen.
    fn apply_local(
        &self,
        write: impl FnOnce(&mut vuo_core::db::Database, EntryId) -> vuo_core::Result<()>,
    ) -> bool {
        if self.entry_id == 0 {
            return false;
        }
        let Some(ctx) = self.context() else {
            return false;
        };
        let applied = ctx
            .write(|db| write(db, EntryId(self.entry_id)))
            .transpose()
            .ok()
            .flatten()
            .is_some();
        if applied {
            // Mirror changed, say so: entry list row and cover's unread badge
            // read from it. Else list shows read article unread, cover count
            // stale until something reloads.
            //
            // Safe against scroll reset above: model re-reads only for scrape
            // it asked for; own bump doesn't move reader.
            ctx.signal().bump();
            // Opportunistic: offline, intent waits in outbox; next sync carries
            // it.
            ctx.send(Command::FlushOutbox);
        }
        applied
    }

    fn clear(&mut self) {
        QAbstractListModel::begin_reset_model(self);
        self.rows.clear();
        QAbstractListModel::end_reset_model(self);
        self.countChanged();
        // Forget entry, else next `clear`-then-toggle mutates article no longer
        // open.
        self.entry_id = 0;
        self.article_url = String::new();
        self.articleUrl = QString::from(String::new());
        self.isRead = false;
        self.isStarred = false;
        self.entryStateChanged();
        if self.fetching {
            self.fetching = false;
            self.fetchingChanged();
        }
        self.clearFetchStatus();
    }

    /// The open article's blocks, in display order.
    #[must_use]
    pub fn rows(&self) -> &[BlockRow] {
        &self.rows
    }
}

impl QAbstractListModel for ArticleModel {
    fn row_count(&self) -> i32 {
        i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
    }

    fn data(&self, index: QModelIndex, role: i32) -> QVariant {
        let Ok(i) = usize::try_from(index.row()) else {
            return QVariant::default();
        };
        let Some(row) = self.rows.get(i) else {
            return QVariant::default();
        };
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
            ROLE_IMAGE_HOST => QString::from(row.image_host.clone()).into(),
            ROLE_NEEDS_CONSENT => row.needs_consent.into(),
            ROLE_IMAGE_RATIO => row.image_ratio.into(),
            ROLE_IMAGE_LINK => QString::from(row.image_link.as_str()).into(),
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
        names.insert(ROLE_IMAGE_HOST, "imageHost".into());
        names.insert(ROLE_NEEDS_CONSENT, "needsConsent".into());
        names.insert(ROLE_IMAGE_RATIO, "imageRatio".into());
        names.insert(ROLE_IMAGE_LINK, "imageLink".into());
        names.insert(ROLE_CODE_LANGUAGE, "codeLanguage".into());
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §an `<img>` cannot reserve more of the page than it could fill.
    ///
    /// Delegate multiplies screen width by this: one number here a feed hands
    /// straight into layout. Core caps each dimension at 20 000 px, not ratio:
    /// `width="1" height="20000"` = 21 million px on 1080 px screen;
    /// reservation stands until `Ready`, so never-arriving image leaves article
    /// unscrollable all session.
    #[test]
    fn an_images_reserved_shape_is_bounded_whatever_the_tag_claims() {
        assert_eq!(
            image_ratio((2, 1)),
            0.5,
            "an ordinary landscape passes through"
        );
        assert_eq!(image_ratio((1, 2)), 2.0, "and so does an ordinary portrait");
        assert_eq!(
            image_ratio((1, 20_000)),
            MAX_IMAGE_RATIO,
            "the tallest thing the core will accept is clamped, not honoured"
        );
        // Zero width divides to infinity; `width * inf` = no finite height.
        assert_eq!(image_ratio((0, 100)), 0.0, "no width is no hint");
        assert_eq!(image_ratio((100, 0)), 0.0, "and neither is no height");
    }

    /// §a tap on an image zooms it, unless the image is a link.
    ///
    /// Delegate decides on `imageLink` alone: non-empty opens link, empty opens
    /// viewer. Row carries link core found, nothing when none.
    #[test]
    fn an_image_row_carries_the_link_it_sits_in() {
        let instance = url::Url::parse("https://miniflux.example/").expect("url");
        let ctx = TransformContext {
            media: MediaPolicy::ProxyThroughInstance {
                instance: instance.clone(),
                extra_trusted: Vec::new(),
                fallback: UnproxiedMedia::Allow,
            },
            ..TransformContext::new(instance)
        };
        let doc = vuo_core::content::transform(
            r#"<a href="https://site.example/big"><img src="https://miniflux.example/a.png"></a>
               <p><img src="https://miniflux.example/b.png"></p>"#,
            &ctx,
        );
        let images: Vec<BlockRow> = doc
            .blocks
            .iter()
            .map(row_for)
            .filter(|r| r.kind == "image")
            .collect();
        assert_eq!(images.len(), 2, "both images must survive the transform");
        assert_eq!(images[0].image_link, "https://site.example/big");
        assert_eq!(images[1].image_link, "", "an unlinked image zooms instead");
    }

    /// §9.3 media policy, from user's setting to transform context.
    ///
    /// Only place policy is built: Images control in Settings must reach it
    /// (Strict → Strict, Allow → Allow), not hardcoded `UnproxiedMedia::Ask`.
    #[test]
    fn the_images_setting_reaches_the_transform_context() {
        use vuo_core::content::UnproxiedMedia;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = vuo_core::db::Database::open(&dir.path().join("m.sqlite")).expect("mirror");
        let instance = url::Url::parse("https://miniflux.example/").expect("url");
        let ctx = crate::context::context_for_test(db, instance);
        let model = ArticleModel::default();

        for (setting, expected) in [
            (crate::settings::MEDIA_STRICT, UnproxiedMedia::Strict),
            (crate::settings::MEDIA_ASK, UnproxiedMedia::Ask),
            (crate::settings::MEDIA_ALLOW, UnproxiedMedia::Allow),
        ] {
            ctx.set_media_policy(setting);
            let context = model.transform_context(&ctx);
            let MediaPolicy::ProxyThroughInstance { fallback, .. } = context.media else {
                panic!("the policy must proxy through the instance");
            };
            assert_eq!(
                fallback, expected,
                "the Images setting {setting} must reach the transform"
            );
        }

        // Unrecognised value from hand-edited account file falls back to Ask,
        // not Allow.
        ctx.set_media_policy(99);
        let context = model.transform_context(&ctx);
        let MediaPolicy::ProxyThroughInstance { fallback, .. } = context.media else {
            panic!("the policy must proxy through the instance");
        };
        assert_eq!(fallback, UnproxiedMedia::Ask);
    }

    /// §the article view can see and change read/starred.
    ///
    /// With article open, reader can tell read/starred and set both, not only
    /// from entry list's context menu before opening.
    #[test]
    fn the_open_article_reports_and_changes_its_own_read_and_starred_state() {
        use vuo_core::db::outbox;
        use vuo_core::model::{Entry, EntryStatus, FeedId};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("m.sqlite");
        let mut db = vuo_core::db::Database::open(&path).expect("mirror");

        let entry = Entry {
            id: EntryId(7),
            feed_id: FeedId(1),
            status: EntryStatus::Unread,
            starred: false,
            title: "t".to_owned(),
            url: None,
            comments_url: None,
            author: String::new(),
            content: "<p>body</p>".to_owned(),
            published_at: None,
            created_at: None,
            changed_at: None,
            reading_time: 1,
            tags: Vec::new(),
            enclosures: Vec::new(),
        };
        db.with_tx(|tx| {
            store::upsert_feed(
                tx,
                &vuo_core::model::Feed {
                    id: FeedId(1),
                    category_id: None,
                    title: "f".to_owned(),
                    site_url: None,
                    feed_url: None,
                    icon_id: None,
                    checked_at: None,
                    parsing_error_message: String::new(),
                    parsing_error_count: 0,
                    disabled: false,
                    hide_globally: false,
                    crawler: false,
                },
                1,
            )?;
            store::upsert_entry(tx, &entry, 1, store::Arrival::Quiet)
        })
        .expect("seed");

        let instance = url::Url::parse("https://miniflux.example/").expect("url");
        let ctx = crate::context::context_for_test(db, instance);
        crate::context::install(std::rc::Rc::clone(&ctx));

        let mut model = ArticleModel::default();
        model.load(7);
        assert!(!model.isRead, "the seeded entry is unread");
        assert!(!model.isStarred);

        model.toggleRead();
        model.toggleStarred();
        assert!(model.isRead, "the view must reflect what it just did");
        assert!(model.isStarred);

        // Both intents reached outbox, not just flipped label server never
        // hears about.
        let queued = ctx
            .read(|db| outbox::len(db.conn()).unwrap_or(0))
            .expect("read the mirror");
        assert_eq!(queued, 2, "a read and a star must both be enqueued");

        // Toggling back symmetric.
        model.toggleRead();
        model.toggleStarred();
        assert!(!model.isRead);
        assert!(!model.isStarred);

        // Nothing open: toggle = no-op, not mutation of last loaded entry.
        model.clear();
        model.toggleStarred();
        assert!(!model.isStarred);
    }
}
