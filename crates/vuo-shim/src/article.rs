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
use vuo_core::content::{
    BlockKind, MediaFetch, MediaPolicy, RenderBlock, Span, TransformContext, UnproxiedMedia,
};
use vuo_core::db::store;
use vuo_core::model::{EntryId, EntryStatus};

use crate::context::AppContext;
use crate::worker::Command;

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
pub const ROLE_IMAGE_HOST: i32 = USER_ROLE + 12;
pub const ROLE_IMAGE_RATIO: i32 = USER_ROLE + 13;

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
/// The server rejected the API key. `fetchMessage` is empty by design; the
/// page supplies its own translated line (see `Notice::SyncFailed`).
pub const FETCH_AUTH: i32 = 5;

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
    /// The host the image would be fetched from. Shown in the consent
    /// placeholder so "load images" is an informed choice rather than a shrug.
    pub image_host: String,
    /// True when the image is third-party and un-proxied: the delegate shows a
    /// tap-to-load placeholder naming the host and fetches nothing (§9.3).
    pub needs_consent: bool,
    /// height / width from the `<img>` tag's own attributes; 0 when it gave
    /// none usable.
    ///
    /// A ratio rather than the pair, because reserving space is all the
    /// delegate does with it and the width it renders at is its own. 0 means
    /// "no hint": the delegate falls back to a fixed placeholder rather than
    /// collapsing the row to nothing, which is what made an article re-flow
    /// under the reader every time an image landed.
    pub image_ratio: f64,
    pub code_language: String,
}

fn row_for(block: &RenderBlock) -> BlockRow {
    let mut row = BlockRow {
        quote_depth: i32::from(block.quote_depth),
        ..BlockRow::default()
    };
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
        BlockKind::ListItem {
            ordered,
            number,
            indent,
            spans,
        } => {
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
        BlockKind::Image {
            src,
            alt,
            fetch,
            intrinsic,
            ..
        } => {
            row.kind = "image".to_owned();
            row.image_src = src.as_str().to_owned();
            row.image_alt = alt.clone();
            row.image_host = src.as_url().host_str().unwrap_or_default().to_owned();
            row.needs_consent = matches!(fetch, MediaFetch::NeedsConsent);
            // The core caps both dimensions well below the point where this
            // division could lose precision or produce a silly rectangle.
            row.image_ratio = intrinsic
                .map(|(w, h)| f64::from(h) / f64::from(w))
                .unwrap_or(0.0);
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

    count: qt_property!(i32; READ row_count NOTIFY countChanged),
    countChanged: qt_signal!(),

    /// True when the transform had to cut the article short. The UI says so
    /// rather than presenting a fragment as the whole thing.
    pub truncated: qt_property!(bool; NOTIFY truncatedChanged),
    truncatedChanged: qt_signal!(),

    /// How many images are held back awaiting consent, so the UI can offer a
    /// single "load images from example.com" affordance rather than N.
    pub blockedImages: qt_property!(i32; NOTIFY blockedImagesChanged),
    blockedImagesChanged: qt_signal!(),

    /// Whether the open article is read, and whether it is starred.
    ///
    /// The article view could show neither and change neither: the only place
    /// either could be seen or set was the entry list's context menu, so a
    /// reader who opened an article had no way to star it, and no way to tell
    /// whether the one they were looking at was already starred.
    pub isRead: qt_property!(bool; NOTIFY entryStateChanged),
    pub isStarred: qt_property!(bool; NOTIFY entryStateChanged),
    /// The article's own link, or empty when the entry has none.
    ///
    /// A property rather than only `openInBrowser`'s return value, because the
    /// page BINDS to it: the site page attached to the right of the article
    /// exists exactly when this is non-empty. Validated as http(s) by the
    /// core before it was stored, so handing it to a WebView or the system
    /// browser cannot launch anything else.
    pub articleUrl: qt_property!(QString; NOTIFY entryStateChanged),
    entryStateChanged: qt_signal!(),

    clear: qt_method!(fn(&mut self)),
    /// Load an entry from the mirror and transform its stored HTML.
    ///
    /// Reads locally, so opening an article works with no network at all.
    load: qt_method!(fn(&mut self, entry_id: i64)),
    /// Hand the article's own URL to the system browser.
    openInBrowser: qt_method!(fn(&self) -> QString),
    /// Ask the *server* to scrape the original page (§3: no local Readability).
    fetchOriginal: qt_method!(fn(&mut self)),
    /// Consent to loading images from the origin of the block at `row`, for
    /// this article. Re-transforms so the placeholders become images.
    allowImagesFrom: qt_method!(fn(&mut self, row: i32)),
    /// Re-read the open article when the worker has changed the mirror.
    ///
    /// This model had no poll at all, which is why "Fetch original content"
    /// looked like a menu item wired to nothing: the scrape really did run and
    /// the scraped body really was written to SQLite, but nothing told the
    /// OPEN page to read it again, so it appeared only if you left the article
    /// and came back. See `EntryModel::pollSync`, the same pattern.
    pollSync: qt_method!(fn(&mut self) -> bool),
    /// True while a scrape is in flight, so the page can say something.
    pub fetching: qt_property!(bool; NOTIFY fetchingChanged),
    fetchingChanged: qt_signal!(),
    /// How the last scrape ended: one of the `FETCH_*` constants.
    pub fetchStatus: qt_property!(i32; NOTIFY fetchStatusChanged),
    /// Foreign text explaining a failure; empty otherwise. Render as
    /// `Text.PlainText` (§9.3).
    pub fetchMessage: qt_property!(QString; NOTIFY fetchStatusChanged),
    fetchStatusChanged: qt_signal!(),
    /// Acknowledge the last scrape result, so its banner can be dismissed.
    clearFetchStatus: qt_method!(fn(&mut self)),
    /// How long the open article must stay on screen before it counts as
    /// read: -1 never, 0 immediately, otherwise milliseconds.
    ///
    /// Milliseconds because the page drives a QML `Timer` with it directly.
    pub markReadDelayMs: qt_property!(i32; READ mark_read_delay_ms NOTIFY entryStateChanged),
    /// Mark the open article read. Idempotent, and never a toggle.
    ///
    /// Deliberately not `toggleRead`: this fires against whatever happens to
    /// be open, and a toggle would mark an already-read article UNREAD --
    /// re-opening something you had read would quietly resurrect it.
    markRead: qt_method!(fn(&mut self)),
    /// Flip read/unread, and flip starred, for the open article.
    ///
    /// Local first and enqueued, exactly as the entry list's own toggles are:
    /// the mirror is the source of truth and the outbox carries the intent to
    /// the server whenever there is a network.
    toggleRead: qt_method!(fn(&mut self)),
    toggleStarred: qt_method!(fn(&mut self)),

    rows: Vec<BlockRow>,
    /// The worker generation this model last re-read at.
    seen_generation: u64,
    entry_id: i64,
    article_url: String,
    /// Origins the user has agreed to for this article view.
    consented: Vec<url::Url>,
    ctx: Option<std::rc::Rc<AppContext>>,
}

impl ArticleModel {
    /// Give the model its context. Called during app start-up, not from QML.
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
    }

    /// The app context: the one attached explicitly (tests), else the global.
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

        let content = entry.content.clone();
        let ctx_for_transform = self.transform_context(&ctx);
        self.set_html(&content, &ctx_for_transform);
    }

    /// Build the transform context, honouring consent already granted.
    fn transform_context(&self, ctx: &AppContext) -> TransformContext {
        TransformContext {
            base_url: url::Url::parse(&self.article_url).ok(),
            media: MediaPolicy::ProxyThroughInstance {
                instance: ctx.instance().clone(),
                // Origins the user consented to are folded in as trusted, so
                // the transform yields Fetch rather than NeedsConsent for them.
                extra_trusted: self.consented.clone(),
                // The user's Images setting, not a hardcoded default. This was
                // `UnproxiedMedia::Ask` unconditionally, so choosing Strict or
                // Allow in Settings did nothing at all (§9.3).
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
        // Returns the URL rather than launching anything: QML owns
        // Qt.openUrlExternally, and handing a URL back keeps this crate free
        // of platform launching concerns. Empty means "no link".
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
        // Keep the generation cursor moving even when we do nothing with it,
        // so a later poll cannot mistake a backlog of unrelated bumps for
        // something addressed to this article.
        self.seen_generation = ctx.signal().generation();

        let open = self.entry_id;
        if open == 0 {
            return false;
        }
        let Some(outcome) = ctx.signal().take_fetch_outcome(open) else {
            // A scrape can only be resolved by its own outcome. Reacting to
            // the generation counter instead is what made this menu item look
            // dead: mark-read-after-N-seconds bumps too, and whichever bump
            // arrived first cleared `fetching` and re-read a mirror the scrape
            // had not written to yet.
            return false;
        };

        if self.fetching {
            self.fetching = false;
            self.fetchingChanged();
        }
        self.fetchStatus = outcome.status;
        self.fetchMessage = QString::from(outcome.message);
        self.fetchStatusChanged();

        // Only a stored body changes what is on screen. Re-loading for an
        // empty or unchanged scrape would reset the block list -- and so the
        // scroll position -- to show the reader exactly what they were already
        // reading.
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

    /// Grant consent for the ORIGIN of the image at `row`.
    ///
    /// Scoped to that one origin, not to third-party media in general: §9.3's
    /// whole point is that agreeing to load from one host is not agreeing to
    /// load from every host a feed happens to reference.
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
        // Remember across articles too: agreeing to a host once should not have
        // to be repeated on every entry from the same site.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        ctx.write(|db| db.with_tx(|tx| store::grant_media_consent(tx, origin.as_str(), now)));

        // Re-transform so the placeholders become images.
        let entry_id = self.entry_id;
        self.load(entry_id);
    }

    /// Transform an article's HTML and show it.
    pub fn set_html(&mut self, html: &str, ctx: &TransformContext) {
        let document = vuo_core::content::transform(html, ctx);
        let rows: Vec<BlockRow> = document.blocks.iter().map(row_for).collect();
        let blocked = rows.iter().filter(|r| r.needs_consent).count();

        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows = rows;
        (self as &mut dyn QAbstractListModel).end_reset_model();

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

    /// Queue a local mutation for the open entry and nudge the outbox.
    ///
    /// `false` when there is no context, no entry open, or the write was
    /// refused -- in which case the caller leaves the displayed state alone
    /// rather than showing a change that did not happen.
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
            // The mirror changed, so say so: the entry list's row and the
            // cover's unread badge both read from it. Without this, marking an
            // article read from here left the list showing it as unread and
            // the cover's count stale until something else reloaded them.
            //
            // Safe against the scroll-reset above: this model re-reads only
            // for a scrape it asked for, so its own bump does not move the
            // reader.
            ctx.signal().bump();
            // Opportunistic: with no network the intent waits in the outbox
            // and the next sync carries it.
            ctx.send(Command::FlushOutbox);
        }
        applied
    }

    fn clear(&mut self) {
        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows.clear();
        (self as &mut dyn QAbstractListModel).end_reset_model();
        self.countChanged();
        // Forget which entry this was, or the next `clear`-then-toggle would
        // mutate the article that is no longer open.
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
        names.insert(ROLE_CODE_LANGUAGE, "codeLanguage".into());
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §9.3's media policy, from the user's setting to the transform context.
    ///
    /// `transform_context` used to hardcode `UnproxiedMedia::Ask`, and
    /// `Settings::media_policy_for` -- the function that maps the setting --
    /// had ZERO production callers. So the Images control in Settings was
    /// rendered, persisted and read back, and did nothing: a user who chose
    /// Strict still got Ask, and one who chose Allow still got a tap-to-load
    /// placeholder. Nothing in the build noticed, because nothing tested the
    /// one place the policy is actually built.
    #[test]
    fn the_images_setting_reaches_the_transform_context() {
        use vuo_core::content::UnproxiedMedia;

        let dir = tempfile::tempdir().expect("tempdir");
        let db = vuo_core::db::Database::open(&dir.path().join("m.sqlite")).expect("mirror");
        let signal = std::sync::Arc::new(crate::context::SyncSignal::default());
        let instance = url::Url::parse("https://miniflux.example/").expect("url");
        let worker = crate::worker::Worker::spawn(
            dir.path().join("m.sqlite"),
            instance.clone(),
            vuo_core::redact::ApiToken::new("t"),
            vuo_core::api::TransportConfig::default(),
            std::sync::Arc::clone(&signal),
            |_| {},
        );
        let ctx = AppContext::new(db, worker, instance, signal, 0);
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

        // An unrecognised value from a hand-edited account file falls back to
        // Ask rather than to Allow.
        ctx.set_media_policy(99);
        let context = model.transform_context(&ctx);
        let MediaPolicy::ProxyThroughInstance { fallback, .. } = context.media else {
            panic!("the policy must proxy through the instance");
        };
        assert_eq!(fallback, UnproxiedMedia::Ask);
    }

    /// §the article view can see and change read/starred.
    ///
    /// Reported from a device: with an article open there was no way to tell
    /// whether it was read or starred, and no way to set either. Both states
    /// existed in the mirror and both mutations existed on `EntryModel`, but
    /// `ArticleModel` exposed neither -- so the only place a reader could star
    /// an article was the entry list's context menu, before opening it.
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
            vuo_core::db::store::upsert_feed(
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
            vuo_core::db::store::upsert_entry(tx, &entry, 1)
        })
        .expect("seed");

        let signal = std::sync::Arc::new(crate::context::SyncSignal::default());
        let instance = url::Url::parse("https://miniflux.example/").expect("url");
        let worker = crate::worker::Worker::spawn(
            path,
            instance.clone(),
            vuo_core::redact::ApiToken::new("t"),
            vuo_core::api::TransportConfig::default(),
            std::sync::Arc::clone(&signal),
            |_| {},
        );
        let ctx = AppContext::new(db, worker, instance, signal, 0);
        crate::context::install(std::rc::Rc::clone(&ctx));

        let mut model = ArticleModel::default();
        model.load(7);
        assert!(!model.isRead, "the seeded entry is unread");
        assert!(!model.isStarred);

        model.toggleRead();
        model.toggleStarred();
        assert!(model.isRead, "the view must reflect what it just did");
        assert!(model.isStarred);

        // And both intents actually reached the outbox, rather than only
        // flipping a label the server will never hear about.
        let queued = ctx
            .read(|db| outbox::len(db.conn()).unwrap_or(0))
            .expect("read the mirror");
        assert_eq!(queued, 2, "a read and a star must both be enqueued");

        // Toggling back is symmetric.
        model.toggleRead();
        model.toggleStarred();
        assert!(!model.isRead);
        assert!(!model.isStarred);

        // With nothing open, a toggle is a no-op rather than a mutation of
        // whichever entry happened to be loaded last.
        model.clear();
        model.toggleStarred();
        assert!(!model.isStarred);
    }
}
