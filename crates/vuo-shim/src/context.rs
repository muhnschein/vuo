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

use crate::worker::{Account, AppPaths, Command, Event, Worker};

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
    /// The user's "mark read when opened" setting (`settings::MARK_READ_*`).
    ///
    /// Here for the same reason `media_policy` is: `Settings` and
    /// `ArticleModel` are both constructed by QML and cannot reach each other.
    mark_read_delay_index: std::cell::Cell<i32>,
    /// Bumped by the worker when the mirror changes; polled by the models.
    signal: std::sync::Arc<SyncSignal>,
    /// Which stored account this context was built for; see [`fingerprint`].
    ///
    /// The worker captures the server URL and the API key when it spawns, so
    /// a context outlives the credentials it was built from. This is how a
    /// save decides whether the running worker is still the right one.
    fingerprint: u64,
    /// Owned here so the worker thread lives exactly as long as the context
    /// that talks to it. Dropping the context sends Shutdown and joins the
    /// thread; the alternative — leaking the handle with `mem::forget` — would
    /// mean the thread is never told to stop and never joined.
    ///
    /// `Option`, because a context that is being *replaced* rather than shut
    /// down retires its worker instead of joining it. See
    /// [`AppContext::retire`].
    worker: RefCell<Option<Worker>>,
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
        worker: Worker,
        instance: url::Url,
        signal: std::sync::Arc<SyncSignal>,
        fingerprint: u64,
    ) -> Rc<Self> {
        let commands = worker.sender();
        Rc::new(AppContext {
            db: Rc::new(RefCell::new(db)),
            commands,
            instance,
            media_policy: std::cell::Cell::new(crate::settings::MEDIA_ASK),
            mark_read_delay_index: std::cell::Cell::new(crate::settings::MARK_READ_DEFAULT_INDEX),
            signal,
            fingerprint,
            worker: RefCell::new(Some(worker)),
        })
    }

    /// Which stored account this context was built for.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Tell the worker to stop, without waiting for it.
    ///
    /// `Drop` joins, which is what process shutdown wants. Replacing a live
    /// context is the other case, and there a join is a hazard: it runs on the
    /// Qt thread, and a worker in the middle of a request holds its thread
    /// until the network times out — so the join would freeze the UI for
    /// exactly that long. Retiring drops the join handle instead. The thread
    /// still receives `Shutdown` and still winds down; it is simply not waited
    /// for, and the mirror tolerates the overlap (WAL, a 5-second busy
    /// timeout, and a second *process* already writes it on the sync timer).
    pub fn retire(&self) {
        if let Ok(mut slot) = self.worker.try_borrow_mut() {
            if let Some(mut worker) = slot.take() {
                worker.retire();
            }
        }
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

    /// The user's "mark read when opened" setting.
    #[must_use]
    pub fn mark_read_delay_index(&self) -> i32 {
        self.mark_read_delay_index.get()
    }

    /// Record it. Called on start-up and whenever Settings is saved.
    pub fn set_mark_read_delay_index(&self, index: i32) {
        self.mark_read_delay_index.set(index);
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
    /// The single application context.
    ///
    /// Installed at start-up when there is already an account to build it
    /// from, and again by the settings screen when one is saved or changed.
    /// It was *only* the first of those for a while, which meant a first run
    /// left this empty for the whole session — see [`refresh`].
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

/// Install the application context, replacing any already installed.
///
/// From the Qt thread only. Prefer [`refresh`], which decides whether a
/// replacement is warranted and retires the outgoing worker; this is the raw
/// store behind it.
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

/// Identify an account without keeping a second copy of its credentials.
///
/// A save that changed nothing must not restart the worker; a save that changed
/// the server or the key must, because the worker captures both when it spawns.
/// Hashing rather than storing the strings keeps the token out of one more live
/// object: this is change detection, not authentication, so a non-cryptographic
/// hash is the right tool for it.
fn fingerprint(account: &Account) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    account.server_url.hash(&mut hasher);
    account.token.hash(&mut hasher);
    account.use_custom_ca.hash(&mut hasher);
    hasher.finish()
}

/// Where the worker's events go.
///
/// Logging only, and deliberately *not* wrapped in `queued_callback`. It
/// touches no `QObject`, and `tracing` is built to be called from any thread,
/// so marshalling it back to the Qt thread would buy nothing and cost
/// something real: it would tie every rebuild of the context to a live Qt
/// event loop, which start-up has and the settings screen cannot promise. The
/// results the UI has to *show* travel by [`SyncSignal`] instead; see that
/// type for why it is a poll.
fn log_event(event: Event) {
    match &event {
        Event::SyncFinished { unread, .. } => tracing::info!(unread, "sync finished"),
        Event::AuthFailed => tracing::warn!("the server rejected the API key"),
        Event::SyncFailed { message } => tracing::warn!(%message, "sync failed"),
        other => tracing::debug!(?other, "sync event"),
    }
}

/// What the settings screen shows when the stored address will not parse.
///
/// Deliberately does not echo the address back: it is the user's own, this text
/// reaches the log as well as the screen, and a URL may carry userinfo. Naming
/// the missing part is more use anyway — `10.77.0.1:8083` is the natural thing
/// to type into that field, and it is not a URL.
const NOT_A_URL: &str =
    "the server address is not a URL. It needs a scheme, as in https://miniflux.example.com/";

/// Build a context for the account stored at `paths`.
///
/// Nothing is installed here; [`refresh`] does that. Split out so the whole
/// path — account file to running worker — can be exercised without a Qt event
/// loop. It used to live in the application binary, which has no tests at all
/// and is not even built by most of `make check`, and that is precisely why the
/// defect below survived to a device.
pub fn build(
    paths: &AppPaths,
    on_event: impl Fn(Event) + Send + 'static,
) -> vuo_core::Result<Rc<AppContext>> {
    let account = crate::worker::load_account(&paths.account)?;
    build_from(paths, account, on_event)
}

fn build_from(
    paths: &AppPaths,
    account: Account,
    on_event: impl Fn(Event) + Send + 'static,
) -> vuo_core::Result<Rc<AppContext>> {
    let server = url::Url::parse(&account.server_url)
        .map_err(|_| vuo_core::Error::Config(NOT_A_URL.to_owned()))?;
    let config = crate::worker::transport_config_for(paths, &account)?;
    let db = Database::open(&paths.database)?;
    let fingerprint = fingerprint(&account);

    let signal = std::sync::Arc::new(SyncSignal::default());
    let worker = Worker::spawn(
        paths.database.clone(),
        server.clone(),
        vuo_core::redact::ApiToken::new(account.token),
        config,
        std::sync::Arc::clone(&signal),
        on_event,
    );

    let ctx = AppContext::new(db, worker, server, signal, fingerprint);
    // Seed the Images setting from the stored account, so the first article
    // opened after this honours it rather than falling back to Ask.
    ctx.set_media_policy(account.media_policy);
    ctx.set_mark_read_delay_index(account.mark_read_delay_index);
    Ok(ctx)
}

/// Make the installed context match the account stored at `paths`.
///
/// Installs one when there is none. That is the case this exists for: the
/// context used to be built exactly once, at start-up, from an account file
/// that **does not exist until the user has saved one**. So on a first run the
/// build failed, nothing ever retried it, and every worker-backed action —
/// "Test connection", the pulley menu's Refresh — reached no worker at all and
/// did nothing, with no error and no spinner, until Vuo was restarted.
///
/// Rebuilds when the stored server or key changed, because the worker captures
/// both when it spawns. Without that, testing a connection after editing the
/// server address would report a confident answer about the *previous* account.
///
/// Idempotent and cheap when nothing changed: one file read and a hash.
pub fn refresh(paths: &AppPaths) -> vuo_core::Result<Rc<AppContext>> {
    let account = crate::worker::load_account(&paths.account)?;
    let wanted = fingerprint(&account);

    if let Some(existing) = current() {
        if existing.fingerprint() == wanted {
            return Ok(existing);
        }
        // Told to stop, but not waited for: a join here runs on the Qt thread.
        existing.retire();
    }

    let ctx = build_from(paths, account, log_event)?;
    install(Rc::clone(&ctx));
    Ok(ctx)
}

/// [`refresh`] against the standard locations.
///
/// `None` when there is no data directory or no usable account yet — both of
/// which are ordinary states before the user has been to Settings, not faults.
#[must_use]
pub fn refresh_current() -> Option<Rc<AppContext>> {
    let paths = AppPaths::resolve()?;
    match refresh(&paths) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            tracing::info!(error = %e, "no usable account yet");
            None
        }
    }
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
            0,
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

    fn account_at(dir: &tempfile::TempDir, server: &str) -> AppPaths {
        let paths = AppPaths::under(dir.path().join("harbour-vuo"));
        crate::worker::save_account(
            &paths.account,
            &Account {
                server_url: server.to_owned(),
                token: "k".to_owned(),
                ..Account::default()
            },
        )
        .expect("write the account");
        paths
    }

    #[test]
    fn refresh_installs_a_context_for_a_stored_account() {
        // The first-run path: `main` builds the context once, before any
        // account exists, so this is the call that has to work afterwards.
        let dir = tempfile::tempdir().expect("tempdir");
        // Plain http, as a WireGuard-only deployment uses. The transport
        // accepts it; nothing in this path may quietly require TLS.
        let paths = account_at(&dir, "http://10.77.0.1:8083/");
        assert!(current().is_none());

        let ctx = refresh(&paths).expect("a stored account must yield a context");
        assert_eq!(ctx.instance().as_str(), "http://10.77.0.1:8083/");
        assert!(
            ctx.send(Command::TestConnection),
            "the worker must be alive"
        );

        let installed = current().expect("and it must be installed, not just returned");
        assert!(Rc::ptr_eq(&ctx, &installed));

        // Idempotent: calling it again for the same account returns the same
        // context rather than restarting a worker on every save.
        let again = refresh(&paths).expect("a context");
        assert!(Rc::ptr_eq(&ctx, &again));
    }

    #[test]
    fn refresh_reports_an_address_that_is_not_a_url_rather_than_swallowing_it() {
        // `10.77.0.1:8083` is the natural thing to type and is not a URL. It
        // saves fine, so the failure lands here -- and used to land in a log
        // line at a level the default filter drops, leaving the app with no
        // worker and the user with no explanation.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = account_at(&dir, "10.77.0.1:8083");

        let e = refresh(&paths).expect_err("that is not a URL");
        assert!(
            e.to_string().contains("scheme"),
            "the message has to name what is missing: {e}"
        );
        assert!(current().is_none(), "and nothing half-built is installed");
    }

    #[test]
    fn a_retired_worker_is_not_waited_for() {
        // `retire` is what makes replacing a context safe on the Qt thread: a
        // join there blocks the UI until an in-flight request times out. It
        // must still stop the worker.
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = refresh(&account_at(&dir, "http://10.77.0.1:8083/")).expect("a context");

        ctx.retire();
        // The channel's receiver is dropped when the worker thread ends, so
        // this settles rather than hanging either way; what matters is that
        // `retire` itself returned without waiting for the thread.
        ctx.retire();
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
    /// A refresh failed. `message` is foreign text; render it as plain text.
    ///
    /// `auth` distinguishes "the server rejected the key" from everything
    /// else, because the two ask different things of the user -- go and fix
    /// the key, versus try again later. A bool rather than a second variant:
    /// a variant would duplicate the take-and-re-post plumbing on every page
    /// that drains this slot, for one bit.
    ///
    /// On `auth` the message is deliberately EMPTY. The server's own text for
    /// a rejected key says nothing a user can act on, and the page supplies a
    /// fixed translated line instead.
    SyncFailed { auth: bool, message: String },
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
