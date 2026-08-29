//! What the QML-facing models share.
//!
//! Every model needs the same two things: the mirror, to read from and to write
//! local mutations into, and a way to ask the worker for the few operations
//! that genuinely require the network.
//!
//! `Rc`, not `Arc`: this is handed only to `QObject`s, which all live on the Qt
//! thread. Using an atomic refcount would imply a sharing that must not happen
//! and would quietly make a thread-affinity bug compile.

use std::cell::RefCell;
use std::rc::Rc;

use vuo_core::db::Database;

use crate::worker::Command;

pub struct AppContext {
    db: Rc<RefCell<Database>>,
    commands: std::sync::mpsc::Sender<Command>,
    /// The configured Miniflux origin, for the content transform's media policy.
    instance: url::Url,
    /// The user's Images setting (`settings::MEDIA_*`).
    ///
    /// Lives here because the two objects that care are both constructed by
    /// QML and cannot reach each other: `Settings` writes it on save, and
    /// `ArticleModel` reads it when it builds a transform context. Before this
    /// existed, `ArticleModel` hardcoded `UnproxiedMedia::Ask` and
    /// `Settings::media_policy_for` had no production caller at all -- so the
    /// Images control was wired to nothing and a user who chose Strict still
    /// got Ask (§9.3).
    media_policy: std::cell::Cell<i32>,
    /// Bumped by the worker when the mirror changes; polled by the models.
    signal: std::sync::Arc<SyncSignal>,
    /// Owned here so the worker thread lives exactly as long as the context
    /// that talks to it. Dropping the context sends Shutdown and joins the
    /// thread; the alternative — leaking the handle with `mem::forget` — would
    /// mean the thread is never told to stop and never joined.
    _worker: crate::worker::Worker,
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppContext { .. }")
    }
}

impl AppContext {
    #[must_use]
    pub fn new(
        db: Database,
        worker: crate::worker::Worker,
        instance: url::Url,
        signal: std::sync::Arc<SyncSignal>,
    ) -> Rc<Self> {
        let commands = worker.sender();
        Rc::new(AppContext {
            db: Rc::new(RefCell::new(db)),
            commands,
            instance,
            media_policy: std::cell::Cell::new(crate::settings::MEDIA_ASK),
            signal,
            _worker: worker,
        })
    }

    #[must_use]
    pub fn signal(&self) -> &SyncSignal {
        &self.signal
    }

    /// The shared handle, for code that needs to hold on to it.
    #[must_use]
    pub fn signal_handle(&self) -> &std::sync::Arc<SyncSignal> {
        &self.signal
    }

    #[must_use]
    pub fn instance(&self) -> &url::Url {
        &self.instance
    }

    /// The user's Images setting (`settings::MEDIA_*`).
    #[must_use]
    pub fn media_policy(&self) -> i32 {
        self.media_policy.get()
    }

    /// Record the Images setting. Called on start-up and whenever it is saved.
    pub fn set_media_policy(&self, policy: i32) {
        self.media_policy.set(policy);
    }

    /// Borrow the mirror for a read.
    ///
    /// Returns `None` rather than panicking if a borrow is already outstanding.
    /// A `RefCell` double-borrow is a panic, and §9.5 makes a panic here
    /// undefined behaviour once it unwinds into Qt's C++ frames — so every
    /// borrow in this crate is fallible and every caller degrades instead.
    pub fn read<T>(&self, f: impl FnOnce(&Database) -> T) -> Option<T> {
        self.db.try_borrow().ok().map(|db| f(&db))
    }

    /// Borrow the mirror for a write. Same fallibility rule as [`read`].
    pub fn write<T>(&self, f: impl FnOnce(&mut Database) -> T) -> Option<T> {
        self.db.try_borrow_mut().ok().map(|mut db| f(&mut db))
    }

    /// Ask the worker to do something. `false` if the worker has stopped.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }
}

thread_local! {
    /// The single application context, installed once at start-up.
    ///
    /// A thread-local rather than a `static`: everything that reads this is a
    /// `QObject` living on the Qt thread, and a thread-local makes that
    /// requirement structural instead of a comment. A background thread that
    /// tried to reach the mirror this way would find nothing rather than
    /// racing.
    ///
    /// This exists because QML instantiates the models — `EntryModel {}` in a
    /// .qml file — so Rust has no handle to hand them a context through. The
    /// alternative would be a singleton QObject that every model reaches
    /// through QML, which is more machinery for the same one global.
    static CURRENT: RefCell<Option<Rc<AppContext>>> = const { RefCell::new(None) };
}

/// Install the application context. Call once, from the Qt thread, before QML
/// loads.
pub fn install(ctx: Rc<AppContext>) {
    CURRENT.with(|c| {
        if let Ok(mut slot) = c.try_borrow_mut() {
            *slot = Some(ctx);
        }
    });
}

/// The installed context, if there is one.
///
/// Returns `None` before start-up finishes and in tests, so every caller has
/// to handle its absence — which is why a QML-constructed model that is never
/// attached degrades to an empty list rather than crashing.
#[must_use]
pub fn current() -> Option<Rc<AppContext>> {
    CURRENT.with(|c| c.try_borrow().ok().and_then(|slot| slot.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_no_context_before_startup() {
        // Models constructed by QML before install() must degrade, not panic.
        // (Each test thread has its own slot, so this is not order-dependent.)
        assert!(current().is_none());
    }

    /// A context over a temp mirror and a worker pointed at an origin nothing
    /// answers on. Nothing here makes a request; the worker exists because
    /// `AppContext` owns it.
    fn test_context(dir: &tempfile::TempDir) -> Rc<AppContext> {
        let db_path = dir.path().join("mirror.sqlite");
        let db = Database::open(&db_path).expect("mirror");
        let signal = std::sync::Arc::new(SyncSignal::default());
        let worker = crate::worker::Worker::spawn(
            db_path,
            url::Url::parse("https://unreachable.invalid/").expect("url"),
            vuo_core::redact::ApiToken::new("t"),
            vuo_core::api::TransportConfig::default(),
            std::sync::Arc::clone(&signal),
            |_event| {},
        );
        AppContext::new(
            db,
            worker,
            url::Url::parse("https://unreachable.invalid/").expect("url"),
            signal,
        )
    }

    #[test]
    fn a_reentrant_borrow_returns_none_rather_than_panicking() {
        // §9.5: a panic here unwinds into Qt's C++ frames, which is undefined
        // behaviour -- so every borrow of the mirror is fallible.
        //
        // Nothing exercised that. No test built an `AppContext` at all, so
        // `read` and `write` were never called, let alone reentrantly:
        // replacing `try_borrow` with `borrow` left the whole shim suite green
        // while turning a recoverable `None` into UB.
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = test_context(&dir);

        // A plain borrow succeeds.
        assert!(ctx.read(|_| ()).is_some());
        assert!(ctx.write(|_| ()).is_some());

        // A write inside a read, and a read inside a write, are the two ways
        // this happens for real: a model reading the mirror to build a row and
        // calling something that writes.
        let nested_write = ctx.read(|_| ctx.write(|_| ()));
        assert_eq!(
            nested_write,
            Some(None),
            "a write inside a read must be refused, not panic"
        );
        let nested_read = ctx.write(|_| ctx.read(|_| ()));
        assert_eq!(
            nested_read,
            Some(None),
            "a read inside a write must be refused, not panic"
        );

        // And the refusal is temporary: the borrow is released afterwards.
        assert!(ctx.write(|_| ()).is_some());
    }

    #[test]
    fn the_sync_signal_counts_generations() {
        // The models poll this to decide whether to reload. If `bump` stops
        // incrementing, the UI never refreshes after a sync and silently shows
        // stale articles; if the poll stops comparing, every model reloads from
        // SQLite twice a second forever. Neither direction was tested.
        let signal = SyncSignal::default();
        assert_eq!(signal.generation(), 0);
        signal.bump();
        assert_eq!(signal.generation(), 1);
        signal.bump();
        assert_eq!(signal.generation(), 2, "two bumps must advance by two");

        // And it is observable from another thread, which is the only way it
        // is ever used: the worker bumps, the Qt thread polls.
        let shared = std::sync::Arc::new(SyncSignal::default());
        let writer = std::sync::Arc::clone(&shared);
        std::thread::spawn(move || {
            for _ in 0..100 {
                writer.bump();
            }
        })
        .join()
        .expect("worker thread");
        assert_eq!(shared.generation(), 100);

        assert!(!shared.is_running());
        shared.set_running(true);
        assert!(shared.is_running());
        shared.set_running(false);
        assert!(!shared.is_running());
    }
}

/// A counter the worker bumps whenever it changes the mirror.
///
/// Deliberately not a callback registry. QML owns the models, so Rust has no
/// list of live ones to call into, and building one out of `QPointer`s means
/// cross-thread lifetime rules that cannot be exercised without a device. An
/// atomic the UI polls has neither problem: the worker thread only ever
/// increments an integer, and every `QObject` touch stays on the Qt thread
/// where the poll runs.
#[derive(Debug, Default)]
pub struct SyncSignal {
    generation: std::sync::atomic::AtomicU64,
    running: std::sync::atomic::AtomicBool,
    /// A one-shot result the UI has to SHOW rather than merely reload for.
    ///
    /// The generation counter says "the mirror changed"; it cannot carry the
    /// server's answer to "test this connection" or the error text from a
    /// rejected feed URL. Those reached a log line and nothing else, so "Test
    /// connection" appeared to do nothing whether the credentials were right
    /// or wrong.
    notice: std::sync::Mutex<Option<Notice>>,
}

/// Something the worker produced that a page must display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `GET /v1/me` answered. `message` is the username, or the error.
    ConnectionTested { ok: bool, message: String },
    /// A subscribe or unsubscribe finished. `message` is the server's error
    /// text when it failed -- foreign text, so it renders as plain text.
    SubscriptionChanged { ok: bool, message: String },
}

impl SyncSignal {
    /// Called from the worker thread when the mirror changed.
    pub fn bump(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Leave a result for the UI to pick up. Called from the worker thread.
    ///
    /// A poisoned lock is ignored rather than unwrapped: §9.5 forbids a panic
    /// that could unwind into Qt's frames, and a dropped notice costs the user
    /// a status line, not data.
    pub fn post(&self, notice: Notice) {
        if let Ok(mut slot) = self.notice.lock() {
            *slot = Some(notice);
        }
    }

    /// Take the pending notice, if any. Called from the Qt thread.
    #[must_use]
    pub fn take_notice(&self) -> Option<Notice> {
        self.notice.lock().ok().and_then(|mut slot| slot.take())
    }

    pub fn set_running(&self, running: bool) {
        self.running
            .store(running, std::sync::atomic::Ordering::Release);
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Acquire)
    }
}
