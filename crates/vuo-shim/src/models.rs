//! `QAbstractListModel` adapters over the local mirror.
//!
//! §5: *the local SQLite mirror is the single source of truth for the UI [...]
//! sync writes to SQLite; models observe SQLite.* These models therefore hold
//! no authority of their own. They are a cache of rows read from the mirror,
//! refreshed by [`reload`](EntryModel::reload) whenever something says the
//! mirror changed. Nothing here ever talks to the network.
//!
//! # Thread affinity
//!
//! Every method here runs on the Qt thread. The worker never touches a
//! `QObject`: it writes to SQLite and bumps [`crate::context::SyncSignal`]'s
//! generation counter, and the models notice by POLLING that counter from
//! `pollSync`, driven by a `Timer` in the QML.
//!
//! There is no callback into a model, and there cannot easily be one: QML
//! constructs these objects, so Rust holds no handle to a live one. (This
//! paragraph used to describe a `queued_callback` signal handler calling
//! `reload`. No such handler exists — the poll is the mechanism.)
//!
//! # Roles, not rich text
//!
//! Roles expose *data*, and the QML side decides how to draw it. In particular
//! titles and author names are exposed as plain strings and the QML must set
//! `textFormat: Text.PlainText` (§9.3). Article body text is exposed
//! pre-escaped by [`crate::article`], which is the one place markup is
//! generated, and QML sets `Text.StyledText` there.

use std::collections::HashMap;

use qmetaobject::*;
use vuo_core::db::store;
#[allow(unused_imports)]
use vuo_core::model::FeedId;
use vuo_core::model::{Entry, EntryId, EntryStatus};

use crate::context::AppContext;
use crate::worker::{self, Command};

/// Rows loaded into a list model at once.
///
/// The list is a window onto the mirror, not the whole of it. Anything that
/// acts on "everything in this scope" must query the database rather than
/// iterating the loaded rows.
pub const PAGE_SIZE: i64 = 500;

pub const ROLE_ID: i32 = USER_ROLE;
pub const ROLE_TITLE: i32 = USER_ROLE + 1;
pub const ROLE_AUTHOR: i32 = USER_ROLE + 2;
pub const ROLE_UNREAD: i32 = USER_ROLE + 3;
pub const ROLE_STARRED: i32 = USER_ROLE + 4;
pub const ROLE_FEED_ID: i32 = USER_ROLE + 5;
pub const ROLE_PUBLISHED: i32 = USER_ROLE + 6;
pub const ROLE_READING_TIME: i32 = USER_ROLE + 7;
pub const ROLE_URL: i32 = USER_ROLE + 8;
/// The name of the feed the entry came from. FOREIGN TEXT: `Text.PlainText`.
pub const ROLE_FEED_NAME: i32 = USER_ROLE + 9;
/// A `data:` URI for the feed's icon, or empty when the mirror has none.
pub const ROLE_FEED_ICON: i32 = USER_ROLE + 10;

/// A row as the UI needs it. Deliberately not [`Entry`]: the model holds only
/// what the list draws, so scrolling a long list does not keep every article
/// body resident.
#[derive(Debug, Clone, Default)]
pub struct EntryRow {
    pub id: i64,
    pub feed_id: i64,
    pub title: String,
    pub author: String,
    pub unread: bool,
    pub starred: bool,
    pub published: i64,
    pub reading_time: i32,
    pub url: String,
    /// The feed's name, filled in after the query from the chrome cache.
    pub feed_name: String,
    /// The feed's icon as a `data:` URI, or empty.
    pub feed_icon: String,
}

impl From<&Entry> for EntryRow {
    fn from(e: &Entry) -> Self {
        EntryRow {
            id: e.id.get(),
            feed_id: e.feed_id.get(),
            title: e.title.clone(),
            author: e.author.clone(),
            unread: !e.status.is_read(),
            starred: e.starred,
            published: e.published_at.map(|t| t.timestamp()).unwrap_or(0),
            reading_time: e.reading_time,
            url: e
                .url
                .as_ref()
                .map(|u| u.as_str().to_owned())
                .unwrap_or_default(),
            // Filled in by `reload` from the per-feed cache: the entry query
            // knows nothing about feeds.
            feed_name: String::new(),
            feed_icon: String::new(),
        }
    }
}

/// A feed's name and icon URI, as an entry row needs them.
#[derive(Debug, Clone, Default)]
struct FeedChrome {
    name: String,
    icon_uri: String,
}

/// Wrap image bytes as a `data:` URI QML's `Image.source` can take.
///
/// The MIME type comes from the mirror, which stores the format DETERMINED
/// FROM THE BYTES rather than the one the server claimed -- so a server that
/// labels a script `image/png` cannot get that label back out of here.
fn data_uri(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    // SVG is excluded on purpose: it is a document, not a bitmap, and Qt's
    // renderer will follow external references in one -- which would leak the
    // device's IP to whatever host a feed operator names, on a list scroll.
    // The raster formats are passed through even where the device may lack a
    // handler (it ships only libqjpeg.so as a plugin, so ICO and GIF are a
    // gamble); the delegate hides an Image that fails to load, so the cost of
    // guessing wrong is a missing favicon rather than a broken-image glyph.
    if mime == "image/svg+xml" {
        return String::new();
    }
    format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Which slice of the mirror a model shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Unread,
    Starred,
    All,
    Feed(i64),
    Category(i64),
}

impl Scope {
    fn to_filter(self) -> store::EntryFilter {
        match self {
            Scope::Unread => store::EntryFilter::Unread,
            Scope::Starred => store::EntryFilter::Starred,
            Scope::All => store::EntryFilter::All,
            Scope::Feed(id) => store::EntryFilter::Feed(id),
            Scope::Category(id) => store::EntryFilter::Category(id),
        }
    }

    /// Decode the integer pair QML passes. Qt 5.6 has no `QEnum` registration
    /// in this crate version, so the scope crosses the boundary as two ints
    /// rather than as a typed enum.
    pub fn from_qml(kind: i32, id: i64) -> Scope {
        match kind {
            1 => Scope::Starred,
            2 => Scope::All,
            3 => Scope::Feed(id),
            4 => Scope::Category(id),
            _ => Scope::Unread,
        }
    }
}

#[derive(QObject, Default)]
pub struct EntryModel {
    base: qt_base_class!(trait QAbstractListModel),

    count: qt_property!(i32; READ row_count NOTIFY countChanged),
    countChanged: qt_signal!(),

    /// Set the scope and reload. `kind`: 0 unread, 1 starred, 2 all,
    /// 3 feed (id), 4 category (id).
    setScope: qt_method!(fn(&mut self, kind: i32, id: i64)),
    /// Re-read the mirror. Called after the worker reports a change.
    refresh: qt_method!(fn(&mut self)),
    entryIdAt: qt_method!(fn(&self, row: i32) -> i64),

    /// Mark the entry at `row` read or unread.
    ///
    /// Applied to the mirror immediately and queued for the server, so it
    /// works offline and the list updates in the same frame as the tap.
    setRead: qt_method!(fn(&mut self, row: i32, read: bool)),
    setStarred: qt_method!(fn(&mut self, row: i32, starred: bool)),
    /// Ask the worker for a NETWORK sync.
    ///
    /// Distinct from `refresh`, which only re-reads the local mirror. Nothing
    /// in the UI used to reach the worker at all, so `Command::Sync` had a
    /// handler and no sender: the app never talked to the server.
    requestSync: qt_method!(fn(&mut self)),
    /// Total unread across the whole mirror, for the cover. NOT `count`, which
    /// is the number of rows this model is currently showing and is capped by
    /// the page size.
    unreadTotal: qt_property!(i32; READ unread_total NOTIFY countChanged),
    /// True while a sync is in flight. Read from the worker's signal rather
    /// than set locally, so it clears when the worker actually finishes.
    syncing: qt_property!(bool; READ is_syncing NOTIFY syncingChanged),
    syncingChanged: qt_signal!(),

    /// Why the last refresh failed, or empty when the last one was fine.
    ///
    /// State on the model rather than a one-shot signal: the failure has to
    /// survive being produced while the app is on its cover (the cover has a
    /// Refresh of its own and no page to receive a signal), and it has to
    /// survive a page that is not currently the visible one. FOREIGN TEXT --
    /// the server's own words -- so the page renders it as PlainText.
    pub syncError: qt_property!(QString; NOTIFY syncStateChanged),
    /// True when the failure was the server rejecting the API key.
    ///
    /// The page shows a fixed translated line for this and offers Settings;
    /// `syncError` is empty in that case, because a server's phrasing for a
    /// bad key tells the user nothing they can act on.
    pub syncErrorIsAuth: qt_property!(bool; NOTIFY syncStateChanged),
    syncStateChanged: qt_signal!(),
    /// Poll for worker activity. Called from a QML Timer.
    ///
    /// A poll rather than a push: QML owns these objects, so Rust has no list
    /// of live models to call into, and a registry of cross-thread pointers is
    /// exactly the kind of thing that cannot be exercised without a device.
    /// Returns true if anything reloaded.
    pollSync: qt_method!(fn(&mut self) -> bool),
    /// Mark everything in the current scope read.
    ///
    /// Expanded locally into concrete entry ids rather than queueing the
    /// server's bulk endpoint, which applies a `published_at < now()` cut-off
    /// captured at request time and would mark entries the user never saw.
    markAllRead: qt_method!(fn(&mut self)),
    /// Mark everything in an EXPLICIT scope as read.
    ///
    /// `markAllRead` reads the scope this model happens to be in when the
    /// remorse countdown FIRES, which is a different moment from the one the
    /// user was looking at when they tapped. That was survivable only because
    /// re-scoping needed a page change and a page change flushed the popup
    /// first; swiping between tabs re-scopes without one, so arming "Mark all
    /// as read" on Unread and swiping to All within the countdown would have
    /// run it over every article in the mirror. Passing the scope in binds the
    /// action to the tab that was showing when it was armed.
    markAllReadIn: qt_method!(fn(&mut self, kind: i32, id: i64)),

    rows: Vec<EntryRow>,
    scope: Option<Scope>,
    /// The worker generation this model last reloaded at.
    seen_generation: u64,
    /// Feed name and icon URI per feed id, built once and reused.
    ///
    /// Re-encoding every icon on every reload would base64 the same few
    /// kilobytes on each poll of a list that is polled twice a second.
    feed_chrome: std::collections::HashMap<i64, FeedChrome>,
    /// The spinner state this model last told QML about.
    ///
    /// `syncing` is READ + NOTIFY, so QML re-evaluates it only when
    /// `syncingChanged` fires -- and the only emitter was below the generation
    /// early-return. A failure that cleared the flag without changing the
    /// mirror therefore never reached the binding, and the spinner kept
    /// spinning on a flag that was already false. Tracking it here means the
    /// signal fires on the transition itself, whatever the generation did.
    seen_running: bool,
    /// `None` until [`EntryModel::attach`] is called from Rust — QML never
    /// constructs this.
    ctx: Option<std::rc::Rc<AppContext>>,
}

impl EntryModel {
    /// Give the model its context. Called during app start-up, not from QML.
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
        self.reload();
    }

    /// The app context: the one attached explicitly (tests), else the global
    /// installed at start-up. QML constructs these objects, so there is no
    /// constructor to pass it through.
    fn context(&self) -> Option<std::rc::Rc<AppContext>> {
        self.ctx.clone().or_else(crate::context::current)
    }

    fn setRead(&mut self, row: i32, read: bool) {
        let Some(id) = self.id_at(row) else { return };
        let Some(ctx) = self.context() else { return };
        let status = if read {
            EntryStatus::Read
        } else {
            EntryStatus::Unread
        };
        let applied = ctx
            .write(|db| worker::apply_local_status(db, id, status))
            .transpose()
            .ok()
            .flatten()
            .is_some();
        if applied {
            self.mark_row(id, Some(!read), None);
            self.announce_local_change(&ctx);
            // Send opportunistically: if there is no signal the intent stays
            // in the outbox and the next sync carries it.
            ctx.send(Command::FlushOutbox);
        }
    }

    fn setStarred(&mut self, row: i32, starred: bool) {
        let Some(id) = self.id_at(row) else { return };
        let Some(ctx) = self.context() else { return };
        let applied = ctx
            .write(|db| worker::apply_local_starred(db, id, starred))
            .transpose()
            .ok()
            .flatten()
            .is_some();
        if applied {
            self.mark_row(id, None, Some(starred));
            self.announce_local_change(&ctx);
            ctx.send(Command::FlushOutbox);
        }
    }

    /// Tell the OTHER models that this one changed the mirror.
    ///
    /// There is one model per scope tab now, and they all read the same
    /// entries. Marking an article read on Unread has to reach the copy of
    /// that row held by All, or swiping across shows it still unread until the
    /// next network sync.
    ///
    /// The generation is bumped, and then this model's own cursor is moved
    /// PAST that bump. Without the second half the mutating model would
    /// reload itself on its next poll -- a full reset, which scrolls the list
    /// back to the top under the finger that just tapped. `mark_row` has
    /// already patched this model's own row in place; the bump is for everyone
    /// else.
    fn announce_local_change(&mut self, ctx: &AppContext) {
        ctx.signal().bump();
        self.seen_generation = ctx.signal().generation();
    }

    fn requestSync(&mut self) {
        // `or_else`, not a bare `context()`: the context is built at start-up
        // from the stored account, and on a first run there is none to build
        // from. Without this second chance the pulley menu's Refresh did
        // nothing at all -- no spinner, no error -- for the whole of the
        // session in which the account was first configured.
        let Some(ctx) = self.context().or_else(crate::context::refresh_current) else {
            return;
        };
        // A new attempt supersedes whatever the last one said.
        if !self.syncError.to_string().is_empty() || self.syncErrorIsAuth {
            self.syncError = QString::from(String::new());
            self.syncErrorIsAuth = false;
            self.syncStateChanged();
        }
        // Set before sending, not after: the worker clears this flag when the
        // command finishes, and a fast failure can beat us to it. Setting it
        // afterwards would leave the spinner running forever.
        ctx.signal().set_running(true);
        if !ctx.send(Command::Sync) {
            // The worker is gone, so nothing will ever clear the flag.
            ctx.signal().set_running(false);
        }
        // Record what we just told QML, or the spinner never stops.
        //
        // `pollSync` reports a change by comparing the flag against
        // `seen_running`. Raising the flag here and emitting the signal
        // WITHOUT recording it left `seen_running` false, so when the worker
        // lowered the flag the poll compared false against false, saw no
        // transition, and never emitted again -- `syncing` is READ + NOTIFY,
        // so QML kept the last value it was told. The spinner only ever
        // stopped when a poll happened to land inside the window while the
        // sync was still running, which on a fast local server it usually
        // does not.
        self.seen_running = ctx.signal().is_running();
        self.syncingChanged();
    }

    fn is_syncing(&self) -> bool {
        self.context().is_some_and(|ctx| ctx.signal().is_running())
    }

    fn pollSync(&mut self) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };

        // The spinner first, and BEFORE the generation guard. A refresh that
        // fails changes the running flag without necessarily changing the
        // mirror, and that is exactly the case the old order could not report.
        let running = ctx.signal().is_running();
        if running != self.seen_running {
            self.seen_running = running;
            self.syncingChanged();
        }

        // Then anything the worker left for the user to read. Drained here, in
        // the one app-wide poll, rather than by a Timer on each page: several
        // EntryListPages are alive at once (the feed views), and per-page
        // timers would race for a slot that holds exactly one notice.
        self.drain_notice(ctx.signal());

        let generation = ctx.signal().generation();
        if generation == self.seen_generation {
            return false;
        }
        self.seen_generation = generation;
        self.reload();
        true
    }

    /// Take a sync failure off the signal, if one is waiting.
    ///
    /// Anything this model does not own is put back for the page that does --
    /// the same discipline `Settings::poll_notice_from` follows.
    pub fn drain_notice(&mut self, signal: &crate::context::SyncSignal) {
        match signal.take_notice() {
            Some(crate::context::Notice::SyncFailed { auth, message }) => {
                self.syncErrorIsAuth = auth;
                self.syncError = QString::from(message);
                self.syncStateChanged();
            }
            Some(other) => signal.post(other),
            None => {}
        }
    }

    fn unread_total(&self) -> i32 {
        self.context()
            .and_then(|ctx| ctx.read(|db| store::unread_count(db.conn()).unwrap_or(0)))
            .map(|n| i32::try_from(n).unwrap_or(i32::MAX))
            .unwrap_or(0)
    }

    fn markAllRead(&mut self) {
        let Some(scope) = self.scope else { return };
        self.mark_all_read_in(scope);
    }

    fn markAllReadIn(&mut self, kind: i32, id: i64) {
        self.mark_all_read_in(Scope::from_qml(kind, id));
    }

    fn mark_all_read_in(&mut self, scope: Scope) {
        let Some(ctx) = self.context() else { return };
        let done = match scope {
            Scope::Feed(feed_id) => ctx
                .write(|db| worker::apply_local_mark_feed_read(db, feed_id))
                .transpose()
                .ok()
                .flatten()
                .is_some(),
            Scope::Category(category_id) => ctx
                .write(|db| worker::apply_local_mark_category_read(db, category_id))
                .transpose()
                .ok()
                .flatten()
                .is_some(),
            // Unread/Starred/All have no single server-side scope, so expand
            // over every entry in scope -- NOT over `self.rows`, which holds
            // only the page the list is currently showing. Using the rows
            // marked at most PAGE_SIZE entries and silently left the rest
            // unread, while the UI reported success.
            _ => {
                let ids: Vec<EntryId> = ctx
                    .read(|db| {
                        store::list_entries(db.conn(), scope.to_filter(), i64::MAX, 0)
                            .unwrap_or_default()
                            .iter()
                            .map(|e| e.id)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                ctx.write(|db| worker::apply_local_status_bulk(db, &ids, EntryStatus::Read))
                    .transpose()
                    .ok()
                    .flatten()
                    .is_some()
            }
        };
        if done {
            self.reload();
            self.announce_local_change(&ctx);
            ctx.send(Command::FlushOutbox);
        }
    }

    fn id_at(&self, row: i32) -> Option<EntryId> {
        usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .map(|r| EntryId(r.id))
    }

    fn setScope(&mut self, kind: i32, id: i64) {
        self.scope = Some(Scope::from_qml(kind, id));
        self.reload();
    }

    fn refresh(&mut self) {
        self.reload();
    }

    fn entryIdAt(&self, row: i32) -> i64 {
        usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.id)
            .unwrap_or(0)
    }

    /// Re-read rows from the mirror.
    ///
    /// A full reset rather than a diff. For the list sizes a phone actually
    /// scrolls this is imperceptible, and a wrong `begin_insert_rows` range is
    /// a crash inside Qt's model machinery rather than a visual glitch — a bad
    /// trade for a saved millisecond.
    pub fn reload(&mut self) {
        let Some(ctx) = self.context() else { return };
        let Some(scope) = self.scope else { return };

        let entries = ctx
            .read(|db| {
                store::list_entries(db.conn(), scope.to_filter(), 500, 0).unwrap_or_default()
            })
            .unwrap_or_default();
        let mut rows: Vec<EntryRow> = entries.iter().map(EntryRow::from).collect();
        self.refresh_feed_chrome();
        for row in &mut rows {
            if let Some(chrome) = self.feed_chrome.get(&row.feed_id) {
                row.feed_name = chrome.name.clone();
                row.feed_icon = chrome.icon_uri.clone();
            }
        }

        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows = rows;
        (self as &mut dyn QAbstractListModel).end_reset_model();
        self.countChanged();
    }

    /// Rebuild the feed name/icon cache from the mirror.
    ///
    /// Called from `reload`, which runs on a scope change or a generation
    /// bump -- not on every poll. A mirror has tens of feeds and a favicon is
    /// a couple of kilobytes, so re-encoding the set outright is cheaper than
    /// the invalidation logic that avoiding it would need.
    fn refresh_feed_chrome(&mut self) {
        let Some(ctx) = self.context() else { return };
        let Some(feeds) = ctx.read(|db| store::feed_chrome(db.conn()).unwrap_or_default()) else {
            return;
        };
        self.feed_chrome = feeds
            .into_iter()
            .map(|feed| {
                let icon_uri = feed
                    .icon
                    .map(|(mime, bytes)| data_uri(&mime, &bytes))
                    .unwrap_or_default();
                (
                    feed.feed_id,
                    FeedChrome {
                        name: feed.title,
                        icon_uri,
                    },
                )
            })
            .collect();
    }

    /// Update one row in place after a local mutation, without a full reload.
    pub fn mark_row(&mut self, id: EntryId, unread: Option<bool>, starred: Option<bool>) {
        let Some(index) = self.rows.iter().position(|r| r.id == id.get()) else {
            return;
        };
        if let Some(row) = self.rows.get_mut(index) {
            if let Some(u) = unread {
                row.unread = u;
            }
            if let Some(s) = starred {
                row.starred = s;
            }
        }
        let Ok(i) = i32::try_from(index) else { return };
        let model_index = (self as &mut dyn QAbstractListModel).row_index(i);
        (self as &mut dyn QAbstractListModel).data_changed(model_index, model_index);
        // A read/unread change moves `unreadTotal`, and that is what the app
        // cover shows. `countChanged` is its only NOTIFY and used to fire from
        // `reload()` alone, so marking an entry read left the cover's badge
        // stale until the next sync happened to reload the model. Harmless
        // when it took a deliberate context-menu tap; glaring once opening an
        // article marks it read on its own.
        if unread.is_some() {
            self.countChanged();
        }
    }

    #[must_use]
    pub fn rows(&self) -> &[EntryRow] {
        &self.rows
    }
}

impl QAbstractListModel for EntryModel {
    fn row_count(&self) -> i32 {
        i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
    }

    fn data(&self, index: QModelIndex, role: i32) -> QVariant {
        // Bounds-checked rather than indexed: this is called by Qt with
        // whatever index a delegate asks for, and `clippy::indexing_slicing`
        // is denied for exactly this shape of code.
        let Ok(i) = usize::try_from(index.row()) else {
            return QVariant::default();
        };
        let Some(row) = self.rows.get(i) else {
            return QVariant::default();
        };

        match role {
            ROLE_ID => row.id.into(),
            ROLE_FEED_ID => row.feed_id.into(),
            ROLE_TITLE => QString::from(row.title.clone()).into(),
            ROLE_AUTHOR => QString::from(row.author.clone()).into(),
            ROLE_UNREAD => row.unread.into(),
            ROLE_STARRED => row.starred.into(),
            ROLE_PUBLISHED => row.published.into(),
            ROLE_READING_TIME => row.reading_time.into(),
            ROLE_URL => QString::from(row.url.clone()).into(),
            ROLE_FEED_NAME => QString::from(row.feed_name.clone()).into(),
            ROLE_FEED_ICON => QString::from(row.feed_icon.clone()).into(),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> HashMap<i32, QByteArray> {
        let mut names = HashMap::new();
        names.insert(ROLE_ID, "entryId".into());
        names.insert(ROLE_FEED_ID, "feedId".into());
        names.insert(ROLE_TITLE, "title".into());
        names.insert(ROLE_AUTHOR, "author".into());
        names.insert(ROLE_UNREAD, "unread".into());
        names.insert(ROLE_STARRED, "starred".into());
        names.insert(ROLE_PUBLISHED, "published".into());
        names.insert(ROLE_READING_TIME, "readingTime".into());
        names.insert(ROLE_URL, "url".into());
        names.insert(ROLE_FEED_NAME, "feedName".into());
        names.insert(ROLE_FEED_ICON, "feedIcon".into());
        names
    }
}

// ------------------------------------------------------------------- feeds

pub const ROLE_FEED_TITLE: i32 = USER_ROLE + 1;
pub const ROLE_FEED_UNREAD: i32 = USER_ROLE + 2;
pub const ROLE_FEED_ERROR: i32 = USER_ROLE + 3;
pub const ROLE_FEED_CATEGORY: i32 = USER_ROLE + 4;
/// "Fetch original content" for every entry of this feed, server-side.
pub const ROLE_FEED_CRAWLER: i32 = USER_ROLE + 5;
/// The server has stopped refreshing this feed.
pub const ROLE_FEED_DISABLED: i32 = USER_ROLE + 6;
/// Keep this feed out of the global unread list.
pub const ROLE_FEED_HIDDEN: i32 = USER_ROLE + 7;

/// A feed's icon as the mirror stores it, per feed id: the MIME type sniffed
/// from the bytes, and the bytes. Encoded into a `data:` URI only for the
/// feeds that reach the cover.
type FeedIcons = HashMap<i64, (String, Vec<u8>)>;

/// How many feeds the cover is told about.
///
/// The cover draws a grid of roughly twenty cells and repeats the feeds it is
/// given to fill it, so everything past this many would be encoded, carried
/// across into QML and then never drawn. The feeds are ordered so the ones
/// with something new come first (see [`cover_feeds_json`]), which is what
/// makes the cap safe: the lit cells are always among the ones on screen.
const COVER_FEEDS: usize = 32;

/// What the cover's grid draws, as JSON.
///
/// A JSON string rather than the model itself, because the cover REPEATS feeds
/// to fill its grid -- a view over rows draws each row once and cannot. QML
/// reads it with `JSON.parse`, never `eval` (§9.3).
///
/// Feeds with unread entries come first, most first. That is not decoration:
/// the grid draws only its first cells' worth, so a feed that fell past them
/// would be lit where nobody could see it, and the number in the corner would
/// be the only sign of it.
fn cover_feeds_json(rows: &[FeedRow], icons: &FeedIcons, limit: usize) -> String {
    /// One cell's feed. `camelCase` because QML reads these keys.
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CoverFeed<'a> {
        feed_id: i64,
        /// FOREIGN TEXT -- the feed operator's words. The cover draws its
        /// first letter, as `Text.PlainText`.
        title: &'a str,
        unread: i32,
        /// The favicon as a `data:` URI, or empty when the mirror has none.
        icon: String,
    }

    let mut ordered: Vec<&FeedRow> = rows.iter().collect();
    // A stable sort, so feeds with the same count keep the mirror's order and
    // the grid does not reshuffle itself between two identical syncs.
    ordered.sort_by_key(|row| std::cmp::Reverse(row.unread));
    let picked: Vec<CoverFeed<'_>> = ordered
        .iter()
        .take(limit)
        .map(|row| CoverFeed {
            feed_id: row.id,
            title: row.title.as_str(),
            unread: row.unread,
            // Encoded here rather than for the whole mirror: past the cap
            // it would be base64 nobody ever draws, on every reload.
            icon: icons
                .get(&row.id)
                .map(|(mime, bytes)| data_uri(mime, bytes))
                .unwrap_or_default(),
        })
        .collect();
    // An empty list rather than nothing: the cover parses this, and "" would
    // send it down the catch branch on every reload.
    serde_json::to_string(&picked).unwrap_or_else(|_| "[]".to_owned())
}

#[derive(Debug, Clone, Default)]
pub struct FeedRow {
    pub id: i64,
    pub title: String,
    pub unread: i32,
    /// Non-empty when the server last failed to refresh this feed. Foreign
    /// text: plain text only.
    pub error: String,
    pub category_id: i64,
    pub crawler: bool,
    pub disabled: bool,
    pub hide_globally: bool,
}

#[derive(QObject, Default)]
pub struct FeedModel {
    base: qt_base_class!(trait QAbstractListModel),
    count: qt_property!(i32; READ row_count NOTIFY countChanged),
    countChanged: qt_signal!(),
    refresh: qt_method!(fn(&mut self)),
    feedIdAt: qt_method!(fn(&self, row: i32) -> i64),
    /// See `EntryModel::pollSync`.
    pollSync: qt_method!(fn(&mut self) -> bool),
    /// Mark every unread entry in the feed at `row` as read.
    markFeedRead: qt_method!(fn(&mut self, row: i32)),
    /// Subscribe. The *server* discovers and fetches the feed; Vuo never
    /// downloads one itself (§3).
    subscribe: qt_method!(fn(&mut self, feed_url: QString)),
    unsubscribe: qt_method!(fn(&mut self, row: i32)),
    /// Save a feed's settings to the server, then to the mirror.
    ///
    /// A flat argument list rather than a JS object, because a `QVariantMap`
    /// crossing this boundary in qmetaobject 0.2.10 loses type information for
    /// bools and the page would have to re-encode them as ints anyway. Passing
    /// `title` unchanged is how "do not rename" is expressed -- the shim
    /// diffs against what the mirror holds and sends only what moved, so an
    /// untouched field is never transmitted (see `FeedPatch`).
    updateFeed: qt_method!(
        fn(
            &mut self,
            row: i32,
            title: QString,
            category_id: i64,
            crawler: bool,
            disabled: bool,
            hide_globally: bool,
        ) -> bool
    ),
    /// The pending result of the last `updateFeed`, drained by the edit page.
    ///
    /// See `EntryModel::syncError`: the worker cannot call into a QML-owned
    /// object, so a one-shot slot plus a poll is how an answer gets back.
    pub updateError: qt_property!(QString; NOTIFY updateStateChanged),
    pub updateOk: qt_property!(bool; NOTIFY updateStateChanged),
    /// Bumped for every finished save, so a page can tell a repeat answer from
    /// a stale one.
    pub updateSerial: qt_property!(i32; NOTIFY updateStateChanged),
    updateStateChanged: qt_signal!(),

    /// The feeds the cover draws, as JSON. See [`cover_feeds_json`].
    ///
    /// `countChanged` is its NOTIFY because this is rebuilt in `reload` and
    /// nowhere else, which is exactly when that fires.
    pub coverFeeds: qt_property!(QString; NOTIFY countChanged),

    rows: Vec<FeedRow>,
    seen_generation: u64,
    ctx: Option<std::rc::Rc<AppContext>>,
}

impl FeedModel {
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
        self.reload();
    }

    /// The app context: the one attached explicitly (tests), else the global
    /// installed at start-up. QML constructs these objects, so there is no
    /// constructor to pass it through.
    fn context(&self) -> Option<std::rc::Rc<AppContext>> {
        self.ctx.clone().or_else(crate::context::current)
    }

    fn pollSync(&mut self) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };
        // BEFORE the generation early-return. A rejected save changes nothing
        // in the mirror, so the generation does not move -- and a drain that
        // happened after the early-return could therefore never report a
        // failure, which is the one answer the edit page most needs.
        self.drain_update_notice(ctx.signal());
        let generation = ctx.signal().generation();
        if generation == self.seen_generation {
            return false;
        }
        self.seen_generation = generation;
        self.reload();
        true
    }

    fn markFeedRead(&mut self, row: i32) {
        let Some(ctx) = self.context() else { return };
        let Some(feed_id) = self.id_at(row) else {
            return;
        };
        let done = ctx
            .write(|db| worker::apply_local_mark_feed_read(db, feed_id))
            .transpose()
            .ok()
            .flatten()
            .is_some();
        if done {
            self.reload();
            ctx.send(Command::FlushOutbox);
        }
    }

    fn subscribe(&mut self, feed_url: QString) {
        let Some(ctx) = self.context() else { return };
        let url = feed_url.to_string();
        if url.trim().is_empty() {
            return;
        }
        ctx.send(Command::Subscribe { feed_url: url });
    }

    fn unsubscribe(&mut self, row: i32) {
        let Some(ctx) = self.context() else { return };
        let Some(feed_id) = self.id_at(row) else {
            return;
        };
        ctx.send(Command::Unsubscribe { feed_id });
    }

    fn id_at(&self, row: i32) -> Option<i64> {
        usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.id)
    }

    #[allow(clippy::fn_params_excessive_bools)]
    fn updateFeed(
        &mut self,
        row: i32,
        title: QString,
        category_id: i64,
        crawler: bool,
        disabled: bool,
        hide_globally: bool,
    ) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };
        let Some(current) = usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .cloned()
        else {
            return false;
        };

        let title = title.to_string();
        let trimmed = title.trim();
        let mut update = vuo_core::api::client::FeedPatch::default();
        // An empty title is a rejection, not an instruction: Miniflux would
        // accept it and leave the user with a nameless row they then cannot
        // identify in order to fix it.
        if !trimmed.is_empty() && trimmed != current.title {
            update.title = Some(trimmed.to_owned());
        }
        if category_id > 0 && category_id != current.category_id {
            update.category_id = Some(category_id);
        }
        if crawler != current.crawler {
            update.crawler = Some(crawler);
        }
        if disabled != current.disabled {
            update.disabled = Some(disabled);
        }
        if hide_globally != current.hide_globally {
            update.hide_globally = Some(hide_globally);
        }
        if update.is_empty() {
            return false;
        }
        ctx.send(Command::UpdateFeed {
            feed_id: current.id,
            update,
        })
    }

    /// Pick up the worker's answer to the last `updateFeed`.
    fn drain_update_notice(&mut self, signal: &crate::context::SyncSignal) {
        match signal.take_notice() {
            Some(crate::context::Notice::FeedUpdated { ok, message }) => {
                self.updateOk = ok;
                self.updateError = QString::from(message);
                self.updateSerial = self.updateSerial.wrapping_add(1);
                self.updateStateChanged();
            }
            // Not ours. Put it back for the page that is waiting on it --
            // dropping it is how "Test connection" used to look like it did
            // nothing.
            Some(other) => signal.post(other),
            None => {}
        }
    }

    fn refresh(&mut self) {
        self.reload();
    }

    fn feedIdAt(&self, row: i32) -> i64 {
        usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.id)
            .unwrap_or(0)
    }

    pub fn reload(&mut self) {
        let Some(ctx) = self.context() else { return };
        // The icons come back beside the rows rather than in them: the feed
        // list draws no favicons, and a couple of kilobytes per feed has no
        // business sitting in a row the list re-reads on every poll.
        let (rows, icons): (Vec<FeedRow>, FeedIcons) = ctx
            .read(|db| {
                let counts = store::unread_counts_by_feed(db.conn()).unwrap_or_default();
                let icons = store::feed_chrome(db.conn())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|chrome| chrome.icon.map(|icon| (chrome.feed_id, icon)))
                    .collect();
                let rows = store::feeds(db.conn())
                    .unwrap_or_default()
                    .iter()
                    .map(|f| FeedRow {
                        id: f.id.get(),
                        title: f.title.clone(),
                        unread: i32::try_from(counts.get(&f.id.get()).copied().unwrap_or(0))
                            .unwrap_or(i32::MAX),
                        error: f.parsing_error_message.clone(),
                        category_id: f.category_id.map(|c| c.get()).unwrap_or(0),
                        crawler: f.crawler,
                        disabled: f.disabled,
                        hide_globally: f.hide_globally,
                    })
                    .collect();
                (rows, icons)
            })
            .unwrap_or_default();

        self.coverFeeds = QString::from(cover_feeds_json(&rows, &icons, COVER_FEEDS));

        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows = rows;
        (self as &mut dyn QAbstractListModel).end_reset_model();
        // Also the NOTIFY of `coverFeeds`, set just above.
        self.countChanged();
    }

    #[must_use]
    pub fn rows(&self) -> &[FeedRow] {
        &self.rows
    }
}

impl QAbstractListModel for FeedModel {
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
            ROLE_ID => row.id.into(),
            ROLE_FEED_TITLE => QString::from(row.title.clone()).into(),
            ROLE_FEED_UNREAD => row.unread.into(),
            ROLE_FEED_ERROR => QString::from(row.error.clone()).into(),
            ROLE_FEED_CATEGORY => row.category_id.into(),
            ROLE_FEED_CRAWLER => row.crawler.into(),
            ROLE_FEED_DISABLED => row.disabled.into(),
            ROLE_FEED_HIDDEN => row.hide_globally.into(),
            _ => QVariant::default(),
        }
    }

    fn role_names(&self) -> HashMap<i32, QByteArray> {
        let mut names = HashMap::new();
        names.insert(ROLE_ID, "feedId".into());
        names.insert(ROLE_FEED_TITLE, "title".into());
        names.insert(ROLE_FEED_UNREAD, "unreadCount".into());
        names.insert(ROLE_FEED_ERROR, "errorMessage".into());
        names.insert(ROLE_FEED_CATEGORY, "categoryId".into());
        names.insert(ROLE_FEED_CRAWLER, "crawler".into());
        names.insert(ROLE_FEED_DISABLED, "feedDisabled".into());
        names.insert(ROLE_FEED_HIDDEN, "hideGlobally".into());
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Notice, SyncSignal};

    /// §a failed refresh reaches the screen, and stops the spinner.
    ///
    /// Reported from a device twice over: a refresh that timed out span
    /// forever AND said nothing. The spinner half is the read side of it --
    /// `syncing` is READ + NOTIFY, and the only emitter of `syncingChanged`
    /// sat BELOW the generation early-return, so a failure that cleared the
    /// running flag without changing the mirror never reached the binding.
    #[test]
    fn a_failure_that_changed_nothing_still_repaints_and_still_speaks() {
        let signal = SyncSignal::default();
        let mut model = EntryModel::default();

        // As the Qt thread does when the user pulls to refresh.
        signal.set_running(true);
        model.seen_running = true;

        // As the worker's guard does on a failed refresh: the mirror did not
        // change, so the generation stays exactly where it was.
        let before = signal.generation();
        signal.set_running(false);
        signal.post(Notice::SyncFailed {
            auth: false,
            message: "could not reach the server".to_owned(),
        });
        assert_eq!(signal.generation(), before, "nothing changed the mirror");

        // The poll must notice the transition anyway.
        model.drain_notice(&signal);
        assert_eq!(model.syncError.to_string(), "could not reach the server");
        assert!(!model.syncErrorIsAuth);

        // A rejected key is reported as such, with no server text: the page
        // supplies its own translated line for that case.
        signal.post(Notice::SyncFailed {
            auth: true,
            message: String::new(),
        });
        model.drain_notice(&signal);
        assert!(model.syncErrorIsAuth);
        assert_eq!(model.syncError.to_string(), "");
    }

    /// §the spinner stops after the refresh that started it.
    ///
    /// Reported from a device: "Refresh works, but the spinner doesn't go
    /// away". `requestSync` raised the running flag and told QML about it, but
    /// did not record that it had -- so the later true-to-false transition
    /// compared false against false and was never reported. The spinner
    /// stopped only when a poll happened to land while the sync was still in
    /// flight.
    #[test]
    fn the_spinner_stops_even_when_no_poll_lands_mid_sync() {
        let signal = SyncSignal::default();
        let mut model = EntryModel::default();

        // What `requestSync` does, minus the worker it has no context for.
        signal.set_running(true);
        model.seen_running = signal.is_running();

        // The worker finishes before any poll runs -- the case that used to
        // leave the spinner up for the life of the process.
        signal.set_running(false);

        let running = signal.is_running();
        assert_ne!(
            running, model.seen_running,
            "the poll must see a transition it can report, or `syncing` keeps \
             the last value QML was told"
        );
    }

    /// A notice this model does not own is left for the page that does.
    #[test]
    fn a_notice_for_another_page_is_put_back() {
        let signal = SyncSignal::default();
        let mut model = EntryModel::default();

        signal.post(Notice::ConnectionTested {
            ok: true,
            message: "alice".to_owned(),
        });
        model.drain_notice(&signal);
        assert_eq!(
            model.syncError.to_string(),
            "",
            "the settings screen's answer is not this page's to consume"
        );
        assert!(
            signal.take_notice().is_some(),
            "and it must still be there for the page that owns it"
        );
    }
}

#[cfg(test)]
mod row_decoration_tests {
    use super::*;
    use vuo_core::model::{Entry, EntryStatus, Feed, FeedId, Icon, IconId, ImageFormat};

    /// A mirror with one feed, one icon and one entry, and a context on it.
    fn seeded() -> (tempfile::TempDir, std::rc::Rc<AppContext>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("m.sqlite");
        let mut db = vuo_core::db::Database::open(&path).expect("mirror");

        // A one-pixel PNG, so the format sniffing in the mirror is exercised
        // rather than bypassed.
        let png: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52,
        ];

        db.with_tx(|tx| {
            store::upsert_icon(
                tx,
                &Icon {
                    id: IconId(5),
                    format: ImageFormat::Png,
                    bytes: png.clone(),
                    dimensions: Some((1, 1)),
                },
            )?;
            store::upsert_feed(
                tx,
                &Feed {
                    id: FeedId(1),
                    category_id: None,
                    title: "Tagesschau".to_owned(),
                    site_url: None,
                    feed_url: None,
                    icon_id: Some(IconId(5)),
                    checked_at: None,
                    parsing_error_message: String::new(),
                    parsing_error_count: 0,
                    disabled: false,
                    hide_globally: false,
                    crawler: true,
                },
                1,
            )?;
            store::upsert_entry(
                tx,
                &Entry {
                    id: EntryId(7),
                    feed_id: FeedId(1),
                    status: EntryStatus::Unread,
                    starred: false,
                    title: "Island".to_owned(),
                    url: None,
                    comments_url: None,
                    author: String::new(),
                    content: String::new(),
                    published_at: None,
                    created_at: None,
                    changed_at: None,
                    reading_time: 2,
                    tags: Vec::new(),
                    enclosures: Vec::new(),
                },
                1,
            )
        })
        .expect("seed");

        let signal = std::sync::Arc::new(crate::context::SyncSignal::default());
        let instance = url::Url::parse("https://miniflux.example/").expect("url");
        let worker = crate::worker::Worker::spawn(
            path.clone(),
            instance.clone(),
            vuo_core::redact::ApiToken::new("t"),
            vuo_core::api::TransportConfig::default(),
            std::sync::Arc::clone(&signal),
            |_| {},
        );
        let ctx = AppContext::new(db, worker, instance, signal, 0);
        (dir, ctx)
    }

    /// §an entry row carries the feed it came from.
    ///
    /// Reported from a device: the list showed the age but neither the feed's
    /// name nor its icon, so every row looked like it came from nowhere.
    #[test]
    fn an_entry_row_carries_its_feed_name_and_icon() {
        let (_dir, ctx) = seeded();
        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);

        assert_eq!(model.row_count(), 1, "the seeded entry is unread");
        let row = model.rows().first().expect("one row");
        assert_eq!(
            row.feed_name, "Tagesschau",
            "the row must name the feed it came from"
        );
        assert!(
            row.feed_icon.starts_with("data:image/png;base64,"),
            "and carry its icon as a data URI, got {:?}",
            row.feed_icon
        );

        // The names are half the contract: the delegate reaches these by the
        // bare word, and a role that is present in Rust but unnamed here is
        // simply `undefined` in QML.
        let names = <EntryModel as QAbstractListModel>::role_names(&model);
        for (role, name) in [(ROLE_FEED_NAME, "feedName"), (ROLE_FEED_ICON, "feedIcon")] {
            assert_eq!(
                names.get(&role).map(std::string::ToString::to_string),
                Some(name.to_owned()),
                "QML reaches this role by name"
            );
        }
    }

    /// §the cover draws the feeds, so the model has to hand them over.
    ///
    /// The cover repeats the feeds it is given to fill its grid, which a view
    /// over rows cannot do, so they cross into QML as JSON instead. What this
    /// pins is that the JSON is built FROM THE MIRROR -- names, counts and
    /// icon bytes -- and not from whatever the feed list happened to hold.
    #[test]
    fn the_cover_gets_every_feed_with_its_icon_and_its_unread_count() {
        let (_dir, ctx) = seeded();

        // A second feed with no icon at all, and a third with more unread
        // than the first. Ids ascending, so the mirror's own order is 1, 2, 3
        // and the ordering below is the model's doing rather than SQLite's.
        let quiet = Feed {
            id: FeedId(2),
            category_id: None,
            title: "Aamulehti".to_owned(),
            site_url: None,
            feed_url: None,
            icon_id: None,
            checked_at: None,
            parsing_error_message: String::new(),
            parsing_error_count: 0,
            disabled: false,
            hide_globally: false,
            crawler: false,
        };
        let loudest = Feed {
            id: FeedId(3),
            title: "LWN".to_owned(),
            ..quiet.clone()
        };
        let entry = |id: i64, feed: i64| Entry {
            id: EntryId(id),
            feed_id: FeedId(feed),
            status: EntryStatus::Unread,
            starred: false,
            title: "Something".to_owned(),
            url: None,
            comments_url: None,
            author: String::new(),
            content: String::new(),
            published_at: None,
            created_at: None,
            changed_at: None,
            reading_time: 1,
            tags: Vec::new(),
            enclosures: Vec::new(),
        };
        ctx.write(|db| {
            db.with_tx(|tx| {
                store::upsert_feed(tx, &quiet, 1)?;
                store::upsert_feed(tx, &loudest, 1)?;
                store::upsert_entry(tx, &entry(8, 3), 1)?;
                store::upsert_entry(tx, &entry(9, 3), 1)
            })
        })
        .expect("the mirror")
        .expect("seed the other feeds");

        let mut model = FeedModel::default();
        model.attach(std::rc::Rc::clone(&ctx));

        let json = model.coverFeeds.to_string();
        let feeds: Vec<serde_json::Value> =
            serde_json::from_str(&json).unwrap_or_else(|e| panic!("{json:?} is not JSON: {e}"));
        assert_eq!(feeds.len(), 3, "every feed reaches the cover: {json}");

        let ids: Vec<i64> = feeds.iter().filter_map(|f| f["feedId"].as_i64()).collect();
        assert_eq!(
            ids,
            vec![3, 1, 2],
            "whatever has the most unread must come first -- the grid draws \
             only its first cells' worth, so a feed past them would be lit \
             where nobody could see it: {json}"
        );
        let counts: Vec<i64> = feeds.iter().filter_map(|f| f["unread"].as_i64()).collect();
        assert_eq!(counts, vec![2, 1, 0], "the counts come from the mirror");
        assert_eq!(
            feeds[0]["title"].as_str(),
            Some("LWN"),
            "the cover draws a letter from the title, so it has to be there"
        );
        assert!(
            feeds[1]["icon"]
                .as_str()
                .is_some_and(|icon| icon.starts_with("data:image/png;base64,")),
            "the icon must arrive as the data: URI an Image can draw \
             without touching the network: {json}"
        );
        assert_eq!(
            feeds[0]["icon"].as_str(),
            Some(""),
            "a feed the mirror has no icon for says so with an empty string, \
             which is what makes the cover fall back to its initial"
        );
    }

    /// The cap, which nothing else can see.
    ///
    /// `COVER_FEEDS` bounds what is encoded and carried across; the ordering
    /// above is what makes it safe. Both are one function, so this drives it
    /// directly rather than seeding thirty-three feeds into a mirror.
    #[test]
    fn the_cover_is_told_about_only_as_many_feeds_as_it_can_draw() {
        let rows: Vec<FeedRow> = (1..=10)
            .map(|id| FeedRow {
                id,
                title: format!("Feed {id}"),
                unread: i32::try_from(id).unwrap_or(0),
                ..FeedRow::default()
            })
            .collect();
        let icons = HashMap::new();

        let json = cover_feeds_json(&rows, &icons, 3);
        let feeds: Vec<serde_json::Value> =
            serde_json::from_str(&json).unwrap_or_else(|e| panic!("{json:?} is not JSON: {e}"));
        let ids: Vec<i64> = feeds.iter().filter_map(|f| f["feedId"].as_i64()).collect();
        assert_eq!(
            ids,
            vec![10, 9, 8],
            "the cap must keep the feeds with something new, not the first \
             three the mirror happened to return: {json}"
        );

        assert_eq!(
            cover_feeds_json(&[], &icons, 3),
            "[]",
            "no feeds must still parse; the cover would otherwise take the \
             catch branch on every reload"
        );
    }

    /// §the feed list exposes the settings the editor writes.
    ///
    /// Reported from a device: "Feed settings" did nothing at all. The menu
    /// item passes these three roles straight into the pushed page, so a role
    /// that does not resolve makes the whole push throw.
    #[test]
    fn a_feed_row_carries_the_settings_the_editor_edits() {
        let (_dir, ctx) = seeded();
        let mut model = FeedModel::default();
        model.attach(std::rc::Rc::clone(&ctx));

        assert_eq!(model.row_count(), 1);
        let row = model.rows().first().expect("one row");
        assert!(row.crawler, "the seeded feed has the crawler on");
        assert!(!row.disabled);
        assert!(!row.hide_globally);
        let names = <FeedModel as QAbstractListModel>::role_names(&model);
        for (role, name) in [
            (ROLE_FEED_CRAWLER, "crawler"),
            (ROLE_FEED_DISABLED, "feedDisabled"),
            (ROLE_FEED_HIDDEN, "hideGlobally"),
        ] {
            assert_eq!(
                names.get(&role).map(|n| n.to_string()),
                Some(name.to_owned()),
                "QML reaches this role by name, and the editor's push needs it"
            );
        }
    }
}
