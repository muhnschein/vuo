//! `QAbstractListModel` adapters over local mirror.
//!
//! §5: *the local SQLite mirror is the single source of truth for the UI [...]
//! sync writes to SQLite; models observe SQLite.* Models hold no authority:
//! cache of mirror rows, refreshed by [`reload`](EntryModel::reload) when
//! mirror changed. Never talk to network.
//!
//! # Thread affinity
//!
//! Everything runs on Qt thread. Worker never touches `QObject`: writes SQLite,
//! bumps [`crate::context::SyncSignal`] generation; models notice by POLLING it
//! from `pollSync`, driven by QML `Timer`.
//!
//! No callback into model, can't easily be: QML constructs these, Rust holds no
//! handle.
//!
//! # Roles, not rich text
//!
//! Roles expose *data*; QML decides drawing. Titles and authors plain strings;
//! QML sets `textFormat: Text.PlainText` (§9.3). Body text pre-escaped by
//! [`crate::article`] (only place markup generated); QML sets `Text.StyledText`
//! there.

use std::collections::{HashMap, HashSet};

use qmetaobject::{
    qt_base_class, qt_method, qt_property, qt_signal, QAbstractListModel, QByteArray, QMetaType,
    QModelIndex, QObject, QString, QVariant, QVariantList, USER_ROLE,
};
use vuo_core::db::store;
#[allow(unused_imports)]
use vuo_core::model::FeedId;
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::search::{MatchKind, SearchQuery};

use crate::context::AppContext;
use crate::worker::{self, Command};

/// Rows loaded into list model at once, and what `loadMore` adds.
///
/// List = window onto mirror. Anything acting on "everything in this scope"
/// queries database, doesn't iterate loaded rows.
///
/// FOUR models alive (three tabs, browse list); every generation bump (each
/// sync, each local mark-read) re-runs each query, builds, sorts, compares
/// rows. Phone shows ~ten rows.
///
/// Cost = CPU and resident memory; on Sailfish resident memory decides whether
/// app survives until reader returns. Killed process pays cold start, far worse
/// than any reload saved.
///
/// 150 ≈ fifteen screens: end reached by scrolling, not accident; `loadMore`
/// extends by a page.
pub const PAGE_SIZE: i64 = 150;

/// Results per group a search shows at first.
///
/// Per group, not one window: uninteresting group mustn't push next off screen;
/// unshown rows never read (see [`store::search_hits`]).
pub const SEARCH_FIRST_PAGE: usize = 5;
/// What each "Load more" under group adds, in turn. After last, next loads
/// rest: reader that deep is after something not near top.
pub const SEARCH_LOAD_STEPS: [usize; 2] = [10, 20];

/// What one group of search results shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SearchGroup {
    /// How many of group shown: [`SEARCH_FIRST_PAGE`] at start, `usize::MAX`
    /// once reader asked for all.
    shown: usize,
    /// How many times the reader has asked for more.
    loads: u8,
}

impl Default for SearchGroup {
    fn default() -> Self {
        SearchGroup {
            shown: SEARCH_FIRST_PAGE,
            loads: 0,
        }
    }
}

impl SearchGroup {
    /// How many the next "Load more" adds, or `None` when it loads the rest.
    fn next_load(self) -> Option<usize> {
        SEARCH_LOAD_STEPS.get(usize::from(self.loads)).copied()
    }

    fn load_more(&mut self) {
        self.shown = match self.next_load() {
            Some(step) => self.shown.saturating_add(step),
            None => usize::MAX,
        };
        self.loads = self.loads.saturating_add(1);
    }
}

/// The three groups of a search, one per [`MatchKind`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SearchGroups {
    title: SearchGroup,
    feed: SearchGroup,
    text: SearchGroup,
}

impl SearchGroups {
    fn get(&self, kind: MatchKind) -> SearchGroup {
        match kind {
            MatchKind::Title => self.title,
            MatchKind::Feed => self.feed,
            MatchKind::Text => self.text,
        }
    }

    fn get_mut(&mut self, kind: MatchKind) -> &mut SearchGroup {
        match kind {
            MatchKind::Title => &mut self.title,
            MatchKind::Feed => &mut self.feed,
            MatchKind::Text => &mut self.text,
        }
    }

    /// Matches list shows: first [`SearchGroup::shown`] of each group, in found
    /// order.
    fn pick(&self, found: &[store::SearchMatch]) -> Vec<store::SearchMatch> {
        let mut room = *self;
        found
            .iter()
            .filter(|m| {
                let group = room.get_mut(m.kind);
                let fits = group.shown > 0;
                group.shown = group.shown.saturating_sub(1);
                fits
            })
            .copied()
            .collect()
    }
}

/// The group a QML `matchKind` names.
fn match_kind_from_qml(kind: &str) -> Option<MatchKind> {
    match kind {
        "title" => Some(MatchKind::Title),
        "feed" => Some(MatchKind::Feed),
        "text" => Some(MatchKind::Text),
        _ => None,
    }
}

/// Model roles for [`EntryModel`], one per [`EntryRow`] field; model's
/// `role_names` gives QML names.
///
/// `entryId`: [`EntryRow::id`]. Also `feedId` on [`FeedModel`], sharing this
/// number.
pub const ROLE_ID: i32 = USER_ROLE;
/// `title`: [`EntryRow::title`]. FOREIGN TEXT: `Text.PlainText`.
pub const ROLE_TITLE: i32 = USER_ROLE + 1;
/// `author`: [`EntryRow::author`]. FOREIGN TEXT: `Text.PlainText`.
pub const ROLE_AUTHOR: i32 = USER_ROLE + 2;
/// `unread`: [`EntryRow::unread`].
pub const ROLE_UNREAD: i32 = USER_ROLE + 3;
/// `starred`: [`EntryRow::starred`].
pub const ROLE_STARRED: i32 = USER_ROLE + 4;
/// `feedId`: [`EntryRow::feed_id`].
pub const ROLE_FEED_ID: i32 = USER_ROLE + 5;
/// `published`: [`EntryRow::published`].
pub const ROLE_PUBLISHED: i32 = USER_ROLE + 6;
/// `readingTime`: [`EntryRow::reading_time`].
pub const ROLE_READING_TIME: i32 = USER_ROLE + 7;
/// `url`: [`EntryRow::url`].
pub const ROLE_URL: i32 = USER_ROLE + 8;
/// Entry's feed name. FOREIGN TEXT: `Text.PlainText`.
pub const ROLE_FEED_NAME: i32 = USER_ROLE + 9;
/// `data:` URI for feed icon; empty when mirror has none.
pub const ROLE_FEED_ICON: i32 = USER_ROLE + 10;
/// Where search result matched: `title`, `feed`, `text` (group listed under).
/// Empty outside search. `feed` row = FEED, not article: see
/// `EntryRow::feed_found`.
pub const ROLE_MATCH_KIND: i32 = USER_ROLE + 11;
/// Search result title with terms bold. `StyledText` built and ESCAPED in Rust
/// (`SearchQuery::highlight`); QML may render as such (§9.3). Empty outside
/// search.
pub const ROLE_TITLE_STYLED: i32 = USER_ROLE + 12;
/// Feed name, likewise. Empty outside search.
pub const ROLE_FEED_NAME_STYLED: i32 = USER_ROLE + 13;
/// Body-text result: lines around match, terms bold; `StyledText` as above.
/// Empty for other rows.
pub const ROLE_EXCERPT: i32 = USER_ROLE + 14;
/// Feed found by search: unread count as feed list shows. Zero for other rows.
pub const ROLE_UNREAD_COUNT: i32 = USER_ROLE + 15;

/// Row as UI needs it.
///
/// Neither [`vuo_core::model::Entry`] nor copy of feed chrome. Body stays in
/// SQLite; feed name and icon looked up by `feed_id` when delegate asks (see
/// `EntryModel::chrome_for`), not copied in.
///
/// Icon is why: base64 `data:` URI, ordinary 6 KB favicon = 8 KB text, fetcher
/// accepts up to 512 KB. Per-row copy = 8 KB × rows × four models (16 MB on
/// reported mirror, worst case 100× more) to hold ≤ 56 distinct strings, and
/// compared byte by byte every poll (`publish_rows` diffs whole rows).
///
/// `PartialEq`: reload tells moved row from re-read unchanged one (see
/// [`EntryModel::reload`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryRow {
    /// Server's entry id; for feed found by search, feed id negated (see
    /// `feed_found`).
    pub id: i64,
    /// The feed the entry belongs to.
    pub feed_id: i64,
    /// FOREIGN TEXT: the entry's title.
    pub title: String,
    /// FOREIGN TEXT: the entry's author, or empty.
    pub author: String,
    /// Whether it is unread, with any pending local change applied.
    pub unread: bool,
    /// Whether it is starred.
    pub starred: bool,
    /// When the feed says it was published, in Unix seconds; 0 when unknown.
    pub published: i64,
    /// The server's reading-time estimate, in minutes.
    pub reading_time: i32,
    /// The article's link, or empty when there is none.
    pub url: String,
    /// What search result shows of why found. Empty strings (no allocation) on
    /// other lists. See [`ROLE_MATCH_KIND`] and following roles.
    pub match_kind: &'static str,
    /// See [`ROLE_TITLE_STYLED`].
    pub title_styled: String,
    /// See [`ROLE_FEED_NAME_STYLED`].
    pub feed_styled: String,
    /// See [`ROLE_EXCERPT`].
    pub excerpt: String,
    /// See [`ROLE_UNREAD_COUNT`].
    pub unread_count: i64,
}

impl EntryRow {
    /// Search result row: list row, plus why found, marked up for reader.
    fn found(hit: &store::SearchHit, query: Option<&SearchQuery>) -> Self {
        let mut row = EntryRow::from(&hit.row);
        row.match_kind = match hit.kind {
            MatchKind::Title => "title",
            MatchKind::Feed => "feed",
            MatchKind::Text => "text",
        };
        if let Some(query) = query {
            row.title_styled = query.highlight(&row.title);
            row.feed_styled = query.highlight(&hit.feed_title);
            row.excerpt = hit
                .excerpt
                .as_deref()
                .map(|text| query.highlight(text))
                .unwrap_or_default();
        }
        row
    }
}

impl EntryRow {
    /// FEED a search found by name: row in `feed` group, drawn as feed list
    /// draws feed, opens that feed, not article.
    ///
    /// `id` = feed id NEGATED. Row id is what reload tells rows apart by (see
    /// [`publish_rows`]); feeds and articles numbered separately; article id
    /// never below one. Nothing reads it as article: list offers feed row
    /// neither article menu nor page.
    fn feed_found(feed: &store::FeedMatch, query: Option<&SearchQuery>) -> Self {
        EntryRow {
            id: feed.id.get().saturating_neg(),
            feed_id: feed.id.get(),
            title: feed.title.clone(),
            // Feed not read or unread; drawn like unread row, full strength, as
            // in feed list.
            unread: true,
            match_kind: "feed",
            title_styled: query.map(|q| q.highlight(&feed.title)).unwrap_or_default(),
            unread_count: feed.unread,
            ..EntryRow::default()
        }
    }
}

impl From<&store::EntryListRow> for EntryRow {
    fn from(e: &store::EntryListRow) -> Self {
        EntryRow {
            id: e.id.get(),
            feed_id: e.feed_id.get(),
            title: e.title.clone(),
            author: e.author.clone(),
            unread: !e.status.is_read(),
            starred: e.starred,
            published: e.published_at.unwrap_or(0),
            reading_time: e.reading_time,
            url: e.url.clone().unwrap_or_default(),
            ..EntryRow::default()
        }
    }
}

/// List model re-reading rows from mirror wholesale, swapping result in.
///
/// Both models work so; both must keep reader's place when re-read finds held
/// list. Rule subtle enough to live in exactly one place: [`publish_rows`],
/// reason this trait exists.
trait RowList: QAbstractListModel + Sized {
    /// `PartialEq`: re-read unchanged row told from moved one.
    type Row: PartialEq;

    fn rows_held(&self) -> &[Self::Row];
    fn rows_held_mut(&mut self) -> &mut Vec<Self::Row>;
    /// What identifies row across reload: mirror's own key.
    fn row_id(row: &Self::Row) -> i64;
    /// Emit model's `countChanged`. `qt_signal` = inherent method generated per
    /// struct; trait can't name it.
    fn emit_count_changed(&mut self);
}

/// Swap freshly read rows in, resetting model ONLY when held rows actually
/// changed.
///
/// `begin_reset_model` re-lays view from scratch; plain `ListView` returns to
/// top, throwing reader off scroll position. `dataChanged` keeps layout and
/// scroll, only re-reads roles.
///
/// Reader's own bump makes this matter: row's "Mark as read" patches row at tap
/// ([`EntryModel::mark_row`]), sends [`Command::FlushOutbox`]; worker bumps
/// generation when server confirms (for settings page's pending count, nothing
/// lists show). Poll then re-reads same rows; resetting would jump list to top
/// under thumb.
///
/// Test = ids in order, NOT equal rows: row whose STATE changed (read or
/// starred elsewhere, or by sync) redrawn in place, as [`EntryModel::mark_row`]
/// does locally. Rows arriving, leaving, moving = different list, resets.
///
/// Returns true when model RESET (what costs reader their place).
fn publish_rows<M: RowList>(model: &mut M, fresh: Vec<M::Row>) -> bool {
    publish_rows_as(model, fresh, Publish::Reset)
}

/// How [`publish_rows_as`] tells view about different row set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// Model reset, as every list but search.
    Reset,
    /// Never reset: single run of ADDED rows, all held rows still there in
    /// place, is inserted; else every held row removed, every fresh one
    /// inserted.
    ///
    /// For search. Run = shape of group's "Load more"; reset would throw reader
    /// to top from bottom of group. Any reset (view starting over) also hands
    /// focus to first row, taking it and keyboard off search field in view
    /// header on every typing pause.
    Search,
}

/// [`publish_rows`], told how different rows reach view. Inserted range from
/// [`inserted_run`], tested separately.
fn publish_rows_as<M: RowList>(model: &mut M, fresh: Vec<M::Row>, how: Publish) -> bool {
    let held_ids: Vec<i64> = model.rows_held().iter().map(M::row_id).collect();
    let fresh_ids: Vec<i64> = fresh.iter().map(M::row_id).collect();
    let run = if how == Publish::Search {
        inserted_run(&held_ids, &fresh_ids)
    } else {
        None
    };
    if let Some((at, added)) = run {
        let first = i32::try_from(at).ok();
        let last = at
            .checked_add(added)
            .and_then(|end| end.checked_sub(1))
            .and_then(|end| i32::try_from(end).ok());
        if let (Some(first), Some(last)) = (first, last) {
            // Whether already-held row changed state, as same-rows case below.
            let held_after = fresh
                .get(..at)
                .into_iter()
                .flatten()
                .chain(fresh.get(at.saturating_add(added)..).into_iter().flatten());
            let redraw = !model.rows_held().iter().eq(held_after);
            model.begin_insert_rows(first, last);
            *model.rows_held_mut() = fresh;
            model.end_insert_rows();
            if redraw {
                if let Some(end) = model
                    .rows_held()
                    .len()
                    .checked_sub(1)
                    .and_then(|n| i32::try_from(n).ok())
                {
                    let top = model.row_index(0);
                    let bottom = model.row_index(end);
                    model.data_changed(top, bottom);
                }
            }
            model.emit_count_changed();
            return false;
        }
    }

    let same_rows = model.rows_held().len() == fresh.len()
        && model
            .rows_held()
            .iter()
            .zip(&fresh)
            .all(|(held, new)| M::row_id(held) == M::row_id(new));

    if same_rows {
        // Announced only when something moved. Entry delegate rebuilds detail
        // line from font metrics every `dataChanged`; this runs every bump.
        let redraw = model.rows_held() != fresh.as_slice();
        *model.rows_held_mut() = fresh;
        if redraw {
            // No-op on empty model: `row_index` of absent row isn't index view
            // can be told.
            if let Some(last) = model
                .rows_held()
                .len()
                .checked_sub(1)
                .and_then(|n| i32::try_from(n).ok())
            {
                let first = model.row_index(0);
                let last = model.row_index(last);
                model.data_changed(first, last);
            }
        }
        // Emitted either way: `unreadTotal` is mirror's, can move while every
        // row here stays.
        model.emit_count_changed();
        return false;
    }

    if how == Publish::Search {
        // Out, then in: two changes view applies to held rows, where reset
        // starts over.
        if let Some(last) = held_ids
            .len()
            .checked_sub(1)
            .and_then(|n| i32::try_from(n).ok())
        {
            model.begin_remove_rows(0, last);
            model.rows_held_mut().clear();
            model.end_remove_rows();
        }
        if let Some(last) = fresh
            .len()
            .checked_sub(1)
            .and_then(|n| i32::try_from(n).ok())
        {
            model.begin_insert_rows(0, last);
            *model.rows_held_mut() = fresh;
            model.end_insert_rows();
        }
        model.emit_count_changed();
        return false;
    }

    model.begin_reset_model();
    *model.rows_held_mut() = fresh;
    model.end_reset_model();
    model.emit_count_changed();
    true
}

/// Where `fresh` = `held` plus one run, nothing else changed: run's start index
/// in `fresh` and length. `None` otherwise (rows left, moved, arrived in
/// several places) and for no change.
fn inserted_run(held: &[i64], fresh: &[i64]) -> Option<(usize, usize)> {
    let added = fresh.len().checked_sub(held.len()).filter(|&n| n > 0)?;
    let before = held.iter().zip(fresh).take_while(|(a, b)| a == b).count();
    let after = held
        .iter()
        .rev()
        .zip(fresh.iter().rev())
        .take(held.len().saturating_sub(before))
        .take_while(|(a, b)| a == b)
        .count();
    (before.saturating_add(after) == held.len()).then_some((before, added))
}

/// Which slice of the mirror a model shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Unread entries.
    Unread,
    /// Starred entries.
    Starred,
    /// Every entry in the mirror.
    All,
    /// One feed's entries, by its id.
    Feed(i64),
    /// One category's entries, by its id.
    Category(i64),
    /// Whatever model's search text matches; see `EntryModel::setSearch`. Text
    /// on model, not here: `Scope` stays `Copy`, re-scoping a searching model
    /// keeps typed text.
    Search,
}

impl Scope {
    /// Store filter for scope. `None` for search: not a column filter, has own
    /// query.
    fn to_filter(self) -> Option<store::EntryFilter> {
        match self {
            Scope::Unread => Some(store::EntryFilter::Unread),
            Scope::Starred => Some(store::EntryFilter::Starred),
            Scope::All => Some(store::EntryFilter::All),
            Scope::Feed(id) => Some(store::EntryFilter::Feed(id)),
            Scope::Category(id) => Some(store::EntryFilter::Category(id)),
            Scope::Search => None,
        }
    }

    /// Decode integer pair QML passes. No `QEnum` registration on Qt 5.6 here:
    /// scope crosses as two ints.
    #[must_use]
    pub fn from_qml(kind: i32, id: i64) -> Scope {
        match kind {
            1 => Scope::Starred,
            2 => Scope::All,
            3 => Scope::Feed(id),
            4 => Scope::Category(id),
            5 => Scope::Search,
            _ => Scope::Unread,
        }
    }
}

/// Entry list: rows of one [`Scope`], re-read when worker changes mirror.
// Bools are `qt_property!`s, each own setting/state QML binds by name; bitfield
// hides them from QML.
#[allow(clippy::struct_excessive_bools)]
#[derive(QObject, Default)]
pub struct EntryModel {
    base: qt_base_class!(trait QAbstractListModel),

    count: qt_property!(i32; READ row_count NOTIFY countChanged),
    countChanged: qt_signal!(),
    /// True once scope set: when `count` starts meaning something. "Nothing to
    /// read" placeholder reads this; gated on `count` alone it shows at
    /// construction (every model empty) then fades over rows arriving a moment
    /// later.
    ready: qt_property!(bool; READ is_ready NOTIFY countChanged),

    /// Set scope, load fresh list. `kind`: 0 unread, 1 starred, 2 all, 3 feed
    /// (id), 4 category (id).
    setScope: qt_method!(fn(&mut self, kind: i32, id: i64)),
    /// Re-read the mirror, keeping the rows on screen. See [`reload`].
    refresh: qt_method!(fn(&mut self)),
    entryIdAt: qt_method!(fn(&self, row: i32) -> i64),

    /// Mark entry at `row` read or unread.
    ///
    /// Applied to mirror at once and queued for server: works offline, list
    /// updates same frame as tap.
    setRead: qt_method!(fn(&mut self, row: i32, read: bool)),
    /// Mark many entries read or unread at once, by id.
    ///
    /// One transaction, one outbox flush for lot (selection mode); forty
    /// `setRead`s = forty transactions, bumps, flush commands. Ids, not rows:
    /// selection = set of articles; rows shift under reload. Ids not in mirror
    /// skipped; held rows patched in place like `setRead`, list doesn't scroll
    /// away.
    setReadMany: qt_method!(fn(&mut self, ids: QVariantList, read: bool)),
    setStarred: qt_method!(fn(&mut self, row: i32, starred: bool)),
    /// Ask worker for NETWORK sync. MANUAL REFRESH: only thing taking finished
    /// row off list.
    ///
    /// Distinct from `refresh` (re-reads local mirror only). Only UI sender of
    /// `Command::Sync`.
    requestSync: qt_method!(fn(&mut self)),
    /// Total unread across mirror, for cover. NOT `count` (rows this model
    /// shows, capped by page size).
    unreadTotal: qt_property!(i32; READ unread_total NOTIFY countChanged),
    /// True while sync in flight. Read from worker's signal, not set locally:
    /// clears when worker actually finishes.
    syncing: qt_property!(bool; READ is_syncing NOTIFY syncingChanged),
    syncingChanged: qt_signal!(),

    /// Why last refresh failed; empty when fine.
    ///
    /// State on model, not one-shot signal: failure must survive being produced
    /// on cover (cover has own Refresh, no page to receive signal) and on
    /// non-visible page. FOREIGN TEXT (server's words): page renders
    /// `PlainText`.
    pub syncError: qt_property!(QString; NOTIFY syncStateChanged),
    /// True when failure = server rejecting API key.
    ///
    /// Page shows fixed translated line, offers Settings; `syncError` empty
    /// then: server phrasing for bad key not actionable.
    pub syncErrorIsAuth: qt_property!(bool; NOTIFY syncStateChanged),
    syncStateChanged: qt_signal!(),
    /// Poll for worker activity. From QML Timer.
    ///
    /// Poll, not push: QML owns objects, Rust has no live-model list;
    /// cross-thread pointer registry untestable without device. Returns true if
    /// anything reloaded.
    pollSync: qt_method!(fn(&mut self) -> bool),
    /// Mark everything in current scope read.
    ///
    /// Expanded locally into entry ids, not server bulk endpoint (`published_at
    /// < now()` cut-off at request time would mark entries user never saw).
    markAllRead: qt_method!(fn(&mut self)),
    /// Mark everything in EXPLICIT scope read.
    ///
    /// `markAllRead` reads scope model is in when remorse countdown FIRES, not
    /// when user tapped. Swiping tabs re-scopes without page change: arming on
    /// Unread, swiping to All within countdown would mark whole mirror. Scope
    /// passed in binds action to tab showing when armed.
    markAllReadIn: qt_method!(fn(&mut self, kind: i32, id: i64)),
    /// True when mirror holds more rows in scope than loaded.
    ///
    /// List binds "load next page" trigger to this: list showing everything
    /// does nothing at end.
    pub hasMore: qt_property!(bool; NOTIFY hasMoreChanged),
    hasMoreChanged: qt_signal!(),
    /// Extend window by another [`PAGE_SIZE`].
    ///
    /// From list when reader reaches loaded end. No-op when nothing more:
    /// over-eager binding costs comparison, not query.
    loadMore: qt_method!(fn(&mut self)),
    /// Set what [`Scope::Search`] model looks for.
    ///
    /// Kept whatever scope, applied at once when searching. Same text = no-op:
    /// re-firing binding costs comparison, not query.
    setSearch: qt_method!(fn(&mut self, text: QString)),
    /// Results per group ([`MatchKind`]) across whole mirror, not only loaded
    /// rows: articles in `title` and `text`, feeds in `feed`. Group header
    /// text. Zero outside search.
    titleMatches: qt_property!(i32; READ title_matches NOTIFY matchesChanged),
    feedMatches: qt_property!(i32; READ feed_matches NOTIFY matchesChanged),
    textMatches: qt_property!(i32; READ text_matches NOTIFY matchesChanged),
    /// How many of each group list shows: header's "5 of 40". See
    /// [`SEARCH_FIRST_PAGE`].
    titleShown: qt_property!(i32; READ title_shown NOTIFY matchesChanged),
    feedShown: qt_property!(i32; READ feed_shown NOTIFY matchesChanged),
    textShown: qt_property!(i32; READ text_shown NOTIFY matchesChanged),
    /// How many group's next "Load more" adds (button text); 0 when it loads
    /// rest. See [`SEARCH_LOAD_STEPS`].
    titleNextLoad: qt_property!(i32; READ title_next_load NOTIFY matchesChanged),
    feedNextLoad: qt_property!(i32; READ feed_next_load NOTIFY matchesChanged),
    textNextLoad: qt_property!(i32; READ text_next_load NOTIFY matchesChanged),
    matchesChanged: qt_signal!(),
    /// Show more of one search group: `title`, `feed`, `text` as `matchKind`
    /// names. Shown rows stay; new ones inserted under, only they read.
    loadMoreIn: qt_method!(fn(&mut self, kind: QString)),

    rows: Vec<EntryRow>,
    /// Every match of last search, by id: shown rows picked from these, read
    /// when shown. See [`store::search_matches`].
    search_found: Vec<store::SearchMatch>,
    /// Every feed last search found by name. Held whole: name and count each,
    /// few dozen feeds. See [`store::search_feeds`].
    search_feeds: Vec<store::FeedMatch>,
    /// See `titleMatches`: counted once per search, not per read.
    matches: store::SearchCounts,
    /// How much of each group shown. Back to one page each on new search.
    groups: SearchGroups,
    scope: Option<Scope>,
    /// What a [`Scope::Search`] model looks for. See [`EntryModel::setSearch`].
    search: String,
    /// Rows model currently asked to hold.
    ///
    /// Zero until first reload (`Default`), hence
    /// [`window`](EntryModel::window) reads it, not field directly. Grows by
    /// [`PAGE_SIZE`] per [`loadMore`](EntryModel::loadMore); back to one page
    /// on any reload starting list over (new scope, manual refresh).
    limit: i64,
    /// The worker generation this model last reloaded at.
    seen_generation: u64,
    /// Manual-refresh epoch rows loaded under. Reload under newer one starts
    /// from nothing, doesn't keep rows on screen; see
    /// [`reload`](EntryModel::reload).
    seen_refresh_epoch: u64,
    /// Feed name and icon URI per feed id, shared with every list.
    ///
    /// Handle on [`AppContext::feed_chrome`] (map built ≤ once per generation
    /// for all four models). Held, not fetched in `data`: `data` is `&self`,
    /// called per role per visible delegate.
    feed_chrome: std::rc::Rc<HashMap<i64, crate::context::FeedChrome>>,
    /// Spinner state last told to QML.
    ///
    /// `syncing` is READ + NOTIFY: QML re-evaluates only on `syncingChanged`.
    /// Failure clearing flag without changing mirror must still reach binding;
    /// tracking here fires signal on transition itself, whatever generation
    /// did.
    seen_running: bool,
    /// `None` until [`EntryModel::attach`] called from Rust; QML never
    /// constructs this.
    ctx: Option<std::rc::Rc<AppContext>>,
}

impl EntryModel {
    /// Give model context explicitly. Tests only; QML-built model uses
    /// installed context.
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
        self.reload();
    }

    /// App context: explicitly attached (tests), else global installed at
    /// start-up. QML constructs these: no constructor to pass it through.
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
            // Send opportunistically: no signal → intent stays in outbox, next
            // sync carries it.
            ctx.send(Command::FlushOutbox);
        }
    }

    fn setReadMany(&mut self, ids: QVariantList, read: bool) {
        let Some(ctx) = self.context() else { return };
        // JS numbers arrive as doubles; id past 2^53 never issued by Miniflux;
        // non-numbers dropped, not guessed.
        let ids: Vec<EntryId> = (&ids)
            .into_iter()
            .filter_map(|v| i64::from_qvariant(v.clone()))
            .map(EntryId)
            .collect();
        if ids.is_empty() {
            return;
        }
        let status = if read {
            EntryStatus::Read
        } else {
            EntryStatus::Unread
        };
        let applied = ctx
            .write(|db| worker::apply_local_status_bulk(db, &ids, status))
            .transpose()
            .ok()
            .flatten()
            .is_some();
        if applied {
            for id in &ids {
                self.mark_row(*id, Some(!read), None);
            }
            self.announce_local_change(&ctx);
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

    /// Tell OTHER models this one changed mirror.
    ///
    /// One model per scope tab, all reading same entries. Mark read on Unread
    /// must reach All's copy of row, else swiping shows it unread until next
    /// network sync.
    ///
    /// Generation bumped, then this model's cursor moved PAST bump. Else
    /// mutating model reloads itself next poll: full reset, list scrolls to top
    /// under tapping finger. `mark_row` already patched own row; bump is for
    /// everyone else.
    fn announce_local_change(&mut self, ctx: &AppContext) {
        ctx.signal().bump();
        self.seen_generation = ctx.signal().generation();
    }

    fn requestSync(&mut self) {
        // `or_else`, not bare `context()`: context built at start-up from
        // stored account; first run has none. Without second chance, pulley
        // Refresh does nothing all session in which account first configured.
        let Some(ctx) = self.context().or_else(crate::context::refresh_current) else {
            return;
        };
        // New attempt supersedes whatever last one said.
        if !self.syncError.to_string().is_empty() || self.syncErrorIsAuth {
            self.syncError = QString::from(String::new());
            self.syncErrorIsAuth = false;
            self.syncStateChanged();
        }
        // Reader asked for fresh list: give it NOW, before network answers.
        // Read rows leave at pull; sync results land on already-honest list.
        // Every other list starts over too (epoch says so, bump makes them
        // look). Own cursor moved past bump, as `announce_local_change`, else
        // next poll resets just-reset list.
        ctx.signal().mark_refreshed();
        self.reload_fresh();
        self.announce_local_change(&ctx);
        // Set before sending: worker clears flag when command finishes; fast
        // failure can beat us. Setting after leaves spinner running forever.
        ctx.signal().set_running(true);
        if !ctx.send(Command::Sync) {
            // Worker gone: nothing will clear flag.
            ctx.signal().set_running(false);
        }
        // Record what QML was told, else spinner never stops.
        //
        // `pollSync` reports change by comparing flag with `seen_running`.
        // Raising flag and emitting WITHOUT recording leaves `seen_running`
        // false: worker lowering flag compares false to false, no transition,
        // no emit; `syncing` is READ + NOTIFY so QML keeps last value.
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

        // Spinner first, BEFORE generation guard. Failed refresh changes
        // running flag without necessarily changing mirror.
        let running = ctx.signal().is_running();
        if running != self.seen_running {
            self.seen_running = running;
            self.syncingChanged();
        }

        // Then anything worker left for user to read. Drained here in one
        // app-wide poll, not per-page Timer: several EntryListPages alive (feed
        // views); per-page timers race for one-notice slot.
        self.drain_notice(ctx.signal());

        let generation = ctx.signal().generation();
        if generation == self.seen_generation {
            return false;
        }
        self.seen_generation = generation;
        // `reload`'s own answer (whether it RESET list) deliberately not this
        // one. QML polls every model per tick, uses result to decide whether to
        // poll feed list too: turns on MIRROR moving, not how gently this list
        // absorbed it.
        self.reload();
        true
    }

    /// Take sync failure off signal, if waiting.
    ///
    /// Anything not owned put back for page that owns it (same discipline as
    /// `Settings::poll_notice_from`).
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
            .map_or(0, |n| i32::try_from(n).unwrap_or(i32::MAX))
    }

    fn title_matches(&self) -> i32 {
        i32::try_from(self.matches.title).unwrap_or(i32::MAX)
    }

    fn feed_matches(&self) -> i32 {
        i32::try_from(self.matches.feed).unwrap_or(i32::MAX)
    }

    fn text_matches(&self) -> i32 {
        i32::try_from(self.matches.text).unwrap_or(i32::MAX)
    }

    /// Results shown for group: what page allows, or all when fewer.
    fn shown_in(&self, kind: MatchKind) -> i32 {
        let shown = self.groups.get(kind).shown.min(self.matches.get(kind));
        i32::try_from(shown).unwrap_or(i32::MAX)
    }

    fn title_shown(&self) -> i32 {
        self.shown_in(MatchKind::Title)
    }

    fn feed_shown(&self) -> i32 {
        self.shown_in(MatchKind::Feed)
    }

    fn text_shown(&self) -> i32 {
        self.shown_in(MatchKind::Text)
    }

    /// See `titleNextLoad`.
    fn next_load_in(&self, kind: MatchKind) -> i32 {
        self.groups
            .get(kind)
            .next_load()
            .map_or(0, |step| i32::try_from(step).unwrap_or(i32::MAX))
    }

    fn title_next_load(&self) -> i32 {
        self.next_load_in(MatchKind::Title)
    }

    fn feed_next_load(&self) -> i32 {
        self.next_load_in(MatchKind::Feed)
    }

    fn text_next_load(&self) -> i32 {
        self.next_load_in(MatchKind::Text)
    }

    fn loadMoreIn(&mut self, kind: QString) {
        let Some(kind) = match_kind_from_qml(&kind.to_string()) else {
            return;
        };
        if self.scope != Some(Scope::Search) {
            return;
        }
        let Some(ctx) = self.context() else { return };
        self.groups.get_mut(kind).load_more();
        // From matches already found: more of a group ≠ new search; mirror not
        // rescanned.
        let rows = self.search_rows(&ctx);
        publish_rows_as(self, rows, Publish::Search);
        self.matchesChanged();
    }

    /// Rows search shows: articles [`SearchGroups::pick`] picks from matches,
    /// read and marked up, feeds it found between two article groups, in
    /// [`MatchKind`] order.
    fn search_rows(&self, ctx: &AppContext) -> Vec<EntryRow> {
        let picked = self.groups.pick(&self.search_found);
        let search = &self.search;
        let hits = ctx
            .read(|db| store::search_hits(db.conn(), search, &picked).unwrap_or_default())
            .unwrap_or_default();
        let query = SearchQuery::parse(search);
        let articles = |kind: MatchKind| {
            hits.iter()
                .filter(move |hit| hit.kind == kind)
                .map(|hit| EntryRow::found(hit, query.as_ref()))
        };
        let feeds = self
            .search_feeds
            .iter()
            .take(self.groups.get(MatchKind::Feed).shown)
            .map(|feed| EntryRow::feed_found(feed, query.as_ref()));
        articles(MatchKind::Title)
            .chain(feeds)
            .chain(articles(MatchKind::Text))
            .collect()
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
            // Unread/Starred/All have no single server-side scope: expand over
            // every entry in scope, NOT `self.rows` (only current page; would
            // mark ≤ PAGE_SIZE, leave rest unread while UI reports success).
            _ => {
                // Ids only. Full entries for whole scope would haul every
                // title, URL, BODY to read one integer each (54 MB single
                // allocation on reported corpus). Search has no filter; list
                // doesn't offer it "mark all as read".
                let Some(filter) = scope.to_filter() else {
                    return;
                };
                let ids: Vec<EntryId> = ctx
                    .read(|db| store::entry_ids_matching(db.conn(), filter).unwrap_or_default())
                    .unwrap_or_default();
                ctx.write(|db| worker::apply_local_status_bulk(db, &ids, EntryStatus::Read))
                    .transpose()
                    .ok()
                    .flatten()
                    .is_some()
            }
        };
        if done {
            // Fresh, not retained: "mark all as read" = reader done with
            // everything here; list full of dimmed rows would look like nothing
            // happened.
            self.reload_fresh();
            self.announce_local_change(&ctx);
            ctx.send(Command::FlushOutbox);
        }
    }

    fn is_ready(&self) -> bool {
        self.scope.is_some()
    }

    fn id_at(&self, row: i32) -> Option<EntryId> {
        usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .map(|r| EntryId(r.id))
    }

    fn setScope(&mut self, kind: i32, id: i64) {
        self.scope = Some(Scope::from_qml(kind, id));
        // New scope = new list; nothing from old belongs.
        self.reload_fresh();
    }

    fn refresh(&mut self) {
        self.reload();
    }

    fn setSearch(&mut self, text: QString) {
        let text = text.to_string();
        if text == self.search {
            return;
        }
        self.search = text;
        if self.scope == Some(Scope::Search) {
            // New query = new list, like new scope.
            self.reload_fresh();
        }
    }

    fn entryIdAt(&self, row: i32) -> i64 {
        usize::try_from(row)
            .ok()
            .and_then(|i| self.rows.get(i))
            .map_or(0, |r| r.id)
    }

    /// Re-read rows from mirror, KEEPING rows on screen.
    ///
    /// Row no longer matching list (read on Unread, unstarred on Favourites)
    /// stays, drawn in new state, until reader asks for fresh list; else it
    /// vanishes on next poll and rows below jump under thumb. Rows LEAVE only
    /// on manual refresh (`requestSync`), scope change, or "mark all as read"
    /// (via `reload_fresh`). Rows still ARRIVE here (sync landing while open
    /// shows new articles); kept row mirror lost (server deleted) goes.
    ///
    /// Full reset, not diff, WHEN ROWS THEMSELVES CHANGE. Imperceptible at
    /// phone list sizes; wrong `begin_insert_rows` range = crash inside Qt
    /// model machinery, bad trade for a millisecond. Same articles in same
    /// places → no reset; see `reload_keeping`.
    ///
    /// Returns true when model RESET (costs reader scroll position).
    pub fn reload(&mut self) -> bool {
        self.reload_keeping(true)
    }

    /// Re-read rows, show exactly what matches scope.
    ///
    /// Also resets window to one page. Path when list starts over (new scope,
    /// manual refresh, "mark all as read"): reader paged to 400 rows in
    /// previous scope mustn't open next at 400.
    fn reload_fresh(&mut self) -> bool {
        self.limit = PAGE_SIZE;
        self.reload_keeping(false)
    }

    /// Rows to ask mirror for.
    ///
    /// `Default` leaves `limit` zero, QML constructs these: floor applied here,
    /// field not trusted.
    fn window(&self) -> i64 {
        if self.limit < PAGE_SIZE {
            PAGE_SIZE
        } else {
            self.limit
        }
    }

    fn loadMore(&mut self) {
        if !self.hasMore {
            return;
        }
        self.limit = self.window().saturating_add(PAGE_SIZE);
        // `reload`, not `reload_fresh`: paging down isn't starting over; rows
        // on screen are what reader looks at.
        self.reload();
    }

    fn reload_keeping(&mut self, keep_shown: bool) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };
        let Some(scope) = self.scope else {
            return false;
        };

        // Manual refresh anywhere (pulley on another tab, cover action) starts
        // every list over. Epoch is how this model learns it happened
        // elsewhere.
        let epoch = ctx.signal().refresh_epoch();
        let keep_shown = keep_shown && epoch == self.seen_refresh_epoch;
        self.seen_refresh_epoch = epoch;

        let shown: Vec<i64> = if keep_shown {
            self.rows.iter().map(|r| r.id).collect()
        } else {
            Vec::new()
        };

        let window = self.window();
        let mut full_window = false;
        let before = (self.matches, self.groups);
        let rows: Vec<EntryRow> = if let Some(filter) = scope.to_filter() {
            let entries = ctx
                .read(|db| {
                    let mut entries =
                        store::list_entries(db.conn(), filter, window, 0).unwrap_or_default();
                    // Before kept rows appended: those are finished rows scope
                    // no longer matches; counting them makes part-full window
                    // look full.
                    full_window = i64::try_from(entries.len()).unwrap_or(i64::MAX) >= window;
                    let listed: HashSet<i64> = entries.iter().map(|e| e.id.get()).collect();
                    // One statement per kept row, not built `IN (...)`, as
                    // outbox (§9.4). Handful (rows finished since last
                    // refresh), not a page.
                    for id in shown {
                        if listed.contains(&id) {
                            continue;
                        }
                        if let Ok(Some(entry)) = store::entry_list_row(db.conn(), EntryId(id)) {
                            entries.push(entry);
                        }
                    }
                    entries
                })
                .unwrap_or_default();
            let mut rows: Vec<EntryRow> = entries.iter().map(EntryRow::from).collect();
            // Back into mirror order, newest first: kept row sits where it was,
            // not at bottom. Stable sort over already-sorted fresh rows: only
            // kept ones move.
            rows.sort_by_key(|r| (std::cmp::Reverse(r.published), std::cmp::Reverse(r.id)));
            rows
        } else {
            // Search keeps nothing: reading doesn't stop match, results in
            // search order (grouped by match) which date sort would undo. No
            // whole-list paging: `hasMore` false, each group has own "Load
            // more" (`loadMoreIn`).
            //
            // Searched again each reload (mirror may have changed); fresh one
            // restarts every group.
            if !keep_shown {
                self.groups = SearchGroups::default();
            }
            let search = &self.search;
            (self.search_found, self.search_feeds) = ctx
                .read(|db| {
                    (
                        store::search_matches(db.conn(), search).unwrap_or_default(),
                        store::search_feeds(db.conn(), search).unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            self.matches = store::SearchCounts::of(&self.search_found, &self.search_feeds);
            self.search_rows(&ctx)
        };
        if before != (self.matches, self.groups) {
            self.matchesChanged();
        }

        // Chrome not in row: row diff can't see renamed feed or new icon. Take
        // shared map first, note if different; unchanged rows still need redraw
        // then.
        let chrome = ctx.feed_chrome();
        let chrome_moved = !std::rc::Rc::ptr_eq(&self.feed_chrome, &chrome);
        self.feed_chrome = chrome;

        // Window mirror filled completely may have more. One row short proves
        // not: common case, no extra query.
        if full_window != self.hasMore {
            self.hasMore = full_window;
            self.hasMoreChanged();
        }

        // Reset only if rows differ from held ones, else reader loses place for
        // nothing. See `publish_rows` (rule for both models).
        let how = if scope == Scope::Search {
            Publish::Search
        } else {
            Publish::Reset
        };
        let reset = publish_rows_as(self, rows, how);
        if !reset && chrome_moved {
            self.redraw_all_rows();
        }
        reset
    }

    /// Feed name and icon row is drawn with: what [`EntryModel::data`] hands
    /// QML for `feedName` and `feedIcon`.
    ///
    /// Borrowed from shared map, not copied into row (see [`EntryRow`]).
    /// Unknown feed draws with neither: what just-unsubscribed feed's entry
    /// looks like between two reloads.
    fn chrome_for(&self, feed_id: i64) -> Option<&crate::context::FeedChrome> {
        self.feed_chrome.get(&feed_id)
    }

    /// Announce every row needs re-reading, moving none.
    ///
    /// No-op on empty model: `row_index` of absent row isn't index view can be
    /// told.
    fn redraw_all_rows(&mut self) {
        let Some(last) = self
            .rows
            .len()
            .checked_sub(1)
            .and_then(|n| i32::try_from(n).ok())
        else {
            return;
        };
        let first = QAbstractListModel::row_index(self, 0);
        let last = QAbstractListModel::row_index(self, last);
        QAbstractListModel::data_changed(self, first, last);
    }

    /// Update one row in place after local mutation, no full reload.
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
        let model_index = QAbstractListModel::row_index(self, i);
        QAbstractListModel::data_changed(self, model_index, model_index);
        // Read/unread change moves `unreadTotal` (cover shows it).
        // `countChanged` = its only NOTIFY: must fire here too, else cover
        // badge stale until next reload (glaring when opening article marks it
        // read).
        if unread.is_some() {
            self.countChanged();
        }
    }

    /// The rows as they stand, in display order.
    #[must_use]
    pub fn rows(&self) -> &[EntryRow] {
        &self.rows
    }
}

impl RowList for EntryModel {
    type Row = EntryRow;

    fn rows_held(&self) -> &[EntryRow] {
        &self.rows
    }

    fn rows_held_mut(&mut self) -> &mut Vec<EntryRow> {
        &mut self.rows
    }

    fn row_id(row: &EntryRow) -> i64 {
        row.id
    }

    fn emit_count_changed(&mut self) {
        self.countChanged();
    }
}

impl QAbstractListModel for EntryModel {
    fn row_count(&self) -> i32 {
        i32::try_from(self.rows.len()).unwrap_or(i32::MAX)
    }

    fn data(&self, index: QModelIndex, role: i32) -> QVariant {
        // Bounds-checked, not indexed: Qt calls with any index delegate asks;
        // `clippy::indexing_slicing` denied for exactly this shape.
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
            // Looked up, not held per row (see [`EntryRow`]), CLONED not
            // rebuilt: `QString`s in shared map, clone = refcount bump. Runs
            // per visible row per repaint.
            ROLE_FEED_NAME => self
                .chrome_for(row.feed_id)
                .map(|c| c.name.clone())
                .unwrap_or_default()
                .into(),
            ROLE_FEED_ICON => self
                .chrome_for(row.feed_id)
                .map(|c| c.icon_uri.clone())
                .unwrap_or_default()
                .into(),
            ROLE_MATCH_KIND => QString::from(row.match_kind).into(),
            ROLE_TITLE_STYLED => QString::from(row.title_styled.clone()).into(),
            ROLE_FEED_NAME_STYLED => QString::from(row.feed_styled.clone()).into(),
            ROLE_EXCERPT => QString::from(row.excerpt.clone()).into(),
            ROLE_UNREAD_COUNT => row.unread_count.into(),
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
        names.insert(ROLE_MATCH_KIND, "matchKind".into());
        names.insert(ROLE_TITLE_STYLED, "titleStyled".into());
        names.insert(ROLE_FEED_NAME_STYLED, "feedNameStyled".into());
        names.insert(ROLE_EXCERPT, "excerpt".into());
        names.insert(ROLE_UNREAD_COUNT, "unreadCount".into());
        names
    }
}

// ------------------------------------------------------------------- feeds

/// Model roles for [`FeedModel`], one per [`FeedRow`] field; reuses [`ROLE_ID`]
/// for feed id.
///
/// `title`: [`FeedRow::title`]. FOREIGN TEXT: `Text.PlainText`.
pub const ROLE_FEED_TITLE: i32 = USER_ROLE + 1;
/// `unreadCount`: [`FeedRow::unread`].
pub const ROLE_FEED_UNREAD: i32 = USER_ROLE + 2;
/// `errorMessage`: [`FeedRow::error`].
pub const ROLE_FEED_ERROR: i32 = USER_ROLE + 3;
/// `categoryId`: [`FeedRow::category_id`].
pub const ROLE_FEED_CATEGORY: i32 = USER_ROLE + 4;
/// "Fetch original content" for every entry of this feed, server-side.
pub const ROLE_FEED_CRAWLER: i32 = USER_ROLE + 5;
/// The server has stopped refreshing this feed.
pub const ROLE_FEED_DISABLED: i32 = USER_ROLE + 6;
/// Keep this feed out of the global unread list.
pub const ROLE_FEED_HIDDEN: i32 = USER_ROLE + 7;
/// `data:` URI for feed icon; empty when mirror has none.
///
/// Separate from [`ROLE_FEED_ICON`] though same content: role numbers per
/// model, both number from `USER_ROLE` independently.
pub const ROLE_FEED_ICON_URI: i32 = USER_ROLE + 8;

/// `PartialEq` for same reason as [`EntryRow`]: reload finding held list must
/// not reset model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedRow {
    /// The server's id for the feed.
    pub id: i64,
    /// FOREIGN TEXT: the feed's name.
    pub title: String,
    /// How many of its entries are unread in the mirror.
    pub unread: i32,
    /// Non-empty when server last failed to refresh feed. Foreign text: plain
    /// only.
    pub error: String,
    /// The feed's category, or 0 when it has none.
    pub category_id: i64,
    /// See [`ROLE_FEED_CRAWLER`].
    pub crawler: bool,
    /// See [`ROLE_FEED_DISABLED`].
    pub disabled: bool,
    /// See [`ROLE_FEED_HIDDEN`].
    pub hide_globally: bool,
    /// Feed icon as `data:` URI, or empty. Built from bytes already in mirror:
    /// drawing fetches nothing (§9.3).
    ///
    /// `QString` from shared chrome map: this row and every entry list drawing
    /// same feed share one buffer (see [`crate::context::FeedChrome`]).
    pub icon_uri: QString,
}

/// Feed list, and feed-level actions: subscribe, unsubscribe, edit, mark read.
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
    /// Subscribe. *Server* discovers and fetches feed; Vuo never downloads one
    /// (§3).
    subscribe: qt_method!(fn(&mut self, feed_url: QString)),
    unsubscribe: qt_method!(fn(&mut self, row: i32)),
    /// Save feed settings to server, then mirror.
    ///
    /// Flat argument list, not JS object: `QVariantMap` across this boundary in
    /// qmetaobject 0.2.10 loses bool type info. Passing `title` unchanged =
    /// "don't rename": shim diffs against mirror, sends only what moved;
    /// untouched field never transmitted (see `FeedPatch`).
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
    /// Pending result of last `updateFeed`, drained by edit page.
    ///
    /// See `EntryModel::syncError`: worker can't call into QML-owned object;
    /// one-shot slot plus poll carries answer back.
    pub updateError: qt_property!(QString; NOTIFY updateStateChanged),
    /// Whether the last `updateFeed` succeeded; read beside `updateError`.
    pub updateOk: qt_property!(bool; NOTIFY updateStateChanged),
    /// Bumped per finished save: page tells repeat answer from stale one.
    pub updateSerial: qt_property!(i32; NOTIFY updateStateChanged),
    updateStateChanged: qt_signal!(),

    rows: Vec<FeedRow>,
    seen_generation: u64,
    ctx: Option<std::rc::Rc<AppContext>>,
}

impl FeedModel {
    /// Give model context explicitly, load list. Tests only; QML-built model
    /// uses installed context.
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
        self.reload();
    }

    /// App context: explicitly attached (tests), else global installed at
    /// start-up. QML constructs these: no constructor to pass it through.
    fn context(&self) -> Option<std::rc::Rc<AppContext>> {
        self.ctx.clone().or_else(crate::context::current)
    }

    fn pollSync(&mut self) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };
        // BEFORE generation early-return. Rejected save changes nothing in
        // mirror, generation stays; drain after early-return could never report
        // failure (answer edit page most needs).
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
        // Empty title = rejection, not instruction: Miniflux would accept,
        // leaving nameless row user can't identify to fix.
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
            // Not ours. Put back for waiting page; dropping makes "Test
            // connection" look dead.
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
            .map_or(0, |r| r.id)
    }

    /// Re-read feed list. True when model RESET (costs reader place); mirrors
    /// [`EntryModel::reload`].
    pub fn reload(&mut self) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };
        // Same shared map entry lists draw from: can't disagree about feed
        // look, encode paid once. Taken outside `read`, which would hold mirror
        // borrowed while cache build borrows again.
        let chrome = ctx.feed_chrome();
        let rows: Vec<FeedRow> = ctx
            .read(|db| {
                let counts = store::unread_counts_by_feed(db.conn()).unwrap_or_default();
                store::feeds(db.conn())
                    .unwrap_or_default()
                    .iter()
                    .map(|f| FeedRow {
                        id: f.id.get(),
                        title: f.title.clone(),
                        unread: i32::try_from(counts.get(&f.id.get()).copied().unwrap_or(0))
                            .unwrap_or(i32::MAX),
                        error: f.parsing_error_message.clone(),
                        category_id: f.category_id.map_or(0, vuo_core::model::CategoryId::get),
                        crawler: f.crawler,
                        disabled: f.disabled,
                        hide_globally: f.hide_globally,
                        // Held per row, not looked up per delegate like entry
                        // list: tens of rows, one icon each. Clone = `QString`
                        // refcount bump onto shared buffer.
                        icon_uri: chrome
                            .get(&f.id.get())
                            .map(|c| c.icon_uri.clone())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Same rule as entry list, own reason: reloads on every bump entry
        // models report; unread count moves per finished article; without
        // `publish_rows` reading one article scrolls Feeds page to top.
        publish_rows(self, rows)
    }

    /// The rows as they stand, in display order.
    #[must_use]
    pub fn rows(&self) -> &[FeedRow] {
        &self.rows
    }
}

impl RowList for FeedModel {
    type Row = FeedRow;

    fn rows_held(&self) -> &[FeedRow] {
        &self.rows
    }

    fn rows_held_mut(&mut self) -> &mut Vec<FeedRow> {
        &mut self.rows
    }

    fn row_id(row: &FeedRow) -> i64 {
        row.id
    }

    fn emit_count_changed(&mut self) {
        self.countChanged();
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
            ROLE_FEED_ICON_URI => row.icon_uri.clone().into(),
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
        names.insert(ROLE_FEED_ICON_URI, "feedIcon".into());
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Notice, SyncSignal};

    /// §a failed refresh reaches the screen, and stops the spinner.
    ///
    /// Timed-out refresh must stop spinning AND say something. `syncing` is
    /// READ + NOTIFY; `syncingChanged` must fire above generation early-return,
    /// else failure clearing running flag without changing mirror never reaches
    /// binding.
    #[test]
    fn a_failure_that_changed_nothing_still_repaints_and_still_speaks() {
        let signal = SyncSignal::default();
        let mut model = EntryModel::default();

        // As Qt thread does on pull to refresh.
        signal.set_running(true);
        model.seen_running = true;

        // As worker guard does on failed refresh: mirror unchanged, generation
        // stays.
        let before = signal.generation();
        signal.set_running(false);
        signal.post(Notice::SyncFailed {
            auth: false,
            message: "could not reach the server".to_owned(),
        });
        assert_eq!(signal.generation(), before, "nothing changed the mirror");

        // Poll notices transition anyway.
        model.drain_notice(&signal);
        assert_eq!(model.syncError.to_string(), "could not reach the server");
        assert!(!model.syncErrorIsAuth);

        // Rejected key reported as such, no server text: page supplies own
        // translated line.
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
    /// `requestSync` raises running flag, tells QML, and records that it did;
    /// else later true→false compares false to false, never reported, spinner
    /// stops only if poll lands mid-sync.
    #[test]
    fn the_spinner_stops_even_when_no_poll_lands_mid_sync() {
        let signal = SyncSignal::default();
        let mut model = EntryModel::default();

        // What `requestSync` does, minus worker (no context).
        signal.set_running(true);
        model.seen_running = signal.is_running();

        // Worker finishes before any poll runs.
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

    /// Mirror with one feed, one icon, one entry, and context on it.
    fn seeded() -> (tempfile::TempDir, std::rc::Rc<AppContext>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("m.sqlite");
        let mut db = vuo_core::db::Database::open(&path).expect("mirror");

        // One-pixel PNG: mirror's format sniffing exercised, not bypassed.
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
                store::Arrival::Quiet,
            )
        })
        .expect("seed");

        let instance = url::Url::parse("https://miniflux.example/").expect("url");
        let ctx = crate::context::context_for_test(db, instance);
        (dir, ctx)
    }

    /// §an entry row carries the feed it came from.
    ///
    /// Row shows feed name and icon, not just age; else every row looks like it
    /// came from nowhere.
    #[test]
    fn an_entry_row_carries_its_feed_name_and_icon() {
        let (_dir, ctx) = seeded();
        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);

        assert_eq!(model.row_count(), 1, "the seeded entry is unread");

        // Via `chrome_for` (what `data` calls), NOT off row: chrome looked up
        // by feed id when delegate asks (see `EntryRow`).
        //
        // Not via `data` itself: needs `QModelIndex`, only obtainable from
        // model's C++ side, which a model never handed to QML lacks; every
        // index invalid, every role null.
        let row = model.rows().first().expect("one row");
        let chrome = model.chrome_for(row.feed_id).cloned().unwrap_or_default();
        assert_eq!(
            chrome.name.to_string(),
            "Tagesschau",
            "the row must name the feed it came from"
        );
        let icon = chrome.icon_uri.to_string();
        assert!(
            icon.starts_with("data:image/png;base64,"),
            "and carry its icon as a data URI, got {icon:?}"
        );

        // Names = half the contract: delegate reaches these by bare word; role
        // present in Rust but unnamed here is `undefined` in QML.
        let names = <EntryModel as QAbstractListModel>::role_names(&model);
        for (role, name) in [(ROLE_FEED_NAME, "feedName"), (ROLE_FEED_ICON, "feedIcon")] {
            assert_eq!(
                names.get(&role).map(ToString::to_string),
                Some(name.to_owned()),
                "QML reaches this role by name"
            );
        }
    }

    /// §one encode of the feed chrome, not one per list.
    ///
    /// Four `EntryModel`s and a `FeedModel` alive, all reload on same bump.
    /// Per-model re-read and re-encode of every icon wasteful for set changing
    /// only on new feed or icon. Cache on context: only thing QML-constructed
    /// models share.
    ///
    /// `Rc::ptr_eq` assertion: only check telling shared map from identical
    /// copy; identical copy is the bug.
    #[test]
    fn every_list_draws_from_one_feed_chrome_map() {
        let (_dir, ctx) = seeded();

        let mut unread = EntryModel::default();
        unread.attach(std::rc::Rc::clone(&ctx));
        unread.setScope(0, 0);
        let mut all = EntryModel::default();
        all.attach(std::rc::Rc::clone(&ctx));
        all.setScope(2, 0);
        let mut feeds = FeedModel::default();
        feeds.attach(std::rc::Rc::clone(&ctx));

        let held = ctx.feed_chrome();
        assert!(
            std::rc::Rc::ptr_eq(&held, &ctx.feed_chrome()),
            "asking twice at the same generation must not build it twice"
        );
        assert!(
            std::rc::Rc::ptr_eq(&held, &unread.feed_chrome)
                && std::rc::Rc::ptr_eq(&held, &all.feed_chrome),
            "every entry list must hold the one map, not a copy of it"
        );
        // SAME BUFFER, not equal string. `QString` implicitly shared: clone
        // from chrome map = refcount bump onto one buffer, affordable per feed
        // row and per visible entry row per repaint. Separate `String` copies
        // (up to 683 KB base64 each) would compare equal; equality can't tell.
        let from_feed_list = feeds
            .rows()
            .first()
            .map(|r| r.icon_uri.clone())
            .unwrap_or_default();
        let from_chrome = held.get(&1).map(|c| c.icon_uri.clone()).unwrap_or_default();
        assert!(
            !from_chrome.to_slice().is_empty(),
            "the fixture must have an icon for this to say anything"
        );
        assert_eq!(
            from_feed_list.to_slice().as_ptr(),
            from_chrome.to_slice().as_ptr(),
            "the feed list must share the entry lists' icon, not copy it"
        );

        // Bump alone must NOT move map. Mark-read bumps generation; list seeing
        // map move redraws every row (delegate detail line from font metrics):
        // cheapest reader action repaints whole list.
        ctx.signal().bump();
        assert!(
            std::rc::Rc::ptr_eq(&held, &ctx.feed_chrome()),
            "a bump that did not change a feed must hand back the same map"
        );

        // Actually changed feed does move it, else renamed feed keeps old name
        // on every list until restart.
        ctx.write(|db| {
            db.with_tx(|tx| {
                store::upsert_feed(
                    tx,
                    &Feed {
                        id: FeedId(1),
                        category_id: None,
                        title: "Tagesschau 24".to_owned(),
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
                    2,
                )
            })
        })
        .expect("borrow")
        .expect("rename");
        ctx.signal().bump();

        let after = ctx.feed_chrome();
        assert!(
            !std::rc::Rc::ptr_eq(&held, &after),
            "a renamed feed must move the map"
        );
        assert_eq!(
            after.get(&1).map(|c| c.name.to_string()),
            Some("Tagesschau 24".to_owned())
        );

        // List must be told: no ROW changed; row diff can't see rename; map
        // moving only signal.
        unread.reload();
        assert_eq!(
            unread.chrome_for(1).map(|c| c.name.to_string()),
            Some("Tagesschau 24".to_owned()),
            "the list must pick the new name up"
        );
    }

    /// §the feed list exposes the settings the editor writes.
    ///
    /// "Feed settings" menu item passes these three roles into pushed page;
    /// unresolved role makes whole push throw.
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
        // Feeds page draws same icon as entry list (same chrome map), not bare
        // names.
        let icon = row.icon_uri.to_string();
        assert!(
            icon.starts_with("data:image/png;base64,"),
            "a feed row must carry its icon as a data URI, got {icon:?}"
        );
        let names = <FeedModel as QAbstractListModel>::role_names(&model);
        for (role, name) in [
            (ROLE_FEED_CRAWLER, "crawler"),
            (ROLE_FEED_DISABLED, "feedDisabled"),
            (ROLE_FEED_HIDDEN, "hideGlobally"),
            (ROLE_FEED_ICON_URI, "feedIcon"),
        ] {
            assert_eq!(
                names.get(&role).map(ToString::to_string),
                Some(name.to_owned()),
                "QML reaches this role by name, and the editor's push needs it"
            );
        }
    }

    /// Unread entry in seeded feed. No publish time (like seeded one): mirror
    /// order by id. Shim has no chrono on purpose; ids enough to tell merge
    /// from append.
    fn unread_entry(id: i64) -> Entry {
        Entry {
            id: EntryId(id),
            feed_id: FeedId(1),
            status: EntryStatus::Unread,
            starred: false,
            title: format!("entry {id}"),
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
        }
    }

    fn put(ctx: &AppContext, entry: &Entry) {
        ctx.write(|db| db.with_tx(|tx| store::upsert_entry(tx, entry, 1, store::Arrival::Quiet)))
            .expect("the mirror")
            .expect("upsert");
    }

    fn ids(model: &EntryModel) -> Vec<i64> {
        model.rows().iter().map(|r| r.id).collect()
    }

    /// §a row the reader has finished with leaves the list when THEY say so.
    ///
    /// Article opened from Unread and marked read stays listed, drawn read,
    /// through any number of mirror changes; leaves on pulley Refresh. Else it
    /// vanishes on poll and rows below jump under thumb.
    #[test]
    fn a_row_read_elsewhere_stays_listed_until_the_reader_refreshes() {
        let (_dir, ctx) = seeded();
        put(&ctx, &unread_entry(5));

        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);
        assert_eq!(ids(&model), vec![7, 5], "newest first");

        // Article view marks 5 read: local write and bump, as
        // `ArticleModel::apply_local`.
        ctx.write(|db| worker::apply_local_status(db, EntryId(5), EntryStatus::Read))
            .expect("the mirror")
            .expect("mark read");
        ctx.signal().bump();
        assert!(model.pollSync(), "the poll must notice the bump");
        assert_eq!(
            ids(&model),
            vec![7, 5],
            "the row the reader has read must stay on the list"
        );
        assert!(
            !model.rows().get(1).expect("row 5").unread,
            "but it must show as read"
        );
        assert_eq!(model.unread_total(), 1, "the cover's count is the mirror's");

        // Sync lands older article while read row still there. It ARRIVES; read
        // row keeps ITS place between them (mirror order), not at bottom
        // (append).
        put(&ctx, &unread_entry(3));
        ctx.signal().bump();
        assert!(model.pollSync());
        assert_eq!(
            ids(&model),
            vec![7, 5, 3],
            "new rows still arrive, and a kept row sits where it did"
        );

        // Reader pulls Refresh (any tab or cover). What `requestSync` does,
        // minus worker.
        ctx.signal().mark_refreshed();
        ctx.signal().bump();
        assert!(model.pollSync());
        assert_eq!(
            ids(&model),
            vec![7, 3],
            "a manual refresh is the one thing that takes a read row off Unread"
        );
    }

    /// §the list must not move under the reader when nothing on it moved.
    ///
    /// Context-menu mark-read: `setRead` patches row in place, sends
    /// `FlushOutbox`; worker bumps generation when server confirms (for
    /// settings page pending count, nothing list shows). Poll re-reads same
    /// rows; must not reset (reset re-lays plain `ListView` at its top).
    #[test]
    fn a_bump_that_leaves_the_rows_alone_does_not_reset_the_list() {
        let (_dir, ctx) = seeded();
        put(&ctx, &unread_entry(5));

        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);
        assert_eq!(ids(&model), vec![7, 5], "newest first");

        // Reader marks 5 read from context menu: patched in place.
        model.setRead(1, true);
        assert!(
            !model.rows().get(1).expect("row 5").unread,
            "the tap itself shows immediately"
        );

        // Flush confirms intent, worker bumps generation. ENTRIES untouched
        // (only outbox rows deleted): poll's reload finds held list.
        ctx.signal().bump();
        assert!(
            !model.reload(),
            "a reload that finds the same articles in the same places must not \
             reset the model: a reset takes the reader back to the top"
        );
        assert_eq!(ids(&model), vec![7, 5], "and it keeps the rows either way");

        // Row read on OTHER tab redrawn in place too. Kept on Unread until
        // refresh: same list, one row in new state.
        ctx.write(|db| worker::apply_local_status(db, EntryId(7), EntryStatus::Read))
            .expect("the mirror")
            .expect("mark read");
        assert!(
            !model.reload(),
            "a row whose state changed is redrawn, not re-laid out"
        );
        assert!(
            !model.rows().first().expect("row 7").unread,
            "and it does show its new state"
        );

        // Row ARRIVING = different list, must reset.
        put(&ctx, &unread_entry(3));
        assert!(
            model.reload(),
            "an article arriving changes which rows there are"
        );
        assert_eq!(ids(&model), vec![7, 5, 3]);
    }

    /// Feeds page reloads on every bump entry models report; unread count moves
    /// per finished article: same reset would scroll it away.
    #[test]
    fn a_feed_whose_count_moved_is_redrawn_rather_than_re_laid_out() {
        let (_dir, ctx) = seeded();
        put(&ctx, &unread_entry(5));
        let mut feeds = FeedModel::default();
        feeds.attach(std::rc::Rc::clone(&ctx));
        let before = feeds.rows().first().expect("the seeded feed").unread;

        ctx.write(|db| worker::apply_local_status(db, EntryId(5), EntryStatus::Read))
            .expect("the mirror")
            .expect("mark read");
        assert!(
            !feeds.reload(),
            "the same feeds in the same places must not reset the list"
        );
        assert_eq!(
            feeds.rows().first().expect("the seeded feed").unread,
            before - 1,
            "but the count must be redrawn"
        );
    }

    /// New scope = new list.
    #[test]
    fn a_scope_change_shows_only_what_matches_it() {
        let (_dir, ctx) = seeded();
        put(&ctx, &unread_entry(5));
        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);

        // Read from list itself: patched in place, row stays.
        model.setRead(1, true);
        assert_eq!(ids(&model), vec![7, 5]);

        model.setScope(0, 0);
        assert_eq!(
            ids(&model),
            vec![7],
            "re-scoping must not carry the finished row over"
        );
    }

    /// Kept row mirror lost (server deleted) goes too.
    #[test]
    fn a_kept_row_the_mirror_no_longer_has_is_dropped() {
        let (_dir, ctx) = seeded();
        put(&ctx, &unread_entry(5));
        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);
        model.setRead(1, true);
        assert_eq!(ids(&model), vec![7, 5]);

        ctx.write(|db| db.with_tx(|tx| store::delete_entry(tx, EntryId(5))))
            .expect("the mirror")
            .expect("delete");
        ctx.signal().bump();
        assert!(model.pollSync());
        assert_eq!(
            ids(&model),
            vec![7],
            "a row that is not in the mirror cannot be kept on a list of it"
        );
    }

    /// Two tabs, one refresh: epoch reaches unpulled tab.
    #[test]
    fn a_manual_refresh_on_one_tab_starts_every_tab_over() {
        let (_dir, ctx) = seeded();
        put(&ctx, &unread_entry(5));
        let mut unread = EntryModel::default();
        unread.attach(std::rc::Rc::clone(&ctx));
        unread.setScope(0, 0);
        let mut all = EntryModel::default();
        all.attach(std::rc::Rc::clone(&ctx));
        all.setScope(2, 0);

        unread.setRead(1, true);
        assert_eq!(ids(&unread), vec![7, 5]);
        assert!(all.pollSync(), "the other tab hears about the change");
        assert_eq!(ids(&all), vec![7, 5], "and All lists everything anyway");

        // Refresh pulled on All: its epoch moves, bump carries it.
        ctx.signal().mark_refreshed();
        ctx.signal().bump();
        assert!(unread.pollSync());
        assert_eq!(
            ids(&unread),
            vec![7],
            "a refresh is asked of the app, not of a tab"
        );
    }

    /// §the list is a window, and the window grows when the reader asks.
    ///
    /// Smaller `PAGE_SIZE` point: unseen rows not built, sorted, compared, held
    /// four times over. Acceptable only if rows past window REACHABLE: walks
    /// reader's scroll-to-bottom path. Full window says more; `loadMore`
    /// widens; unfillable window says nothing left.
    #[test]
    fn a_full_window_can_be_extended_until_the_mirror_runs_out() {
        let (_dir, ctx) = seeded();

        // One page and a bit: first window full, second not. Entry 7 seeded by
        // `seeded`.
        let extra = PAGE_SIZE + 10;
        ctx.write(|db| {
            db.with_tx(|tx| {
                for n in 0..extra {
                    store::upsert_entry(
                        tx,
                        &Entry {
                            id: EntryId(1000 + n),
                            feed_id: FeedId(1),
                            status: EntryStatus::Unread,
                            starred: false,
                            title: format!("Entry {n}"),
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
                        },
                        1,
                        store::Arrival::Quiet,
                    )?;
                }
                Ok(())
            })
        })
        .transpose()
        .expect("seed a page and a bit")
        .expect("seed a page and a bit");

        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);

        assert_eq!(
            i64::from(model.row_count()),
            PAGE_SIZE,
            "the list must open on ONE page, not on the whole mirror"
        );
        assert!(
            model.hasMore,
            "a window the mirror filled completely may have more behind it, \
             and a list that says otherwise strands every row past the first \
             page"
        );

        model.loadMore();
        assert_eq!(
            i64::from(model.row_count()),
            extra + 1,
            "the second page must bring the rest in"
        );
        assert!(
            !model.hasMore,
            "a window the mirror could not fill is proof there is nothing \
             behind it; saying otherwise leaves the list asking forever"
        );

        // No-op once nothing left: list bouncing at bottom doesn't re-query per
        // bounce.
        let before = model.row_count();
        model.loadMore();
        assert_eq!(model.row_count(), before);
    }

    /// §starting the list over starts the window over with it.
    ///
    /// Reader deep in Unread opening a feed mustn't find it at 400 rows: window
    /// memory never given back.
    #[test]
    fn a_new_scope_goes_back_to_one_page() {
        let (_dir, ctx) = seeded();
        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(0, 0);

        model.limit = PAGE_SIZE * 4;
        model.setScope(2, 0);

        assert_eq!(
            model.limit, PAGE_SIZE,
            "a new scope is a new list, and it opens at one page"
        );
    }

    /// §search: the list a search opens into, and what it follows.
    ///
    /// Search page re-scopes model on push and sets text as reader types,
    /// either order; list shows what text matches whichever came first. Empty
    /// field lists nothing, not whole mirror.
    #[test]
    fn a_search_model_lists_what_its_text_matches() {
        let (_dir, ctx) = seeded();
        let mut harbour = unread_entry(5);
        harbour.title = "Harbour at dusk".to_owned();
        put(&ctx, &harbour);
        let mut walk = unread_entry(6);
        walk.content = "<p>A walk by the <b>harbour</b></p>".to_owned();
        put(&ctx, &walk);

        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(5, 0);
        assert!(model.is_ready(), "a search with no text is still a list");
        assert_eq!(ids(&model), Vec::<i64>::new(), "no text, no results");

        model.setSearch("HARBOUR".into());
        assert_eq!(
            ids(&model),
            vec![5, 6],
            "title hits before body hits, whatever their dates"
        );
        assert_eq!(
            (
                model.title_matches(),
                model.feed_matches(),
                model.text_matches()
            ),
            (1, 0, 1)
        );
        let found = model.rows();
        assert_eq!(found[0].match_kind, "title");
        assert_eq!(found[0].title_styled, "<b>Harbour</b> at dusk");
        assert_eq!(
            found[0].excerpt, "",
            "a title hit shows its match in the title"
        );
        assert_eq!(found[1].match_kind, "text");
        assert_eq!(found[1].title_styled, "entry 6");
        assert_eq!(
            found[1].excerpt, "A walk by the <b>harbour</b>",
            "a body hit shows the text around the match, the match in bold"
        );
        assert_eq!(found[1].feed_styled, "Tagesschau");

        model.setSearch("harbour walk".into());
        assert_eq!(ids(&model), vec![6], "every term must match");
        assert_eq!(
            (
                model.title_matches(),
                model.feed_matches(),
                model.text_matches()
            ),
            (0, 0, 1),
            "and the counts follow the search"
        );

        // Feed's name finds FEED (seeded one is Tagesschau), not its articles:
        // one row, opens feed.
        model.setSearch("tagesschau".into());
        assert_eq!(ids(&model), vec![-1], "the feed, not its three articles");
        let feed = &model.rows()[0];
        assert_eq!(feed.match_kind, "feed");
        assert_eq!(feed.feed_id, 1, "what the row opens");
        assert_eq!(feed.title, "Tagesschau");
        assert_eq!(feed.title_styled, "<b>Tagesschau</b>");
        assert_eq!(feed.unread_count, 3, "as the feed list counts it");
        assert!(feed.unread, "drawn at full strength, as the feed list does");
        assert_eq!(
            (
                model.title_matches(),
                model.feed_matches(),
                model.text_matches()
            ),
            (0, 1, 0)
        );
        model.setSearch("tagesschau island".into());
        assert_eq!(
            ids(&model),
            Vec::<i64>::new(),
            "a feed's name and an article's title do not add up to a match"
        );

        // Group order: titles, feeds, body text.
        model.setSearch("a".into());
        assert_eq!(
            model
                .rows()
                .iter()
                .map(|r| (r.match_kind, r.id < 0))
                .collect::<Vec<_>>(),
            vec![
                ("title", false),
                ("title", false),
                ("feed", true),
                ("text", false)
            ]
        );

        // Text set before scope applied when scope arrives; re-scoping keeps
        // it: page re-applies scope each activation, query must survive.
        let mut early = EntryModel::default();
        early.attach(std::rc::Rc::clone(&ctx));
        early.setSearch("dusk".into());
        early.setScope(5, 0);
        assert_eq!(ids(&early), vec![5]);
        early.setScope(5, 0);
        assert_eq!(ids(&early), vec![5]);

        // Model NOT searching ignores text altogether.
        let mut unread = EntryModel::default();
        unread.attach(std::rc::Rc::clone(&ctx));
        unread.setSearch("dusk".into());
        unread.setScope(0, 0);
        assert_eq!(ids(&unread), vec![7, 6, 5]);
        assert!(
            unread.rows().iter().all(|r| r.match_kind.is_empty()
                && r.title_styled.is_empty()
                && r.excerpt.is_empty()),
            "and draws no search marks"
        );
        assert_eq!(unread.title_matches(), 0);
    }

    /// §a search shows five of each group, and more of one group when asked:
    /// ten more, then twenty more, then the rest. Only shown rows read.
    #[test]
    fn a_search_shows_a_page_of_each_group_and_more_on_request() {
        let (_dir, ctx) = seeded();
        // 45 title hits, newest first by id, plus 3 body hits.
        for id in 100..145 {
            let mut entry = unread_entry(id);
            entry.title = format!("Harbour {id}");
            put(&ctx, &entry);
        }
        for id in 200..203 {
            let mut entry = unread_entry(id);
            entry.content = "<p>by the harbour</p>".to_owned();
            put(&ctx, &entry);
        }

        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(5, 0);
        model.setSearch("harbour".into());
        let shown = |m: &EntryModel| (m.title_shown(), m.feed_shown(), m.text_shown());

        assert_eq!(
            model.title_matches(),
            45,
            "the header counts the whole group"
        );
        assert_eq!(shown(&model), (5, 0, 3), "and the list shows five of it");
        assert_eq!(
            ids(&model),
            (140..145).rev().chain([202, 201, 200]).collect::<Vec<_>>(),
            "the newest five titles, then the body hits"
        );
        assert!(!model.hasMore, "a search does not page as a whole");
        assert_eq!(model.title_next_load(), 10, "the button offers ten more");

        model.loadMoreIn("title".into());
        assert_eq!(shown(&model), (15, 0, 3));
        assert_eq!(model.rows()[14].id, 130, "the next ten, under the first");
        assert_eq!(model.rows()[15].id, 202, "and the next group after them");
        assert_eq!(model.title_next_load(), 20, "then twenty more");
        assert_eq!(model.text_next_load(), 10, "in that group only");

        model.loadMoreIn("title".into());
        assert_eq!(shown(&model), (35, 0, 3));
        assert_eq!(model.title_next_load(), 0, "then the rest");

        // Poll finding mirror changed keeps what reader opened.
        model.reload();
        assert_eq!(shown(&model), (35, 0, 3));

        model.loadMoreIn("title".into());
        assert_eq!(shown(&model), (45, 0, 3));
        assert_eq!(model.row_count(), 48);

        // Nothing for non-group, nor outside search.
        model.loadMoreIn("author".into());
        assert_eq!(model.row_count(), 48);

        // New results never reset list: reset hands focus to first row, off
        // search field and keyboard.
        model.search = "harbour 12".to_owned();
        assert!(
            !model.reload_fresh(),
            "a new search is rows out and rows in"
        );
        assert_eq!(model.title_matches(), 11, "112 and 120 to 129");
        assert_eq!(model.title_shown(), 5, "a new search starts over");
        assert!(!model.reload(), "and so is a re-read");

        // New search restarts every group. ("entry 201": 1 in title, harbour in
        // body → title hit now.)
        model.loadMoreIn("title".into());
        model.setSearch("harbour 1".into());
        assert_eq!(model.title_matches(), 46);
        assert_eq!(shown(&model), (5, 0, 0));
        assert_eq!(model.title_next_load(), 10);

        let mut unread = EntryModel::default();
        unread.attach(std::rc::Rc::clone(&ctx));
        unread.setScope(0, 0);
        let before = unread.row_count();
        unread.loadMoreIn("title".into());
        assert_eq!(unread.row_count(), before);
    }

    /// §the feeds a search finds are a group like the others: a page of them,
    /// and more on request, in the feed list's order.
    #[test]
    fn a_search_pages_through_the_feeds_it_found() {
        let (_dir, ctx) = seeded();
        for id in 300..312 {
            let feed = Feed {
                id: FeedId(id),
                category_id: None,
                title: format!("Harbour {id}"),
                site_url: None,
                feed_url: None,
                icon_id: None,
                checked_at: None,
                parsing_error_message: String::new(),
                parsing_error_count: 0,
                disabled: false,
                hide_globally: id == 300,
                crawler: false,
            };
            ctx.write(|db| db.with_tx(|tx| store::upsert_feed(tx, &feed, 1)))
                .expect("the mirror")
                .expect("upsert");
        }
        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(5, 0);
        model.setSearch("harbour".into());

        assert_eq!(model.feed_matches(), 12, "hidden ones too");
        assert_eq!(model.feed_shown(), 5);
        assert_eq!(ids(&model), vec![-300, -301, -302, -303, -304]);
        assert_eq!(model.feed_next_load(), 10);

        model.loadMoreIn("feed".into());
        assert_eq!(model.feed_shown(), 12, "no more than there are");
        assert_eq!(ids(&model), (300..312).map(|id| -id).collect::<Vec<_>>());
        assert_eq!(model.feed_next_load(), 20);

        // New search restarts group.
        model.setSearch("harbour 30".into());
        assert_eq!(model.feed_matches(), 10);
        assert_eq!(model.feed_shown(), 5);
    }

    /// Run of rows added among held rows told apart from every other change, at
    /// right place: range goes to Qt as insertion; wrong one crashes inside
    /// model machinery.
    #[test]
    fn an_added_run_of_rows_is_found_and_nothing_else_is() {
        assert_eq!(inserted_run(&[1, 2, 9], &[1, 2, 3, 4, 9]), Some((2, 2)));
        assert_eq!(
            inserted_run(&[1, 2], &[1, 2, 3]),
            Some((2, 1)),
            "at the end"
        );
        assert_eq!(
            inserted_run(&[2, 3], &[1, 2, 3]),
            Some((0, 1)),
            "at the top"
        );
        assert_eq!(inserted_run(&[], &[1, 2]), Some((0, 2)), "into nothing");
        assert_eq!(inserted_run(&[1, 2], &[1, 2]), None, "no change");
        assert_eq!(inserted_run(&[1, 2, 3], &[1, 3]), None, "a row left");
        assert_eq!(inserted_run(&[1, 2], &[2, 1, 3]), None, "rows moved");
        assert_eq!(inserted_run(&[1, 3], &[0, 1, 2, 3]), None, "two runs");
        assert_eq!(inserted_run(&[1, 1], &[1, 1, 1]), Some((2, 1)));
    }

    /// "Mark all as read" has nothing to act on in search: list doesn't offer
    /// it; called anyway, must not fall through to scope marking whole mirror.
    #[test]
    fn mark_all_read_in_a_search_marks_nothing() {
        let (_dir, ctx) = seeded();
        put(&ctx, &unread_entry(5));
        let mut model = EntryModel::default();
        model.attach(std::rc::Rc::clone(&ctx));
        model.setScope(5, 0);
        model.setSearch("entry".into());

        model.markAllReadIn(5, 0);

        let unread = ctx
            .read(|db| store::unread_count(db.conn()).expect("count"))
            .expect("the mirror");
        assert_eq!(unread, 2, "nothing was marked");
    }
}
