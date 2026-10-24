//! What QML-facing models share.
//!
//! Every model needs: mirror (read, write local mutations), and way to ask
//! worker for few network-only operations.
//!
//! `Rc`, not `Arc`: handed only to `QObject`s, all on Qt thread. Atomic
//! refcount implies sharing that mustn't happen; makes thread-affinity bug
//! compile quietly.

use std::cell::RefCell;
use std::rc::Rc;

use vuo_core::db::Database;

use crate::worker::{Account, AppPaths, Command, Event, Worker};

/// Mirror and worker command channel, shared by every QML-facing object; see
/// module docs.
pub struct AppContext {
    db: Rc<RefCell<Database>>,
    commands: std::sync::mpsc::Sender<Command>,
    /// The configured Miniflux origin, for the content transform's media policy.
    instance: url::Url,
    /// User's Images setting (`settings::MEDIA_*`).
    ///
    /// Here because both interested objects are QML-constructed and can't reach
    /// each other: `Settings` writes on save, `ArticleModel` reads when
    /// building transform context (§9.3), mapping it to the core's type
    /// itself.
    media_policy: std::cell::Cell<i32>,
    /// User's "mark read when opened" setting (`settings::MARK_READ_*`).
    ///
    /// Here for same reason as `media_policy`: `Settings` and `ArticleModel`
    /// both QML-constructed, can't reach each other.
    mark_read_delay_index: std::cell::Cell<i32>,
    /// Bumped by the worker when the mirror changes; polled by the models.
    signal: std::sync::Arc<SyncSignal>,
    /// Which stored account context was built for; see [`fingerprint`].
    ///
    /// Worker captures server URL and API key at spawn; context outlives
    /// credentials. How save decides whether running worker still right.
    fingerprint: u64,
    /// Feed names and icons, shared by every list drawing them.
    ///
    /// Here, not per model, same reason as `media_policy`. Four `EntryModel`s
    /// alive, all reload on same generation bump; per-model cache re-reads
    /// every icon blob and re-encodes base64 (~56 encodes per model per bump)
    /// for set changing only when sync brings new feed or icon. One cache, one
    /// encode.
    chrome: RefCell<Option<CachedChrome>>,
}

/// Feed chrome cache, and generation built at.
struct CachedChrome {
    generation: u64,
    feeds: Rc<std::collections::HashMap<i64, FeedChrome>>,
}

/// Feed's name and icon as entry list row needs.
///
/// `PartialEq`: rebuild finding same feeds hands back held map (see
/// [`AppContext::feed_chrome`]).
///
/// `QString`, not `String`: whole point of shape. `QString` implicitly shared,
/// clone = refcount bump; `data()` runs per visible row per repaint. As
/// `String` each call re-converts icon (up to 683 KB base64 per call per row;
/// icon ≤ 512 KiB raw). As `QString` every row, list, delegate points at ONE
/// buffer per feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedChrome {
    /// FOREIGN TEXT: feed's own name, or user's.
    pub name: qmetaobject::QString,
    /// Icon as `data:` URI; empty when mirror has none.
    pub icon_uri: qmetaobject::QString,
}

/// The one worker thread, and signal it publishes through.
///
/// Both process-wide, outlive any account. Signal especially: models compare
/// last seen generation to read one; signal replaced with account could hand
/// model lower generation than already spent.
struct SharedWorker {
    worker: Worker,
    signal: std::sync::Arc<SyncSignal>,
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppContext { .. }")
    }
}

impl AppContext {
    #[must_use]
    /// Context for one account, talking to the one worker thread.
    ///
    /// Worker NOT owned here: single thread created at start-up by
    /// [`start_worker`], handed each account in turn. Owning one would mean
    /// retiring it on account change (detaching thread mid-request) and
    /// starting another: device won't tolerate that once UI up. See
    /// [`Worker::spawn`].
    fn new(db: Database, instance: url::Url, worker: &SharedWorker, fingerprint: u64) -> Rc<Self> {
        Rc::new(AppContext {
            db: Rc::new(RefCell::new(db)),
            commands: worker.worker.sender(),
            instance,
            media_policy: std::cell::Cell::new(crate::settings::MEDIA_ASK),
            mark_read_delay_index: std::cell::Cell::new(crate::settings::MARK_READ_DEFAULT_INDEX),
            signal: std::sync::Arc::clone(&worker.signal),
            fingerprint,
            chrome: RefCell::new(None),
        })
    }

    /// Which stored account this context was built for.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// The worker's change counter and result slots.
    #[must_use]
    pub fn signal(&self) -> &SyncSignal {
        &self.signal
    }

    /// The shared handle, for code that needs to hold on to it.
    #[must_use]
    pub fn signal_handle(&self) -> &std::sync::Arc<SyncSignal> {
        &self.signal
    }

    /// The configured Miniflux origin.
    #[must_use]
    pub fn instance(&self) -> &url::Url {
        &self.instance
    }

    /// The user's Images setting (`settings::MEDIA_*`).
    #[must_use]
    pub fn media_policy(&self) -> i32 {
        self.media_policy.get()
    }

    /// Record Images setting. On start-up and each save.
    pub fn set_media_policy(&self, policy: i32) {
        self.media_policy.set(policy);
    }

    /// The user's "mark read when opened" setting.
    #[must_use]
    pub fn mark_read_delay_index(&self) -> i32 {
        self.mark_read_delay_index.get()
    }

    /// Record it. On start-up and each Settings save.
    pub fn set_mark_read_delay_index(&self, index: i32) {
        self.mark_read_delay_index.set(index);
    }

    /// Borrow mirror for read.
    ///
    /// `None`, not panic, if borrow outstanding. `RefCell` double-borrow
    /// panics; §9.5: panic unwinding into Qt C++ = UB. Every borrow here
    /// fallible; callers degrade.
    pub fn read<T>(&self, f: impl FnOnce(&Database) -> T) -> Option<T> {
        self.db.try_borrow().ok().map(|db| f(&db))
    }

    /// Borrow mirror for write. Same fallibility rule as [`read`](Self::read).
    pub fn write<T>(&self, f: impl FnOnce(&mut Database) -> T) -> Option<T> {
        self.db.try_borrow_mut().ok().map(|mut db| f(&mut db))
    }

    /// Ask worker to do something. `false` if worker stopped.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Feed names and icons, built once per generation, shared.
    ///
    /// Keyed on SYNC GENERATION, bumped by any mirror change (incl. mark-read,
    /// which can't change name or icon): rebuilds slightly too eagerly.
    /// Deliberate: generation = counter every model agrees on; narrower key
    /// needs own invalidation path through worker, untestable without device.
    /// Cost was per model per bump; now once per bump.
    ///
    /// `Rc` matters as much as cache: callers hold map, don't copy strings;
    /// `Rc::ptr_eq` tells list whether its chrome is still mirror's.
    ///
    /// Hence rebuild finding SAME feeds returns held map, not equal copy. List
    /// redraws every row when map moves (renamed feed changes no row), delegate
    /// rebuilds detail line from font metrics per redraw. Mark-read bumps
    /// generation: without this, cheapest reader action repaints whole list.
    pub fn feed_chrome(&self) -> Rc<std::collections::HashMap<i64, FeedChrome>> {
        let generation = self.signal.generation();
        if let Ok(held) = self.chrome.try_borrow() {
            if let Some(cached) = held.as_ref() {
                if cached.generation == generation {
                    return Rc::clone(&cached.feeds);
                }
            }
        }

        let built: std::collections::HashMap<i64, FeedChrome> = self
            .read(|db| vuo_core::db::store::feed_chrome(db.conn()).unwrap_or_default())
            .unwrap_or_default()
            .into_iter()
            .map(|feed| {
                let icon_uri = feed
                    .icon
                    .map(|(mime, bytes)| data_uri(&mime, &bytes))
                    .unwrap_or_default();
                (
                    feed.feed_id,
                    FeedChrome {
                        name: qmetaobject::QString::from(feed.title),
                        icon_uri,
                    },
                )
            })
            .collect();
        // Failed borrow costs rebuild next time, nothing else: same rule as
        // `read` and `write`.
        let Ok(mut slot) = self.chrome.try_borrow_mut() else {
            return Rc::new(built);
        };
        let feeds = match slot.take() {
            // Same feeds as last time: keep map every list holds; none sees it
            // move.
            Some(held) if *held.feeds == built => held.feeds,
            _ => Rc::new(built),
        };
        *slot = Some(CachedChrome {
            generation,
            feeds: Rc::clone(&feeds),
        });
        feeds
    }
}

/// Wrap image bytes as `data:` URI for QML `Image.source`.
///
/// MIME from mirror = format DETERMINED FROM BYTES, not server's claim: server
/// labelling script `image/png` can't get label back out.
fn data_uri(mime: &str, bytes: &[u8]) -> qmetaobject::QString {
    use base64::Engine as _;
    // SVG excluded: document, not bitmap; Qt renderer follows external
    // references, leaking device IP to feed operator's host on list scroll.
    // Raster formats pass even where device may lack handler (ships only
    // libqjpeg.so plugin; ICO, GIF gamble); delegate hides failed Image: wrong
    // guess = missing favicon, not broken glyph.
    if mime == "image/svg+xml" {
        return qmetaobject::QString::default();
    }
    // Converted to `QString` once here, refcount-shared after. See
    // [`FeedChrome`].
    qmetaobject::QString::from(format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

thread_local! {
    /// Single application context.
    ///
    /// Installed at start-up when an account exists, and again by settings
    /// screen on save or change (first run: see [`refresh`]).
    ///
    /// Thread-local, not `static`: every reader is `QObject` on Qt thread;
    /// thread-local makes that structural. Background thread reaching for
    /// mirror finds nothing, no race.
    ///
    /// Exists because QML instantiates models (`EntryModel {}` in .qml): Rust
    /// has no handle to pass context through. Alternative singleton QObject
    /// reached via QML = more machinery, same one global.
    static CURRENT: RefCell<Option<Rc<AppContext>>> = const { RefCell::new(None) };
}

thread_local! {
    /// The one worker thread. See [`start_worker`].
    static WORKER: RefCell<Option<Rc<SharedWorker>>> = const { RefCell::new(None) };
}

/// Start worker thread. Called from `main`, before UI.
///
/// Timing is the point, hence separate call. Creating thread from Qt thread
/// after Wayland and GPU stack dlopened kills process on Jolla Phone 2026 (no
/// panic, no Rust diagnostic, just signal); first run reaches that from QML tap
/// (no account until user types one). Created here: costs nothing waiting;
/// account arrives later as channel send. See [`Worker::spawn`] and
/// docs/testing.md.
pub fn start_worker() {
    let _ = shared_worker();
}

/// Worker, starting it on first ask.
///
/// Lazy path for tests (no `main` calling [`start_worker`]). On device always
/// present.
fn shared_worker() -> Option<Rc<SharedWorker>> {
    let existing = WORKER.with(|w| w.try_borrow().ok().and_then(|slot| slot.clone()));
    if existing.is_some() {
        return existing;
    }

    let signal = std::sync::Arc::new(SyncSignal::default());
    let worker = Worker::spawn(std::sync::Arc::clone(&signal), |event| log_event(&event));
    let shared = Rc::new(SharedWorker { worker, signal });
    WORKER.with(|w| {
        if let Ok(mut slot) = w.try_borrow_mut() {
            *slot = Some(Rc::clone(&shared));
        }
    });
    Some(shared)
}

/// Install application context, replacing any installed.
///
/// Qt thread only. Prefer [`refresh`] (decides whether replacement warranted);
/// this is raw store behind it.
pub fn install(ctx: Rc<AppContext>) {
    CURRENT.with(|c| {
        if let Ok(mut slot) = c.try_borrow_mut() {
            *slot = Some(ctx);
        }
    });
}

/// Installed context, if any.
///
/// `None` before start-up finishes and in tests: every caller handles absence;
/// unattached QML-constructed model degrades to empty list, no crash.
#[must_use]
pub fn current() -> Option<Rc<AppContext>> {
    CURRENT.with(|c| c.try_borrow().ok().and_then(|slot| slot.clone()))
}

/// Identify account without second copy of credentials.
///
/// Save changing nothing must not restart worker; changed server or key must
/// (worker captures both at spawn). Hashing keeps token out of one more live
/// object: change detection, not authentication; non-cryptographic hash right
/// tool.
fn fingerprint(account: &Account) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    account.server_url.hash(&mut hasher);
    account.token.hash(&mut hasher);
    account.use_custom_ca.hash(&mut hasher);
    hasher.finish()
}

/// Where worker's events go.
///
/// Logging only, NOT wrapped in `queued_callback`: touches no `QObject`;
/// `tracing` callable from any thread. Marshalling to Qt thread buys nothing,
/// ties every context rebuild to live Qt event loop (start-up has one, settings
/// screen can't promise). Results UI must *show* travel via [`SyncSignal`]; see
/// why poll there.
fn log_event(event: &Event) {
    match event {
        Event::SyncFinished { unread, .. } => tracing::info!(unread, "sync finished"),
        Event::AuthFailed => tracing::warn!("the server rejected the API key"),
        // `error =`, not bare `%message`: literal below is `tracing`'s own
        // `message` field; second field of that name collides, subscriber shows
        // one, dropping the half saying what went wrong.
        Event::SyncFailed { message } => tracing::warn!(error = %message, "sync failed"),
        other => tracing::debug!(?other, "sync event"),
    }
}

/// Settings screen text when stored address won't parse.
///
/// Doesn't echo address: user's own, text reaches log too, URL may carry
/// userinfo. Naming missing part more useful: `10.77.0.1:8083` natural to type,
/// not URL.
const NOT_A_URL: &str =
    "the server address is not a URL. It needs a scheme, as in https://miniflux.example.com/";

/// Build context for `account` without installing; [`refresh`] installs.
///
/// Here, not in untested app binary (mostly unbuilt by `make check`), so
/// first-run path is tested.
fn build_from(paths: &AppPaths, account: Account) -> vuo_core::Result<Rc<AppContext>> {
    let server = url::Url::parse(&account.server_url).map_err(|e| {
        // Why parse failed, for log; address stays out.
        tracing::warn!(error = %e, "the stored server address does not parse");
        vuo_core::Error::Config(NOT_A_URL.to_owned())
    })?;
    let config = crate::worker::transport_config_for(paths, &account)?;
    // Steps logged one by one on purpose. Only Vuo part running BOTH at
    // start-up and inside QML tap; latter has ended device process with signal,
    // not error: no Rust diagnostic, only last line reached. See
    // docs/testing.md.
    tracing::info!("opening the mirror");
    let db = Database::open(&paths.database)?;
    let fingerprint = fingerprint(&account);
    let sync_interval = crate::settings::sync_interval_minutes_for(account.sync_interval_index);
    let retention = crate::settings::retention_seconds_for(account.retention_index);

    let worker = shared_worker().ok_or_else(|| {
        vuo_core::Error::Config("the sync worker thread is not running".to_owned())
    })?;
    tracing::info!("handing the account to the sync worker");
    // Worker stopping between lookup above and here takes account nowhere;
    // every later sync silently does nothing.
    let configured =
        worker
            .worker
            .send(Command::Configure(Box::new(crate::worker::WorkerAccount {
                database: paths.database.clone(),
                server: server.clone(),
                token: vuo_core::redact::ApiToken::new(account.token),
                transport: config,
            })));
    if !configured {
        return Err(vuo_core::Error::Config(
            "the sync worker thread stopped before it took the account".to_owned(),
        ));
    }

    let ctx = AppContext::new(db, server, &worker, fingerprint);
    // Seed Images setting from stored account: first article opened honours it,
    // not Ask fallback.
    ctx.set_media_policy(account.media_policy);
    ctx.set_mark_read_delay_index(account.mark_read_delay_index);
    // And worker's cadence: syncs on own from here, at account's interval,
    // scheduled from last known sync.
    ctx.send(Command::SetSyncInterval {
        minutes: sync_interval,
    });
    // And retention window, applied at end of each pass.
    ctx.send(Command::SetRetention { seconds: retention });
    // And whether it may use metered connection on own.
    ctx.send(Command::SetWifiOnly {
        enabled: account.wifi_only,
    });
    tracing::info!("the application context is built");
    Ok(ctx)
}

/// Make installed context match account stored at `paths`.
///
/// Installs one when none: the case this exists for. Account file **doesn't
/// exist until user saves one**; building only once at start-up leaves first
/// run with no worker: "Test connection" and pulley Refresh silently do nothing
/// until restart.
///
/// Rebuilds when stored server or key changed (worker captures both at spawn);
/// else testing connection after editing address reports on *previous* account.
///
/// Idempotent, cheap when unchanged: one file read and hash.
///
/// # Errors
///
/// - [`vuo_core::Error::Config`] if account file can't be read or parsed
///   (ordinary before first save), stored server address not URL, configured
///   custom CA unreadable, or worker thread not running / stopped before taking
///   account.
/// - [`vuo_core::Error::Db`] or [`vuo_core::Error::Migration`] if mirror can't
///   open, as for [`Database::open`].
pub fn refresh(paths: &AppPaths) -> vuo_core::Result<Rc<AppContext>> {
    let account = crate::worker::load_account(&paths.account)?;
    let wanted = fingerprint(&account);

    if let Some(existing) = current() {
        if existing.fingerprint() == wanted {
            return Ok(existing);
        }
        tracing::info!("the stored account changed");
    }

    let ctx = build_from(paths, account)?;
    install(Rc::clone(&ctx));
    tracing::info!("the application context is installed");
    Ok(ctx)
}

/// Context over `db` and `instance`, against shared worker.
///
/// For crate tests: one place knows how worker starts; none reaches network or
/// cares which account.
#[cfg(test)]
pub(crate) fn context_for_test(db: Database, instance: url::Url) -> Rc<AppContext> {
    let worker = shared_worker().expect("the worker thread starts");
    AppContext::new(db, instance, &worker, 0)
}

/// [`refresh`] against standard locations.
///
/// `None` when no data directory or no usable account yet: ordinary before
/// Settings visited, not faults.
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

/// Counter worker bumps whenever it changes mirror.
///
/// Not callback registry. QML owns models: Rust has no live list; `QPointer`
/// list means cross-thread lifetime rules untestable without device. Polled
/// atomic avoids both: worker only increments integer, every `QObject` touch
/// stays on Qt thread where poll runs.
#[derive(Debug, Default)]
pub struct SyncSignal {
    generation: std::sync::atomic::AtomicU64,
    running: std::sync::atomic::AtomicBool,
    /// Bumped when USER asks for fresh list (pulley Refresh, cover action), vs
    /// mirror merely changing.
    ///
    /// Entry lists built on this: row no longer matching list (read, on Unread)
    /// stays through mirror changes, leaves only when this moves. One counter
    /// for all models, not flag on pulled one: refresh asked of app, not tab;
    /// pull on Unread then swipe to Favourites mustn't find unstarred row still
    /// there.
    refresh_epoch: std::sync::atomic::AtomicU64,
    /// One-shot result UI must SHOW, not merely reload for.
    ///
    /// Generation counter says "mirror changed"; can't carry server's answer to
    /// "test connection" or error text from rejected feed URL. Without this
    /// slot "Test connection" looks dead either way.
    notice: std::sync::Mutex<Option<Notice>>,
    /// Result of latest "fetch original content", addressed to entry asked for.
    ///
    /// NOT on `generation`: "generation moved while fetching" ≠ "my scrape
    /// landed"; mark-read-after-N-seconds bumps in between, would clear
    /// `fetching` and re-read mirror before scrape wrote (old body,
    /// dead-looking menu item). Addressed slot can't be stolen by unrelated
    /// bump or consumed by article that didn't ask.
    fetch_outcome: std::sync::Mutex<Option<FetchOutcome>>,
}

/// How a server-side scrape ended, and for which entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    /// The entry the scrape was asked for.
    pub entry_id: i64,
    /// One of the `FETCH_*` constants in `crate::article`.
    pub status: i32,
    /// Foreign text when server explained failure; else empty. Plain text
    /// (§9.3).
    pub message: String,
}

/// Something the worker produced that a page must display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `GET /v1/me` answered. `message` is the username, or the error.
    ConnectionTested {
        /// Whether the server accepted the credentials.
        ok: bool,
        /// The username, or the error text.
        message: String,
    },
    /// Subscribe or unsubscribe finished. `message` = server's error text on
    /// failure: foreign, plain text.
    SubscriptionChanged {
        /// Whether the server accepted the change.
        ok: bool,
        /// The server's error text on failure; empty on success.
        message: String,
    },
    /// Feed settings saved, or not. `message` = server's error text: foreign,
    /// plain text.
    FeedUpdated {
        /// Whether the server accepted the change.
        ok: bool,
        /// The server's error text on failure; empty on success.
        message: String,
    },
    /// Refresh failed. `message` foreign; plain text.
    ///
    /// `auth` separates "server rejected key" from rest: different asks of user
    /// (fix key vs retry later). Bool, not variant: variant duplicates
    /// take-and-re-post plumbing on every page draining slot, for one bit.
    ///
    /// On `auth` message EMPTY: server's text for rejected key not actionable;
    /// page supplies fixed translated line.
    SyncFailed {
        /// Whether the server rejected the API key.
        auth: bool,
        /// What went wrong; empty when `auth` is set.
        message: String,
    },
}

impl SyncSignal {
    /// Called from the worker thread when the mirror changed.
    pub fn bump(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Leave result for UI. Called from worker thread.
    ///
    /// Poisoned lock ignored, not unwrapped: §9.5 forbids panic unwinding into
    /// Qt frames; dropped notice costs status line, not data.
    pub fn post(&self, notice: Notice) {
        if let Ok(mut slot) = self.notice.lock() {
            *slot = Some(notice);
        }
    }

    /// Leave a scrape result for the open article. Called from the worker.
    pub fn post_fetch_outcome(&self, outcome: FetchOutcome) {
        if let Ok(mut slot) = self.fetch_outcome.lock() {
            *slot = Some(outcome);
        }
    }

    /// Take scrape result IF addressed to `entry_id`.
    ///
    /// Result for other entry left in place: user may open second article while
    /// first scraping; going back still shows what happened.
    #[must_use]
    pub fn take_fetch_outcome(&self, entry_id: i64) -> Option<FetchOutcome> {
        let mut slot = self.fetch_outcome.lock().ok()?;
        if slot.as_ref().is_some_and(|o| o.entry_id == entry_id) {
            slot.take()
        } else {
            None
        }
    }

    /// Take the pending notice, if any. Called from the Qt thread.
    #[must_use]
    pub fn take_notice(&self) -> Option<Notice> {
        self.notice.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Raise or lower sync spinner. Raised on Qt thread for user-initiated
    /// sync; lowered by worker when command ends.
    pub fn set_running(&self, running: bool) {
        self.running
            .store(running, std::sync::atomic::Ordering::Release);
    }

    /// Record that the user asked for a fresh list. Called from the Qt thread.
    pub fn mark_refreshed(&self) {
        self.refresh_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// How many times the user has asked for a fresh list; see `refresh_epoch`.
    #[must_use]
    pub fn refresh_epoch(&self) -> u64 {
        self.refresh_epoch
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The mirror's change counter, bumped by [`SyncSignal::bump`].
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether the sync spinner is up.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_no_context_before_startup() {
        // QML-constructed models before install() degrade, no panic.
        // (Per-thread slot: not order-dependent.)
        assert!(current().is_none());
    }

    /// Context over temp mirror, pointed at origin nothing answers on.
    fn test_context(dir: &tempfile::TempDir) -> Rc<AppContext> {
        let db = Database::open(&dir.path().join("mirror.sqlite")).expect("mirror");
        context_for_test(
            db,
            url::Url::parse("https://unreachable.invalid/").expect("url"),
        )
    }

    #[test]
    fn a_reentrant_borrow_returns_none_rather_than_panicking() {
        // §9.5: panic here unwinds into Qt C++ = UB; every mirror borrow
        // fallible. `try_borrow` → `borrow` must fail here (recoverable `None`
        // turned UB).
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = test_context(&dir);

        // Plain borrow succeeds.
        assert!(ctx.read(|_| ()).is_some());
        assert!(ctx.write(|_| ()).is_some());

        // Write inside read, read inside write: how it happens for real (model
        // reading mirror for row, calling something that writes).
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

        // Refusal temporary: borrow released afterwards.
        assert!(ctx.write(|_| ()).is_some());
    }

    fn account_at(dir: &tempfile::TempDir, server: &str) -> AppPaths {
        let paths = AppPaths::under(dir.path().join("harbour-vuo"));
        crate::worker::save_account(
            &paths.account,
            &Account {
                server_url: server.to_owned(),
                token: "k".to_owned(),
                // "Manual only", not real default hourly. Tests build REAL
                // workers against dead address; automatic sync would dial it,
                // sit out connect timeout before binary exits. §8.1: checks
                // touch no network.
                sync_interval_index: 0,
                ..Account::default()
            },
        )
        .expect("write the account");
        paths
    }

    #[test]
    fn refresh_installs_a_context_for_a_stored_account() {
        // First-run path: `main` builds context once before any account; this
        // call must work afterwards.
        let dir = tempfile::tempdir().expect("tempdir");
        // Plain http, as WireGuard-only deployment uses. Transport accepts it;
        // nothing here may quietly require TLS.
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

        // Idempotent: same account returns same context, no worker restart per
        // save.
        let again = refresh(&paths).expect("a context");
        assert!(Rc::ptr_eq(&ctx, &again));
    }

    #[test]
    fn refresh_reports_an_address_that_is_not_a_url_rather_than_swallowing_it() {
        // `10.77.0.1:8083` natural to type, not URL. Saves fine, so failure
        // lands here; must surface to user, not vanish into filtered log line.
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
    fn changing_the_account_does_not_start_a_second_worker() {
        // Thread created once at start-up, given each account in turn. Creating
        // later kills process on device (`Worker::spawn`): "user edited server
        // address" must not route back to `thread::Builder::spawn`.
        let dir = tempfile::tempdir().expect("tempdir");
        let first = refresh(&account_at(&dir, "http://10.77.0.1:8083/")).expect("a context");
        let worker = shared_worker().expect("a worker");

        let second = refresh(&account_at(&dir, "http://10.77.0.2:8083/")).expect("a context");
        assert!(
            !Rc::ptr_eq(&first, &second),
            "a changed account is a different context"
        );
        assert!(
            Rc::ptr_eq(&worker, &shared_worker().expect("a worker")),
            "but the same worker thread, handed the new account"
        );
        assert!(
            second.send(Command::TestConnection),
            "and it is still listening"
        );
    }

    /// Rule `take_fetch_outcome` documents.
    ///
    /// Scrape result for one article not consumed by another: user may open
    /// second article while first fetching; going back still shows result. Slot
    /// one-shot too (second take finds nothing), else stale result reappears
    /// every poll.
    #[test]
    fn a_scrape_result_is_only_taken_by_the_article_that_asked_for_it() {
        let signal = SyncSignal::default();
        signal.post_fetch_outcome(FetchOutcome {
            entry_id: 7,
            status: crate::article::FETCH_OK,
            message: String::new(),
        });

        assert!(
            signal.take_fetch_outcome(9).is_none(),
            "an article that did not ask for this result must not consume it"
        );
        let taken = signal
            .take_fetch_outcome(7)
            .expect("the article it is addressed to takes it");
        assert_eq!(taken.entry_id, 7);
        assert_eq!(taken.status, crate::article::FETCH_OK);

        assert!(
            signal.take_fetch_outcome(7).is_none(),
            "the slot is one-shot; a stale result must not survive to the next poll"
        );
    }

    #[test]
    fn the_sync_signal_counts_generations() {
        // Models poll this to decide reload. `bump` not incrementing → UI never
        // refreshes after sync, shows stale articles; poll not comparing →
        // every model reloads from SQLite twice a second forever. Both
        // directions tested.
        let signal = SyncSignal::default();
        assert_eq!(signal.generation(), 0);
        signal.bump();
        assert_eq!(signal.generation(), 1);
        signal.bump();
        assert_eq!(signal.generation(), 2, "two bumps must advance by two");

        // Observable from other thread, only real use: worker bumps, Qt thread
        // polls.
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
