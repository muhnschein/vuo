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
use vuo_core::model::EntryId;

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
            src, alt, fetch, ..
        } => {
            row.kind = "image".to_owned();
            row.image_src = src.as_str().to_owned();
            row.image_alt = alt.clone();
            row.image_host = src.as_url().host_str().unwrap_or_default().to_owned();
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

    rows: Vec<BlockRow>,
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
        if self.entry_id != 0 {
            ctx.send(Command::FetchOriginal {
                entry_id: self.entry_id,
            });
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

    fn clear(&mut self) {
        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows.clear();
        (self as &mut dyn QAbstractListModel).end_reset_model();
        self.countChanged();
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
}
