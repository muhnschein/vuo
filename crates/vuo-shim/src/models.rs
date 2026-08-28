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
//! `QObject`; it writes to SQLite and signals, and the signal handler — already
//! marshalled by `queued_callback` — calls `reload`.
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
use vuo_core::db::{store, Database};
use vuo_core::model::{Entry, EntryId};

pub const ROLE_ID: i32 = USER_ROLE;
pub const ROLE_TITLE: i32 = USER_ROLE + 1;
pub const ROLE_AUTHOR: i32 = USER_ROLE + 2;
pub const ROLE_UNREAD: i32 = USER_ROLE + 3;
pub const ROLE_STARRED: i32 = USER_ROLE + 4;
pub const ROLE_FEED_ID: i32 = USER_ROLE + 5;
pub const ROLE_PUBLISHED: i32 = USER_ROLE + 6;
pub const ROLE_READING_TIME: i32 = USER_ROLE + 7;
pub const ROLE_URL: i32 = USER_ROLE + 8;

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
            url: e.url.as_ref().map(|u| u.as_str().to_owned()).unwrap_or_default(),
        }
    }
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

    count: qt_property!(i32; READ row_count NOTIFY count_changed),
    count_changed: qt_signal!(),

    /// Set the scope and reload. `kind`: 0 unread, 1 starred, 2 all,
    /// 3 feed (id), 4 category (id).
    set_scope: qt_method!(fn(&mut self, kind: i32, id: i64)),
    /// Re-read the mirror. Called after the worker reports a change.
    refresh: qt_method!(fn(&mut self)),
    entry_id_at: qt_method!(fn(&self, row: i32) -> i64),

    rows: Vec<EntryRow>,
    scope: Option<Scope>,
    /// The mirror. `None` until [`EntryModel::attach`] is called from Rust —
    /// QML never constructs this.
    db: Option<std::rc::Rc<std::cell::RefCell<Database>>>,
}

impl EntryModel {
    /// Give the model its database. Called during app start-up, not from QML.
    pub fn attach(&mut self, db: std::rc::Rc<std::cell::RefCell<Database>>) {
        self.db = Some(db);
    }

    fn set_scope(&mut self, kind: i32, id: i64) {
        self.scope = Some(Scope::from_qml(kind, id));
        self.reload();
    }

    fn refresh(&mut self) {
        self.reload();
    }

    fn entry_id_at(&self, row: i32) -> i64 {
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
        let Some(db) = self.db.clone() else { return };
        let Some(scope) = self.scope else { return };

        let entries = {
            let borrowed = db.borrow();
            store::list_entries(borrowed.conn(), scope.to_filter(), 500, 0).unwrap_or_default()
        };
        let rows: Vec<EntryRow> = entries.iter().map(EntryRow::from).collect();

        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows = rows;
        (self as &mut dyn QAbstractListModel).end_reset_model();
        self.count_changed();
    }

    /// Update one row in place after a local mutation, without a full reload.
    pub fn mark_row(&mut self, id: EntryId, unread: Option<bool>, starred: Option<bool>) {
        let Some(index) = self.rows.iter().position(|r| r.id == id.get()) else { return };
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
        (self as &mut dyn QAbstractListModel).data_changed(model_index.clone(), model_index);
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
        let Ok(i) = usize::try_from(index.row()) else { return QVariant::default() };
        let Some(row) = self.rows.get(i) else { return QVariant::default() };

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
        names
    }
}

// ------------------------------------------------------------------- feeds

pub const ROLE_FEED_TITLE: i32 = USER_ROLE + 1;
pub const ROLE_FEED_UNREAD: i32 = USER_ROLE + 2;
pub const ROLE_FEED_ERROR: i32 = USER_ROLE + 3;
pub const ROLE_FEED_CATEGORY: i32 = USER_ROLE + 4;

#[derive(Debug, Clone, Default)]
pub struct FeedRow {
    pub id: i64,
    pub title: String,
    pub unread: i32,
    /// Non-empty when the server last failed to refresh this feed. Foreign
    /// text: plain text only.
    pub error: String,
    pub category_id: i64,
}

#[derive(QObject, Default)]
pub struct FeedModel {
    base: qt_base_class!(trait QAbstractListModel),
    count: qt_property!(i32; READ row_count NOTIFY count_changed),
    count_changed: qt_signal!(),
    refresh: qt_method!(fn(&mut self)),
    feed_id_at: qt_method!(fn(&self, row: i32) -> i64),

    rows: Vec<FeedRow>,
    db: Option<std::rc::Rc<std::cell::RefCell<Database>>>,
}

impl FeedModel {
    pub fn attach(&mut self, db: std::rc::Rc<std::cell::RefCell<Database>>) {
        self.db = Some(db);
    }

    fn refresh(&mut self) {
        self.reload();
    }

    fn feed_id_at(&self, row: i32) -> i64 {
        usize::try_from(row).ok().and_then(|i| self.rows.get(i)).map(|r| r.id).unwrap_or(0)
    }

    pub fn reload(&mut self) {
        let Some(db) = self.db.clone() else { return };
        let rows: Vec<FeedRow> = {
            let borrowed = db.borrow();
            store::feeds(borrowed.conn())
                .unwrap_or_default()
                .iter()
                .map(|f| FeedRow {
                    id: f.id.get(),
                    title: f.title.clone(),
                    unread: 0,
                    error: f.parsing_error_message.clone(),
                    category_id: f.category_id.map(|c| c.get()).unwrap_or(0),
                })
                .collect()
        };

        (self as &mut dyn QAbstractListModel).begin_reset_model();
        self.rows = rows;
        (self as &mut dyn QAbstractListModel).end_reset_model();
        self.count_changed();
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
        let Ok(i) = usize::try_from(index.row()) else { return QVariant::default() };
        let Some(row) = self.rows.get(i) else { return QVariant::default() };
        match role {
            ROLE_ID => row.id.into(),
            ROLE_FEED_TITLE => QString::from(row.title.clone()).into(),
            ROLE_FEED_UNREAD => row.unread.into(),
            ROLE_FEED_ERROR => QString::from(row.error.clone()).into(),
            ROLE_FEED_CATEGORY => row.category_id.into(),
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
        names
    }
}
