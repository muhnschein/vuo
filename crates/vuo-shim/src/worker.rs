//! Background sync worker.
//!
//! # Why worker thread, not futures on Qt event loop
//!
//! `qmetaobject` exposes `execute_async` (polls future from Qt event loop),
//! which looks like §5's "async runtime driven from the Qt event loop". Doesn't
//! work for Vuo's sync; recorded so nobody retries.
//!
//! `reqwest` async client needs Tokio *reactor* (IO driver waking futures on
//! socket readiness). Polling reqwest future from Qt loop with no Tokio runtime
//! entered panics at first socket registration: no reactor.
//!
//! So network and database work runs on dedicated thread with own
//! current-thread Tokio runtime; nothing it produces touches `QObject`
//! directly. Rule that matters, kept structurally: results reach UI via
//! [`crate::context::SyncSignal`], polled by Qt thread; every `QObject` touch
//! on owning thread.
//!
//! `on_event` = log sink only. Not wrapped in `queued_callback`: would tie
//! building worker to live Qt event loop (start-up has one; settings screen
//! rebuilding after save can't promise).
//!
//! Division follows §5 "models observe SQLite": worker writes mirror, signals;
//! models re-read on Qt thread. Sync results never passed through channel as
//! data.

use crate::context::{FetchOutcome, Notice};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use vuo_core::api::{MinifluxClient, Transport, TransportConfig};
use vuo_core::db::outbox::{self, DesiredValue};
use vuo_core::db::{store, Database};
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::redact::ApiToken;
use vuo_core::sync::{self, SyncOptions};

/// How long mark-read or star waits before worker sends it.
///
/// Outbox batches WITHIN flush (`replay::flush` groups by desired value:
/// thousand marks = handful of requests). This batches flushes themselves: UI
/// fires [`Command::FlushOutbox`] per tap; acting on each = thirty requests for
/// thirty marks.
///
/// On phone that's the expensive part: each request re-arms radio connected
/// state, on cellular holds it for tail timer. Thirty over a few minutes keeps
/// modem out of idle most of that time.
///
/// Window = MAXIMUM AGE, not idle timeout: first request arms it, later ones
/// don't push back; intent never delayed more than this however steadily reader
/// taps. Forty-five seconds collapses normal session into one or two requests,
/// short enough that marking then checking another device shows it.
///
/// Nothing lost if window doesn't elapse: intent already in durable, idempotent
/// outbox surviving kill; [`FLUSH_AFTER_START`] picks it up next time.
pub const FLUSH_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(45);

/// How soon after account configured leftover outbox is sent.
///
/// Debounce means closing Vuo mid-session leaves intents. Next interval could
/// be an hour: start-up checks outbox, arms flush if non-empty. One request per
/// launch, only when something to send.
///
/// Not flush on [`Command::Shutdown`]: `Worker::drop` sends that then JOINS on
/// dropping thread (Qt thread when settings rebuilds context). Network request
/// there freezes UI up to transport's whole 120-second budget.
pub const FLUSH_AFTER_START: std::time::Duration = std::time::Duration::from_secs(10);

/// What worker does when wait ends with no command arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueWork {
    /// The interval sync is due.
    Sync,
    /// The debounced outbox flush is due; see [`FLUSH_DEBOUNCE`].
    Flush,
}

/// Nearer of two deadlines, if any.
#[must_use]
pub fn earliest(
    a: Option<std::time::Instant>,
    b: Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Next interval sync after `now`, or `None` for "Manual only".
///
/// One-minute floor not policy: `interval` from stored index; corrupt or
/// hand-edited account file could hold zero → tight sync loop.
#[must_use]
pub fn next_interval_after(
    interval_minutes: Option<i64>,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    // Checked: interval from settings; `Instant` pushed past clock range
    // panics, no wrap.
    interval_minutes
        .and_then(|minutes| minutes.max(1).unsigned_abs().checked_mul(60))
        .and_then(|secs| now.checked_add(std::time::Duration::from_secs(secs)))
}

/// Which scheduled work fell due at `now`.
///
/// Sync replays outbox first: both deadlines passed → run sync, waiting flush
/// subsumed, not sent separately moment earlier.
#[must_use]
pub fn due_work(
    next_sync: Option<std::time::Instant>,
    flush_due: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<DueWork> {
    if next_sync.is_some_and(|at| at <= now) {
        Some(DueWork::Sync)
    } else if flush_due.is_some_and(|at| at <= now) {
        Some(DueWork::Flush)
    } else {
        None
    }
}

/// Everything worker needs to serve one account.
///
/// Via [`Command::Configure`], not captured at thread start: thread starts
/// before any account exists.
pub struct WorkerAccount {
    /// Mirror. Worker opens own connection; Qt thread has second; WAL makes
    /// that safe.
    pub database: PathBuf,
    /// The Miniflux instance to talk to.
    pub server: url::Url,
    /// The API key, which redacts itself in logs.
    pub token: ApiToken,
    /// TLS settings, including a custom CA when one is configured.
    pub transport: TransportConfig,
}

impl std::fmt::Debug for WorkerAccount {
    /// Opaque, for same reason as [`ApiToken`].
    ///
    /// Token redacts itself, nothing else here does; DERIVED `Debug` puts all
    /// of it in any log line formatting a `Command`. Server is `Url`, prints
    /// userinfo (`https://user:pass@host/` gets pasted into address field; §9.1
    /// keeps it out of settings error text). Log line read by more people than
    /// error label: journald, bug reports. Mirror path under user's home;
    /// transport config carries whole CA certificate.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerAccount { .. }")
    }
}

/// What UI can ask worker to do.
///
/// Only network-needing operations. Local mutations (mark read, star) applied
/// inline on Qt thread ([`apply_local_status`]): channel hop between tap and UI
/// update buys nothing.
#[derive(Debug)]
pub enum Command {
    /// Run a full sync pass.
    Sync,
    /// Ask for outbox flush, no pull.
    ///
    /// REQUEST, not order. Every mark-read and star sends one; each as own
    /// HTTPS round trip would re-arm radio per tap. Worker coalesces: see
    /// [`FLUSH_DEBOUNCE`].
    FlushOutbox,
    /// Subscribe to feed. *Server* discovers and fetches it; §3: local feed
    /// fetching = project's most important boundary.
    Subscribe {
        /// The address the user entered; the server finds the feed from it.
        feed_url: String,
    },
    /// Unsubscribe from a feed on the server.
    Unsubscribe {
        /// The server's id for the feed.
        feed_id: i64,
    },
    /// Change a feed's settings on the server, then mirror the result.
    UpdateFeed {
        /// The server's id for the feed.
        feed_id: i64,
        /// The settings to change; fields left `None` are not touched.
        update: vuo_core::api::client::FeedPatch,
    },
    /// Ask server to scrape original article (§3: server endpoint, never local
    /// Readability port).
    FetchOriginal {
        /// The server's id for the entry.
        entry_id: i64,
    },
    /// Verify the configured credentials, for the settings screen.
    TestConnection,
    /// Serve this account from now on, replacing previous.
    ///
    /// Boxed: much largest `Command` payload; else every variant padded to its
    /// size in channel.
    Configure(Box<WorkerAccount>),
    /// How often worker syncs alone, minutes; `None` = "Manual only". Sent when
    /// context built and on each Settings save. Worker schedules next sync from
    /// last known one (stamp beside mirror): restart with fresh mirror doesn't
    /// sync at once.
    SetSyncInterval {
        /// Minutes between automatic syncs, or `None` for "Manual only".
        minutes: Option<i64>,
    },
    /// How long read, unfavourited articles kept in mirror, seconds; `None` =
    /// keep everything. Sent beside [`Command::SetSyncInterval`], applied at
    /// end of next pass.
    SetRetention {
        /// The retention window in seconds, or `None` to keep everything.
        seconds: Option<i64>,
    },
    /// Whether user asked Vuo to sync only over Wi-Fi.
    ///
    /// Only AUTOMATIC work consults it (interval sync, coalesced outbox flush).
    /// Anything user started goes out whatever network: pulled refresh,
    /// subscription, connection test. Setting is about what Vuo does
    /// unprompted.
    SetWifiOnly {
        /// True to sync automatically only over Wi-Fi.
        enabled: bool,
    },
    /// Stop the worker thread. Sent by `Worker`'s `Drop`.
    Shutdown,
}

impl Command {
    /// Fixed name, for logs.
    ///
    /// Never `{:?}` a `Command` into log: `Configure` carries account;
    /// `WorkerAccount` opaque, a later variant might not be. Logged thing =
    /// which command arrived: a word, not payload.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Command::Sync => "Sync",
            Command::FlushOutbox => "FlushOutbox",
            Command::Subscribe { .. } => "Subscribe",
            Command::Unsubscribe { .. } => "Unsubscribe",
            Command::UpdateFeed { .. } => "UpdateFeed",
            Command::FetchOriginal { .. } => "FetchOriginal",
            Command::TestConnection => "TestConnection",
            Command::Configure(_) => "Configure",
            Command::SetSyncInterval { .. } => "SetSyncInterval",
            Command::SetRetention { .. } => "SetRetention",
            Command::SetWifiOnly { .. } => "SetWifiOnly",
            Command::Shutdown => "Shutdown",
        }
    }
}

/// What the worker reports back.
#[derive(Debug, Clone)]
pub enum Event {
    /// A sync pass has begun.
    SyncStarted,
    /// A sync pass, or an outbox flush, completed.
    SyncFinished {
        /// The mirror's unread count afterwards.
        unread: i64,
        /// Whether the mirror was written to.
        changed: bool,
    },
    /// Already-redacted, user-presentable text. Rendered as plain text (§9.3).
    SyncFailed {
        /// What went wrong.
        message: String,
    },
    /// The API key was rejected; the UI should send the user to settings.
    AuthFailed,
    /// A subscribe or unsubscribe finished. `message` is empty on success.
    SubscriptionChanged {
        /// Whether the server accepted the change.
        ok: bool,
        /// The server's error text on failure (foreign text).
        message: String,
    },
    /// A feed's settings were changed. `message` is empty on success.
    FeedUpdated {
        /// The server's id for the feed.
        feed_id: i64,
        /// Whether the server accepted the change.
        ok: bool,
        /// The server's error text on failure (foreign text).
        message: String,
    },
    /// The server returned scraped content for an entry.
    OriginalContentFetched {
        /// The server's id for the entry.
        entry_id: i64,
        /// Whether the scraped content was stored.
        ok: bool,
    },
    /// Result of a settings-screen connection test.
    ConnectionTested {
        /// Whether the server accepted the credentials.
        ok: bool,
        /// The username on success, the error text otherwise.
        message: String,
    },
}

/// Clears sync spinner when command iteration ends, however it ends.
///
/// `Drop` impl, not call: per-arm clearing gets forgotten (failed refresh on
/// dropped VPN would leave `running` true forever: list and cover spin,
/// "Nothing to read" suppressed). Flag cleared because iteration ended; future
/// edit can't forget.
struct CommandGuard<'a> {
    signal: &'a crate::context::SyncSignal,
    /// Set by arm that actually wrote mirror.
    ///
    /// Bump makes every model `reload()` (full reset on plain `ListView`,
    /// scrolls to top). Bumping for no-op command moves page under reader for
    /// nothing: opt-in.
    changed: bool,
    /// Whether this command owns spinner.
    ///
    /// Only user-initiated `Sync` raises it (`EntryModel::requestSync`), so
    /// only it lowers. Decided from command at construction: opportunistic
    /// `FlushOutbox` from star tap physically can't switch off running
    /// refresh's spinner.
    clears_spinner: bool,
    /// Whether command big enough to return its peak to kernel. See
    /// [`crate::memory`].
    ///
    /// Decided from command like `clears_spinner`; true for the two that
    /// inflate, parse, drop whole corpus or article body. Not small ones: star
    /// tap sends `FlushOutbox`; walking every arena per tap buys nothing, costs
    /// thread that must never make UI wait.
    trims_allocator: bool,
}

impl Drop for CommandGuard<'_> {
    fn drop(&mut self) {
        // Before signal: bump sets every model reloading on Qt thread; trim
        // takes each arena lock. First = allocator done before UI asks.
        if self.trims_allocator {
            crate::memory::release_free_memory();
        }
        // Clear BEFORE bump, order load-bearing. `pollSync` spends generation
        // first time seen: poll between bump and clear reads `running` true,
        // never revisits that generation; spinner survives own clear.
        if self.clears_spinner {
            self.signal.set_running(false);
        }
        if self.changed {
            self.signal.bump();
        }
    }
}

/// Write accepted feed update into mirror.
///
/// Only fields update carried: `None` = user didn't touch; writing default
/// would revert value set from web UI.
///
/// # Errors
///
/// [`vuo_core::Error::Db`] if any update fails.
pub fn apply_feed_patch(
    tx: &rusqlite::Transaction<'_>,
    feed_id: i64,
    update: &vuo_core::api::client::FeedPatch,
) -> vuo_core::Result<()> {
    if let Some(title) = &update.title {
        tx.execute(
            "UPDATE feeds SET title = ?2 WHERE id = ?1",
            rusqlite::params![feed_id, title],
        )?;
    }
    if let Some(category_id) = update.category_id {
        // Same placeholder trick as `upsert_feed`: categories fetched
        // separately; chosen category may be absent from mirror, FK would
        // reject.
        tx.execute(
            "INSERT INTO categories (id, title) VALUES (?1, \'\') \
             ON CONFLICT(id) DO NOTHING",
            [category_id],
        )?;
        tx.execute(
            "UPDATE feeds SET category_id = ?2 WHERE id = ?1",
            rusqlite::params![feed_id, category_id],
        )?;
    }
    if let Some(crawler) = update.crawler {
        tx.execute(
            "UPDATE feeds SET crawler = ?2 WHERE id = ?1",
            rusqlite::params![feed_id, i64::from(crawler)],
        )?;
    }
    if let Some(disabled) = update.disabled {
        tx.execute(
            "UPDATE feeds SET disabled = ?2 WHERE id = ?1",
            rusqlite::params![feed_id, i64::from(disabled)],
        )?;
    }
    if let Some(hide_globally) = update.hide_globally {
        tx.execute(
            "UPDATE feeds SET hide_globally = ?2 WHERE id = ?1",
            rusqlite::params![feed_id, i64::from(hide_globally)],
        )?;
    }
    Ok(())
}

/// What to do with a body the server scraped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrapeVerdict {
    /// Worth storing over what the feed gave us.
    Store,
    /// Server answered with nothing. Miniflux returns 200 with empty `content`
    /// when scraper rules match nothing (paywalls, JS-rendered articles,
    /// routinely).
    Empty,
    /// Byte-for-byte what's stored: nothing to show for tap. Not `Empty`: not
    /// failure, feed already carried full article.
    Unchanged,
}

/// Whether scraped body replaces stored one.
///
/// Pure, separate from command handler: interesting cases all about *what
/// server returned*; none need database, runtime, network.
#[must_use]
pub fn classify_scrape(previous: &str, scraped: &str) -> ScrapeVerdict {
    if scraped.trim().is_empty() {
        ScrapeVerdict::Empty
    } else if scraped == previous {
        ScrapeVerdict::Unchanged
    } else {
        ScrapeVerdict::Store
    }
}

/// Handle to worker thread.
///
/// Drop sends [`Command::Shutdown`], joins thread.
pub struct Worker {
    tx: mpsc::Sender<Command>,
    handle: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Worker { .. }")
    }
}

impl Worker {
    /// Start worker thread, no account yet.
    ///
    /// **Called once at start-up, before UI exists**
    /// ([`crate::context::start_worker`]). Creating thread from QML tap
    /// (first-run path) killed process on Jolla Phone 2026: last log line was
    /// the one before `thread::Builder::spawn`; neither parent's next nor
    /// thread's first arrived. Same call from `main` works every launch. Thread
    /// created while process small; account arrives later as
    /// [`Command::Configure`] (channel send, can't fail that way).
    ///
    /// One thread serves each account in turn: no churn, no retired thread left
    /// detached mid-request.
    ///
    /// `on_event` runs **on worker thread**. Must not touch `QObject`:
    /// UI-visible results travel via [`crate::context::SyncSignal`], polled by
    /// Qt thread. Caller needing a `QObject` here wraps with
    /// `qmetaobject::queued_callback` first, and owes that primitive its event
    /// loop.
    pub fn spawn(
        signal: std::sync::Arc<crate::context::SyncSignal>,
        on_event: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Command>();

        let handle = thread::Builder::new()
            .name("vuo-sync".to_owned())
            .spawn(move || {
                // First statement in thread; parent logs one right after
                // `spawn` returns. Which arrives (or neither) says whether
                // death belongs to Qt thread or this one; nothing else tells
                // once process gone. See docs/testing.md.
                tracing::info!("the sync worker thread is running");
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        on_event(Event::SyncFailed {
                            message: format!("could not start the sync runtime: {e}"),
                        });
                        return;
                    }
                };
                tracing::info!("the sync runtime is up");

                // Account served, or `None` until given.
                //
                // Fresh install arrives with nothing configured: ordinary, not
                // failure. Thread waits rather than exits; account user is
                // about to type needs no new thread. `last_sync` sits beside
                // mirror, per `AppPaths::under`.
                let mut db: Option<Database> = None;
                let mut client: Option<MinifluxClient> = None;
                let mut last_sync = PathBuf::new();

                // Worker's own sync, on user's interval.
                //
                // Vuo = one process (Harbour allows no other): no outside
                // timer; worker keeps cadence, waits for command only until
                // next sync due, then runs one as if `Command::Sync` arrived.
                // Manual and automatic = same code; differs only in who asked.
                let mut interval: Option<i64> = None;
                let mut next_sync: Option<std::time::Instant> = None;
                // Retention window, or `None` = keep everything (install that
                // never opened Settings).
                let mut retention: Option<i64> = None;
                // When coalesced outbox flush falls due; `None` while nothing
                // waiting. See [`FLUSH_DEBOUNCE`].
                let mut flush_due: Option<std::time::Instant> = None;
                // User's "Only sync on Wi-Fi". Checked against network read
                // when AUTOMATIC work falls due, never one pushed from Qt
                // thread: matters exactly while app minimised and nothing
                // pushes. See `vuo_core::net`.
                let mut wifi_only = false;
                loop {
                    // Only served account has work that can fall due. Without
                    // filter, interval set before account arrives makes every
                    // wait expire at once; loop spins.
                    let due = earliest(next_sync, flush_due).filter(|_| client.is_some());
                    // Set only when command below came from deadline, not UI.
                    // UI-requested flush arms debounce; debounce-produced flush
                    // actually runs.
                    let mut flush_fell_due = false;
                    let command = match due {
                        Some(at) => {
                            let wait = at.saturating_duration_since(std::time::Instant::now());
                            match rx.recv_timeout(wait) {
                                Ok(command) => command,
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    let now = std::time::Instant::now();
                                    let work = due_work(next_sync, flush_due, now);
                                    // Read HERE, once, for whichever work fell
                                    // due. Not at start-up, not on tap: reader
                                    // leaves Wi-Fi with app on cover; earlier
                                    // reading describes network that's gone.
                                    if !vuo_core::net::probe().allows(wifi_only) {
                                        tracing::info!(
                                            work = ?work,
                                            "the network does not allow automatic work; \
                                             leaving it for the next one"
                                        );
                                        // Flush just waits: later tap re-arms
                                        // it, sync carries outbox anyway. Sync
                                        // goes back on own interval, when
                                        // network is looked at again: no
                                        // polling, no extra wakeup.
                                        flush_due = None;
                                        if matches!(work, Some(DueWork::Sync)) {
                                            next_sync = next_interval_after(interval, now);
                                        }
                                        continue;
                                    }
                                    if let Some(DueWork::Flush) = work {
                                        flush_due = None;
                                        flush_fell_due = true;
                                        Command::FlushOutbox
                                    } else {
                                        // `None` can't happen (wait ended
                                        // because a deadline passed); sync is
                                        // safe reading either way.
                                        //
                                        // As `requestSync` before sending:
                                        // cover and list show it.
                                        signal.set_running(true);
                                        Command::Sync
                                    }
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        None => match rx.recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        },
                    };

                    // UI-requested flush only arms window; deadline above
                    // sends. Re-arming per request = idle timeout; steady
                    // tapper never reaches end. First request sets deadline,
                    // later ones leave it.
                    if matches!(command, Command::FlushOutbox) && !flush_fell_due {
                        if flush_due.is_none() {
                            flush_due = std::time::Instant::now().checked_add(FLUSH_DEBOUNCE);
                        }
                        continue;
                    }
                    // Shutdown handled above guard: no spinner to clear;
                    // draining queue matters more. `Sync` queued behind stop
                    // would leave `running` set with nothing alive to clear it.
                    if matches!(command, Command::Shutdown) {
                        while rx.try_recv().is_ok() {}
                        signal.set_running(false);
                        break;
                    }

                    // Two commands needing no account, taken before guard:
                    // neither touches mirror nor owns spinner. Both `continue`;
                    // value moved out of `command` unwanted below.
                    match command {
                        Command::Configure(account) => {
                            last_sync = account.database.with_file_name("last-sync");
                            // Both reported then left `None`: account that
                            // can't open or whose transport won't build is
                            // dead; serving PREVIOUS account's data under new
                            // name worse than nothing.
                            db = match Database::open(&account.database) {
                                Ok(db) => {
                                    tracing::info!("the worker has opened the mirror");
                                    Some(db)
                                }
                                Err(e) => {
                                    on_event(Event::SyncFailed {
                                        message: e.to_string(),
                                    });
                                    None
                                }
                            };
                            let WorkerAccount {
                                server,
                                token,
                                transport,
                                ..
                            } = *account;
                            client = match Transport::new(server, token, &transport) {
                                Ok(t) => {
                                    tracing::info!("the sync worker is ready");
                                    Some(MinifluxClient::new(t))
                                }
                                Err(e) => {
                                    on_event(Event::SyncFailed {
                                        message: e.to_string(),
                                    });
                                    None
                                }
                            };
                            // Stamp belongs to account: new account's first
                            // sync scheduled from ITS history.
                            next_sync =
                                next_sync_delay(interval, read_sync_time(&last_sync), chrono_now())
                                    .and_then(|delay| std::time::Instant::now().checked_add(delay));
                            // Anything last session left unsent goes shortly
                            // after start-up, not next interval (maybe an
                            // hour). See [`FLUSH_AFTER_START`].
                            flush_due = db
                                .as_ref()
                                .and_then(|db| outbox::len(db.conn()).ok())
                                .filter(|pending| *pending > 0)
                                .and_then(|_| std::time::Instant::now().checked_add(FLUSH_AFTER_START));
                            continue;
                        }
                        Command::SetSyncInterval { minutes } => {
                            interval = minutes;
                            next_sync =
                                next_sync_delay(interval, read_sync_time(&last_sync), chrono_now())
                                    .and_then(|delay| std::time::Instant::now().checked_add(delay));
                            continue;
                        }
                        Command::SetRetention { seconds } => {
                            // Recorded, not acted on. Pruning here deletes
                            // articles the instant reader taps Save, in front
                            // of them; end of pass does it with rest of sync's
                            // changes, once.
                            retention = seconds;
                            continue;
                        }
                        Command::SetWifiOnly { enabled } => {
                            wifi_only = enabled;
                            continue;
                        }
                        _ => {}
                    }

                    // Everything else needs account. None → nothing to answer
                    // with; dropping right: UI can't send until context exists,
                    // context built only after account handed over.
                    let (Some(db), Some(client)) = (db.as_mut(), client.as_ref()) else {
                        tracing::warn!(
                            command = command.name(),
                            "no account is configured; dropping the command"
                        );
                        continue;
                    };

                    // See CommandGuard: spinner clearing structural;
                    // early-return arm (or later one) can't leave it spinning.
                    // Arms opt into bump.
                    let mut guard = CommandGuard {
                        signal: &signal,
                        changed: false,
                        clears_spinner: matches!(command, Command::Sync),
                        trims_allocator: matches!(
                            command,
                            Command::Sync | Command::FetchOriginal { .. }
                        ),
                    };
                    match command {
                        // All three handled above, before guard.
                        Command::Shutdown
                        | Command::Configure(_)
                        | Command::SetSyncInterval { .. }
                        | Command::SetRetention { .. }
                        | Command::SetWifiOnly { .. } => {}
                        Command::Sync => {
                            // Pass replays outbox first: debounced intents go
                            // with it, mustn't be sent again moment later.
                            flush_due = None;
                            on_event(Event::SyncStarted);
                            let options = SyncOptions {
                                retention_secs: retention,
                                ..SyncOptions::default()
                            };
                            match runtime.block_on(sync::sync(db, client, options)) {
                                Ok(report) if report.replay.auth_failed => {
                                    guard.changed = true;
                                    signal.post(Notice::SyncFailed {
                                        auth: true,
                                        message: String::new(),
                                    });
                                    on_event(Event::AuthFailed);
                                }
                                Ok(report) => {
                                    // Next start schedules from this sync,
                                    // doesn't sync at once.
                                    record_sync_time(&last_sync);
                                    let unread = store::unread_count(db.conn()).unwrap_or(0);
                                    let changed = report.pull.upserted > 0
                                        || report.pull.removed > 0
                                        || report.entries_deleted > 0
                                        || report.replay.confirmed > 0
                                        || report.icons_fetched > 0;
                                    guard.changed = changed;
                                    on_event(Event::SyncFinished { unread, changed });
                                }
                                Err(e) if e.is_auth_failure() => {
                                    // `sync` commits incrementally: late
                                    // failure may still have written.
                                    guard.changed = true;
                                    signal.post(Notice::SyncFailed {
                                        auth: true,
                                        message: String::new(),
                                    });
                                    on_event(Event::AuthFailed);
                                }
                                Err(e) => {
                                    guard.changed = true;
                                    // Already redacted: Error Display never
                                    // carries token or userinfo. Still foreign:
                                    // page renders plain text.
                                    let message = e.to_string();
                                    signal.post(Notice::SyncFailed {
                                        auth: false,
                                        message: message.clone(),
                                    });
                                    on_event(Event::SyncFailed { message });
                                }
                            }
                            // Either way next one is an interval away: down
                            // server not asked every few seconds.
                            next_sync = next_interval_after(interval, std::time::Instant::now());
                        }
                        Command::Subscribe { feed_url } => {
                            let result = runtime.block_on(client.create_feed(&feed_url, None));
                            match result {
                                Ok(_) => {
                                    // Pull immediately: new feed's entries
                                    // appear without waiting for timer.
                                    // Subscription succeeded, so failed pull
                                    // only logged; next timed sync brings
                                    // entries.
                                    if let Err(e) = runtime.block_on(sync::sync(
                                        db,
                                        client,
                                        SyncOptions {
                                            retention_secs: retention,
                                            ..SyncOptions::default()
                                        },
                                    )) {
                                        tracing::warn!(error = %e, "the sync after subscribing failed");
                                    }
                                    guard.changed = true;
                                    on_event(Event::SubscriptionChanged {
                                        ok: true,
                                        message: String::new(),
                                    });
                                }
                                Err(e) => on_event(Event::SubscriptionChanged {
                                    ok: false,
                                    message: e.to_string(),
                                }),
                            }
                        }
                        Command::Unsubscribe { feed_id } => {
                            match runtime.block_on(client.delete_feed(feed_id)) {
                                Ok(()) => {
                                    let removed = db.with_tx(|tx| {
                                        store::delete_feed(tx, vuo_core::model::FeedId(feed_id))
                                    });
                                    // DID change mirror. Bump needed, else
                                    // `FeedModel::pollSync` reports nothing and
                                    // deleted feed's row lingers until later
                                    // sync.
                                    guard.changed = removed.is_ok();
                                    signal.post(Notice::SubscriptionChanged {
                                        ok: removed.is_ok(),
                                        message: String::new(),
                                    });
                                    on_event(Event::SubscriptionChanged {
                                        ok: removed.is_ok(),
                                        message: String::new(),
                                    });
                                }
                                Err(e) => {
                                    signal.post(Notice::SubscriptionChanged {
                                        ok: false,
                                        message: e.to_string(),
                                    });
                                    on_event(Event::SubscriptionChanged {
                                        ok: false,
                                        message: e.to_string(),
                                    });
                                }
                            }
                        }
                        Command::UpdateFeed { feed_id, update } => {
                            // Server first, mirror second.
                            //
                            // Opposite order shows new name, then takes it back
                            // when PUT fails; feed silently reverting own name
                            // a second after rename worse than slow change. No
                            // outbox row: rename isn't done dozens of times
                            // offline; replay has no conflict story worth
                            // machinery.
                            match runtime.block_on(client.update_feed(feed_id, &update)) {
                                Ok(()) => {
                                    // Patch mirror, don't re-fetch feed list:
                                    // fields = exactly those accepted; full
                                    // pull per rename = second round trip user
                                    // waits on.
                                    let patched =
                                        db.with_tx(|tx| apply_feed_patch(tx, feed_id, &update));
                                    guard.changed = patched.is_ok();
                                    signal.post(Notice::FeedUpdated {
                                        ok: patched.is_ok(),
                                        message: String::new(),
                                    });
                                    on_event(Event::FeedUpdated {
                                        feed_id,
                                        ok: patched.is_ok(),
                                        message: String::new(),
                                    });
                                }
                                Err(e) => {
                                    signal.post(Notice::FeedUpdated {
                                        ok: false,
                                        message: e.to_string(),
                                    });
                                    on_event(Event::FeedUpdated {
                                        feed_id,
                                        ok: false,
                                        message: e.to_string(),
                                    });
                                }
                            }
                        }
                        Command::FetchOriginal { entry_id } => {
                            let id = EntryId(entry_id);
                            match runtime.block_on(client.fetch_original_content(id)) {
                                Ok(content) => {
                                    let previous = db
                                        .with_tx(|tx| {
                                            tx.query_row(
                                                "SELECT content FROM entries WHERE id = ?1",
                                                rusqlite::params![id.get()],
                                                |row| row.get::<_, String>(0),
                                            )
                                            .map_err(vuo_core::Error::from)
                                        })
                                        .unwrap_or_default();
                                    match classify_scrape(&previous, &content.content) {
                                        ScrapeVerdict::Store => {
                                            // Store scraped body against entry:
                                            // survives restart, readable
                                            // offline.
                                            let stored = db.with_tx(|tx| {
                                                tx.execute(
                                                    "UPDATE entries \
                                                     SET content = ?2, \
                                                         content_scraped = 1 \
                                                     WHERE id = ?1",
                                                    rusqlite::params![id.get(), content.content],
                                                )
                                                .map_err(vuo_core::Error::from)
                                            });
                                            // Same as Unsubscribe: scraped body
                                            // in SQLite, open article stale
                                            // until reload.
                                            guard.changed = stored.is_ok();
                                            signal.post_fetch_outcome(FetchOutcome {
                                                entry_id,
                                                status: if stored.is_ok() {
                                                    crate::article::FETCH_OK
                                                } else {
                                                    crate::article::FETCH_FAILED
                                                },
                                                message: String::new(),
                                            });
                                            on_event(Event::OriginalContentFetched {
                                                entry_id,
                                                ok: stored.is_ok(),
                                            });
                                        }
                                        verdict => {
                                            // Nothing worth storing. Writing
                                            // anyway would ERASE good article:
                                            // server scraping paywall or
                                            // JS-only page answers 200 with
                                            // empty body, which would overwrite
                                            // feed's content.
                                            signal.post_fetch_outcome(FetchOutcome {
                                                entry_id,
                                                status: match verdict {
                                                    ScrapeVerdict::Empty => {
                                                        crate::article::FETCH_EMPTY
                                                    }
                                                    _ => crate::article::FETCH_UNCHANGED,
                                                },
                                                message: String::new(),
                                            });
                                            on_event(Event::OriginalContentFetched {
                                                entry_id,
                                                ok: false,
                                            });
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Addressed to article that asked, NOT
                                    // posted as `SyncFailed` notice: that would
                                    // put "Refresh failed" atop ENTRY LIST for
                                    // scrape started inside article.
                                    signal.post_fetch_outcome(FetchOutcome {
                                        entry_id,
                                        status: if e.is_auth_failure() {
                                            crate::article::FETCH_AUTH
                                        } else {
                                            crate::article::FETCH_FAILED
                                        },
                                        message: if e.is_auth_failure() {
                                            String::new()
                                        } else {
                                            e.to_string()
                                        },
                                    });
                                    on_event(Event::OriginalContentFetched {
                                        entry_id,
                                        ok: false,
                                    });
                                }
                            }
                        }
                        Command::TestConnection => {
                            tracing::info!("asking the server who we are");
                            match runtime.block_on(client.me()) {
                                Ok(user) => {
                                    // Username = user's own from own server,
                                    // still plain text.
                                    signal.post(Notice::ConnectionTested {
                                        ok: true,
                                        message: user.username.clone(),
                                    });
                                    on_event(Event::ConnectionTested {
                                        ok: true,
                                        message: user.username,
                                    });
                                }
                                Err(e) => {
                                    signal.post(Notice::ConnectionTested {
                                        ok: false,
                                        message: e.to_string(),
                                    });
                                    on_event(Event::ConnectionTested {
                                        ok: false,
                                        message: e.to_string(),
                                    });
                                }
                            }
                        }
                        Command::FlushOutbox => {
                            // Belt and braces: deadline cleared where it fires;
                            // arm also reachable from command sent before any
                            // account.
                            flush_due = None;
                            match runtime.block_on(sync::replay::flush(db, client)) {
                                Ok(outcome) if outcome.auth_failed => {
                                    signal.post(Notice::SyncFailed {
                                        auth: true,
                                        message: String::new(),
                                    });
                                    on_event(Event::AuthFailed);
                                }
                                Ok(outcome) => {
                                    // Flush confirming or dropping rows changed
                                    // mirror (`flush` deletes confirmed outbox
                                    // rows, discards permanently refused). Bump
                                    // so `pendingActions` stays current and
                                    // dropped intent doesn't vanish silently.
                                    guard.changed = outcome.confirmed > 0 || outcome.dropped > 0;
                                    let unread = store::unread_count(db.conn()).unwrap_or(0);
                                    on_event(Event::SyncFinished {
                                        unread,
                                        changed: guard.changed,
                                    });
                                }
                                Err(e) => {
                                    // Only PERMANENT failure worth notice.
                                    // Fired per star and mark-read; reporting
                                    // routine offline flush = error per tap
                                    // while outbox does its job (replays next
                                    // sync).
                                    let message = e.to_string();
                                    if !e.is_transient() {
                                        signal.post(Notice::SyncFailed {
                                            auth: false,
                                            message: message.clone(),
                                        });
                                    }
                                    on_event(Event::SyncFailed { message });
                                }
                            }
                        }
                    }
                }
            })
            .ok();
        // Failed spawn = `None`, no panic: say which happened; never-created
        // worker behaves exactly like dead one.
        if handle.is_some() {
            tracing::info!("the sync worker thread is spawned");
        } else {
            tracing::warn!("the sync worker thread could not be created");
        }

        Worker { tx, handle }
    }

    /// Ask the worker to do something. Returns `false` if it has stopped.
    #[must_use]
    pub fn send(&self, command: Command) -> bool {
        self.tx.send(command).is_ok()
    }

    /// A sender for the context to hold.
    #[must_use]
    pub fn sender(&self) -> mpsc::Sender<Command> {
        self.tx.clone()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Already-stopped worker has no one to tell.
        self.tx.send(Command::Shutdown).ok();
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                tracing::error!("the sync worker panicked");
            }
        }
    }
}

/// Apply local mutation now, on calling (Qt) thread.
///
/// Synchronous, not via worker: fast local transaction; inline lets UI update
/// same frame as tap. Server hears on next flush: outbox's point.
///
/// # Errors
///
/// [`vuo_core::Error::Db`] if transaction can't open or commit, or outbox or
/// entry write fails.
pub fn apply_local_status(
    db: &mut Database,
    id: EntryId,
    status: EntryStatus,
) -> vuo_core::Result<()> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue(tx, id, DesiredValue::Status(status), now))
}

/// As [`apply_local_status`], for the starred flag.
///
/// # Errors
///
/// Returns [`vuo_core::Error::Db`] if the transaction cannot be opened or
/// committed, or the outbox or entry write fails.
pub fn apply_local_starred(db: &mut Database, id: EntryId, starred: bool) -> vuo_core::Result<()> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue(tx, id, DesiredValue::Starred(starred), now))
}

/// Apply same status to many entries at once, one transaction.
///
/// "Mark all read" in scope without server-side equivalent (unread, starred,
/// all): intent expanded over concrete entries user sees.
///
/// Returns count queued, always `ids.len()`.
///
/// # Errors
///
/// [`vuo_core::Error::Db`] if any write fails. One transaction: failure applies
/// none.
pub fn apply_local_status_bulk(
    db: &mut Database,
    ids: &[EntryId],
    status: EntryStatus,
) -> vuo_core::Result<usize> {
    let now = chrono_now();
    db.with_tx(|tx| {
        for id in ids {
            outbox::queue(tx, *id, DesiredValue::Status(status), now)?;
        }
        Ok(ids.len())
    })
}

/// Mark every unread entry in feed read, locally and in outbox.
///
/// Expands to concrete unread entries in mirror, one outbox row each; returns
/// count.
///
/// # Errors
///
/// [`vuo_core::Error::Db`] if reading unread entries or queueing fails;
/// transaction applies none.
pub fn apply_local_mark_feed_read(db: &mut Database, feed_id: i64) -> vuo_core::Result<usize> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue_mark_feed_read(tx, feed_id, now))
}

/// As [`apply_local_mark_feed_read`], for every feed in category.
///
/// # Errors
///
/// [`vuo_core::Error::Db`] if reading unread entries or queueing fails;
/// transaction applies none.
pub fn apply_local_mark_category_read(
    db: &mut Database,
    category_id: i64,
) -> vuo_core::Result<usize> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue_mark_category_read(tx, category_id, now))
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Where Vuo keeps data and credentials on device.
///
/// §7: *the API key is stored under the app's data directory with restrictive
/// permissions, relying on Sailfish's home encryption. No custom keyring, no
/// `SQLCipher`, unless a concrete threat model justifies it.*
///
/// Token **not** in SQLite mirror on purpose: mirror = cache deleted, copied
/// for debugging, handed to developer with bug report; credential would travel
/// along.
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// The SQLite mirror.
    pub database: PathBuf,
    /// The account file: server, API key and settings, as JSON.
    pub account: PathBuf,
    /// Expected location of user-supplied CA certificate, for self-hosted
    /// instance with private CA (§9.1). Fixed path, no file picker: rare
    /// deliberate act; user-chosen path = one more thing to validate.
    pub ca_certificate: PathBuf,
    /// When mirror last synced, Unix seconds in file.
    ///
    /// Written after each successful sync, read at worker start: session's
    /// first automatic sync scheduled from last one, not run on open. File
    /// beside database, not row: one small read, no query.
    pub last_sync: PathBuf,
}

impl AppPaths {
    /// Standard layout under explicit base directory.
    ///
    /// Split from [`AppPaths::resolve`]: layout and configured-or-not decision
    /// testable without mutating process-wide env (unsafe in threaded runner).
    #[must_use]
    pub fn under(base: impl Into<PathBuf>) -> Self {
        let base = base.into();
        AppPaths {
            database: base.join("vuo.sqlite"),
            account: base.join("account.json"),
            ca_certificate: base.join("ca.pem"),
            last_sync: base.join("last-sync"),
        }
    }

    /// Resolve standard locations, honouring `XDG_DATA_HOME`.
    ///
    /// `<data>/harbour-vuo/harbour-vuo`, two levels: only directory sandbox
    /// lets app write. Sailjail whitelists
    /// `$HOME/.local/share/<OrganizationName>/<ApplicationName>` from desktop
    /// entry's `[X-Sailjail]` section; both names `harbour-vuo` there.
    #[must_use]
    pub fn resolve() -> Option<Self> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?
            .join("harbour-vuo/harbour-vuo");
        Some(Self::under(base))
    }

    /// `Some` only once account written.
    ///
    /// `None` when never set up: not error, what first run looks like.
    #[must_use]
    pub fn configured(self) -> Option<Self> {
        self.account.exists().then_some(self)
    }

    /// Resolve paths and confirm an account has been configured.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::resolve()?.configured()
    }
}

/// Wait before next automatic sync, given interval and last sync (Unix
/// seconds), at `now`.
///
/// `None` interval = "Manual only": never. Never synced or overdue → due almost
/// at once (short pause: freshly opened app draws list before touching
/// network). Else remainder of interval.
#[must_use]
pub fn next_sync_delay(
    interval_minutes: Option<i64>,
    last_sync: Option<i64>,
    now: i64,
) -> Option<std::time::Duration> {
    const SOON: u64 = 10;
    let minutes = interval_minutes?;
    let Some(last) = last_sync else {
        return Some(std::time::Duration::from_secs(SOON));
    };
    let due_at = last.saturating_add(minutes.saturating_mul(60));
    let remaining = due_at.saturating_sub(now);
    Some(std::time::Duration::from_secs(
        remaining.max(0).unsigned_abs().max(SOON),
    ))
}

/// The last-sync stamp, if there is one.
#[must_use]
pub fn read_sync_time(stamp: &std::path::Path) -> Option<i64> {
    std::fs::read_to_string(stamp)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok())
}

/// Note mirror just synced. Best-effort: unwritten stamp costs one extra
/// background run, not data.
pub fn record_sync_time(stamp: &std::path::Path) {
    if let Err(e) = std::fs::write(stamp, format!("{}\n", chrono_now())) {
        tracing::info!(error = %e, path = %stamp.display(), "could not record the sync time");
    }
}

/// The stored account. Written with owner-only permissions.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Account {
    /// The Miniflux instance's address, as the user entered it.
    pub server_url: String,
    /// The API key.
    pub token: String,
    /// Whether to trust CA certificate at [`AppPaths::ca_certificate`].
    ///
    /// Off unless user enables *and* file exists. §9.1 offers per-host CA so
    /// nobody needs "ignore certificate errors" switch; none exists, this isn't
    /// one.
    #[serde(default)]
    pub use_custom_ca: bool,
    /// 0 strict, 1 ask, 2 allow. See `settings::MEDIA_*`.
    #[serde(default = "default_media_policy")]
    pub media_policy: i32,
    /// Index into `settings::SYNC_INTERVALS_MINUTES`.
    #[serde(default = "default_sync_interval_index")]
    pub sync_interval_index: i32,
    #[serde(default)]
    /// Whether automatic syncs are limited to Wi-Fi; see [`Command::SetWifiOnly`].
    pub wifi_only: bool,
    /// When an opened article is marked read. See `settings::MARK_READ_*`.
    #[serde(default = "default_mark_read_delay_index")]
    pub mark_read_delay_index: i32,
    /// How long read, unfavourited articles kept locally. Index into
    /// `settings::RETENTION_DAYS`; 0 keeps everything.
    #[serde(default)]
    pub retention_index: i32,
    /// Whether to notify when sync brings new articles while Vuo on cover.
    ///
    /// Off unless asked, and off for account predating setting: update starting
    /// to interrupt reader because author thought so = surprise, not setting
    /// (same reasoning as `settings::RETENTION_DEFAULT_INDEX`, other
    /// direction).
    #[serde(default)]
    pub notify_new_articles: bool,
}

/// Ask, not Strict. Stock Miniflux `MEDIA_PROXY_MODE` = `http-only`: most
/// images un-proxied, Strict blanks them.
fn default_media_policy() -> i32 {
    crate::settings::MEDIA_ASK
}

/// Hourly, not "Manual only". See `settings::SYNC_INTERVAL_DEFAULT_INDEX`.
fn default_sync_interval_index() -> i32 {
    crate::settings::SYNC_INTERVAL_DEFAULT_INDEX
}

/// After 5 seconds. Account predating setting gets new-install default, not
/// silent "never".
fn default_mark_read_delay_index() -> i32 {
    crate::settings::MARK_READ_DEFAULT_INDEX
}

impl Default for Account {
    fn default() -> Self {
        Account {
            server_url: String::new(),
            token: String::new(),
            use_custom_ca: false,
            media_policy: default_media_policy(),
            sync_interval_index: default_sync_interval_index(),
            wifi_only: false,
            mark_read_delay_index: default_mark_read_delay_index(),
            retention_index: crate::settings::RETENTION_DEFAULT_INDEX,
            notify_new_articles: false,
        }
    }
}

/// Read account file.
///
/// # Errors
///
/// [`vuo_core::Error::Config`] if file unreadable (incl. not existing yet) or
/// not valid account. Message never quotes file contents.
pub fn load_account(path: &std::path::Path) -> vuo_core::Result<Account> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| vuo_core::Error::Config(format!("could not read the account file: {e}")))?;
    serde_json::from_str(&raw)
        // Where and what kind of fault, never serde's message: can quote value;
        // value may be API token.
        .map_err(|e| {
            vuo_core::Error::Config(format!(
                "the account file is malformed ({:?} at line {}, column {})",
                e.classify(),
                e.line(),
                e.column()
            ))
        })
}

/// Write account file with mode 0600.
///
/// Permissions set *before* writing credential: created world-readable then
/// chmod'ed = readable window; enough on shared device.
///
/// `OpenOptions::mode` applies only at CREATION: does nothing for existing file
/// (older build, restored backup, hand-written). Covered by tightening mode
/// explicitly after open; `truncate(true)` already emptied file, so narrowed
/// before anything sensitive goes in.
///
/// # Errors
///
/// [`vuo_core::Error::Config`] if data directory can't be created, file can't
/// open or be restricted to 0600, or account can't be encoded or written.
pub fn save_account(path: &std::path::Path, account: &Account) -> vuo_core::Result<()> {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            vuo_core::Error::Config(format!("could not create the data directory: {e}"))
        })?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let mut file = options
        .open(path)
        .map_err(|e| vuo_core::Error::Config(format!("could not write the account file: {e}")))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // File empty here: narrows before token written.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| {
                vuo_core::Error::Config(format!("could not secure the account file: {e}"))
            })?;
    }

    let json = serde_json::to_vec_pretty(account)
        .map_err(|e| vuo_core::Error::Config(format!("could not encode the account: {e}")))?;
    file.write_all(&json)
        .map_err(|e| vuo_core::Error::Config(format!("could not write the account file: {e}")))?;
    Ok(())
}

/// Transport config for account, incl. CA if configured.
///
/// CA read only for `https` server. Private CA = statement about TLS;
/// plain-`http` instance (over VPN, tunnel = encryption) has no handshake for
/// it. Demanding file anyway lets setting break unrelated config: switch on, no
/// `ca.pem`, `http://` server → "custom CA certificate ... could not be read",
/// naming file user has no reason to have.
///
/// # Errors
///
/// [`vuo_core::Error::Config`] if account uses custom CA with `https` server
/// and certificate unreadable.
pub fn transport_config_for(
    paths: &AppPaths,
    account: &Account,
) -> vuo_core::Result<TransportConfig> {
    let mut config = TransportConfig::default();
    let uses_tls = url::Url::parse(&account.server_url).is_ok_and(|u| u.scheme() == "https");
    if account.use_custom_ca && uses_tls {
        let pem = std::fs::read(&paths.ca_certificate).map_err(|e| {
            // Loud, never silent. Falling back to platform roots = "ignore
            // certificate errors" switch in effect: user believes private CA in
            // use when not.
            vuo_core::Error::Config(format!(
                "the custom CA certificate at {} could not be read: {e}",
                paths.ca_certificate.display()
            ))
        })?;
        config.extra_ca_pem = Some(pem);
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §the API key is not written anywhere but the account file.
    ///
    /// `Command` derives `Debug`; one arm carries whole account. Log line
    /// reaches journald and bug reports: what `{:?}` prints is disclosure
    /// question. `ApiToken` redacts itself; SERVER is `Url`, prints userinfo
    /// (`https://user:pass@host/` pasted into field ends up in log).
    #[test]
    fn a_command_never_prints_the_account_it_carries() {
        let account = WorkerAccount {
            database: PathBuf::from("/home/defaultuser/.local/share/harbour-vuo/vuo.sqlite"),
            server: url::Url::parse("https://alice:hunter2@miniflux.example/").expect("url"),
            token: ApiToken::new("s3cr3t-key"),
            transport: TransportConfig::default(),
        };
        let printed = format!("{:?}", Command::Configure(Box::new(account)));
        // `probe`, not a credential-sounding name: fixtures whose point is NOT
        // escaping; such a variable formatted into assertion message reads to
        // scanner as credential reaching log.
        for probe in [
            "hunter2",
            "alice",
            "miniflux.example",
            "s3cr3t-key",
            "defaultuser",
        ] {
            assert!(
                !printed.contains(probe),
                "{probe:?} survived the redaction: {printed}"
            );
        }
        // Name log line SHOULD carry still there.
        assert_eq!(Command::TestConnection.name(), "TestConnection");
    }

    /// §the worker syncs on its own, on the interval the user chose.
    ///
    /// One process: cadence is worker's. Rule pure, statable without clock:
    /// never for "Manual only"; soon (not at once, list drawn first) for
    /// never-synced or overdue; else remainder.
    #[test]
    fn the_next_sync_is_scheduled_from_the_last_one() {
        let now = 1_000_000;
        let secs = |d: Option<std::time::Duration>| d.map(|d| d.as_secs());
        assert_eq!(next_sync_delay(None, Some(now), now), None, "manual only");
        assert_eq!(
            secs(next_sync_delay(Some(30), None, now)),
            Some(10),
            "never synced: soon, after the list has been drawn"
        );
        assert_eq!(
            secs(next_sync_delay(Some(30), Some(now - 3600), now)),
            Some(10),
            "overdue: soon"
        );
        assert_eq!(
            secs(next_sync_delay(Some(30), Some(now - 10 * 60), now)),
            Some(20 * 60),
            "ten minutes into a half hour: twenty to go"
        );
        assert_eq!(
            secs(next_sync_delay(Some(30), Some(now - 30 * 60 + 5), now)),
            Some(10),
            "five seconds to go still waits the short pause, never less"
        );

        // Stamp round-trips through file worker reads.
        let dir = tempfile::tempdir().expect("tempdir");
        let stamp = dir.path().join("last-sync");
        assert_eq!(read_sync_time(&stamp), None, "no stamp yet");
        record_sync_time(&stamp);
        let recorded = read_sync_time(&stamp).expect("a stamp");
        assert!((chrono_now() - recorded).abs() < 5);
    }

    #[test]
    fn a_missing_custom_ca_fails_loudly_rather_than_falling_back() {
        // §9.1: TLS verification has no toggle; silent fallback to platform
        // roots would be one in effect (user believes private CA in use when
        // not).
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths {
            database: dir.path().join("db.sqlite"),
            account: dir.path().join("account.json"),
            ca_certificate: dir.path().join("absent.pem"),
            last_sync: dir.path().join("last-sync"),
        };
        let account = Account {
            server_url: "https://h.example/".into(),
            token: "t".into(),
            use_custom_ca: true,
            ..Account::default()
        };
        let err = transport_config_for(&paths, &account).expect_err("should refuse");
        assert!(err.to_string().contains("could not be read"), "{err}");
    }

    /// Real self-signed CA: fixture `Transport::new` accepts, so blessed
    /// configuration can actually build client.
    const TEST_CA_PEM: &str = "\
-----BEGIN CERTIFICATE-----\n\
MIIDDzCCAfegAwIBAgIUdRtsISsyGOb74MwXRLNVZV3gzOkwDQYJKoZIhvcNAQEL\n\
BQAwFjEUMBIGA1UEAwwLVnVvIFRlc3QgQ0EwIBcNMjYwODI4MTgyNjU5WhgPMjEy\n\
NjA4MDQxODI2NTlaMBYxFDASBgNVBAMMC1Z1byBUZXN0IENBMIIBIjANBgkqhkiG\n\
9w0BAQEFAAOCAQ8AMIIBCgKCAQEAsw3RlnIknfUGFlQnR2Nz21l9//UnOCboAvZV\n\
iOaXzQYaFjepYSTTdYcrNjuvxh5SmThKr8LyT2RygxkjI/jo6TFT/PeaqD4NkMsH\n\
4lLxRFGHnsBtq8pGUJOYsG9DdRGvLaCQti5spfkNiElD0QxzH6ZPwGWRKJYi1szG\n\
KNrIe6lYInC1tfI7Twxhte1vMTEeITrZR1FnNkV24Fki4dPZeYr3IAHDJCkYzBqQ\n\
Z5MGjwCB1AJ3gB3oeFbgVmy0Lh8mIz7erGEMD8VfOQ1M2gaglymCRy2dpps/OMa9\n\
iy1zXnGT8xdmI0H3DCg8JQR2DBSP7k/KZH+q3WZOky6FZiU1wwIDAQABo1MwUTAd\n\
BgNVHQ4EFgQUDg3uic6vnPmu8XcrbFHI0vZ+HfowHwYDVR0jBBgwFoAUDg3uic6v\n\
nPmu8XcrbFHI0vZ+HfowDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOC\n\
AQEAIAHzIceTVXtzNzAb66/mU3q51jv7luE9P13FbC76NM9+GbUWoQckroKvd3lt\n\
XZ+Diiv3gxfRbgOjMa1SaJcBMyWi5iEkM2/Ljc1zYAPR3RhfhDUdUPukwmcPy74u\n\
gL0usN+U5YUBGaFwvRgvO/9Vrxhgw4o5QI1AwXMq49e0B3S9F502etJUpbCaXfND\n\
1HvfrW3DQouGqouD0RbbTyEjBpsoI2HZKN2irYq81VBhgDX/NKs1lySVYoz9/JPM\n\
B/ICqFxm4tqVyVqqaxdhkS/DJcUPIyEhhwLStjHyLGh364xT06vcDdcRmGyuCSlb\n\
EQBBQIobIy41+aQiMsM0XBYH3Q==\n\
-----END CERTIFICATE-----\n";

    /// §the spinner cannot outlive the command that raised it.
    ///
    /// Refresh timing out on dropped VPN must not leave list and cover spinning
    /// forever. Guard clears because iteration ended; arm forgetting can't
    /// happen.
    #[test]
    fn the_command_guard_clears_the_spinner_however_the_arm_ends() {
        use crate::context::SyncSignal;

        let signal = SyncSignal::default();

        // User-initiated Sync raises flag from Qt thread first.
        signal.set_running(true);
        {
            let _guard = CommandGuard {
                signal: &signal,
                changed: false,
                clears_spinner: true,
                trims_allocator: false,
            };
            // Arm reporting failure, setting nothing.
        }
        assert!(
            !signal.is_running(),
            "an arm that did nothing must still leave the spinner cleared"
        );
        assert_eq!(
            signal.generation(),
            0,
            "and must not move the mirror generation it did not change"
        );

        // Command not owning spinner must not lower one running refresh raised:
        // opportunistic FlushOutbox fired per star tap.
        signal.set_running(true);
        {
            let mut guard = CommandGuard {
                signal: &signal,
                changed: true,
                clears_spinner: false,
                trims_allocator: false,
            };
            guard.changed = true;
        }
        assert!(
            signal.is_running(),
            "a flush must not switch off a refresh's spinner"
        );
        assert_eq!(signal.generation(), 1, "but it did change the mirror");

        // Early `?`-style exit still clears: Drop runs.
        signal.set_running(true);
        fn arm_that_returns_early(signal: &SyncSignal) {
            let _guard = CommandGuard {
                signal,
                changed: false,
                clears_spinner: true,
                trims_allocator: false,
            };
            #[allow(clippy::needless_return)]
            return;
        }
        arm_that_returns_early(&signal);
        assert!(!signal.is_running(), "an early return must still clear");
    }

    #[test]
    fn a_plain_http_server_never_needs_a_ca_certificate() {
        // Switch on, no ca.pem, plain-http instance (over WireGuard, tunnel =
        // encryption) must connect. No handshake on http for CA to apply to:
        // setting can't break unrelated config.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths {
            database: dir.path().join("db.sqlite"),
            account: dir.path().join("account.json"),
            ca_certificate: dir.path().join("absent.pem"),
            last_sync: dir.path().join("last-sync"),
        };
        let account = Account {
            server_url: "http://10.77.0.1:8083/".into(),
            token: "t".into(),
            use_custom_ca: true,
            ..Account::default()
        };
        let config = transport_config_for(&paths, &account)
            .expect("an http server must not require a CA file");
        assert!(config.extra_ca_pem.is_none());

        // Same account over https still fails loudly: §9.1's
        // never-silently-ignore rule is about TLS, holds there.
        let https = Account {
            server_url: "https://10.77.0.1:8083/".into(),
            ..account
        };
        transport_config_for(&paths, &https).unwrap_err();
    }

    #[test]
    fn a_configured_ca_reaches_the_transport() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, TEST_CA_PEM).expect("write");
        let paths = AppPaths {
            database: dir.path().join("db.sqlite"),
            account: dir.path().join("account.json"),
            ca_certificate: ca,
            last_sync: dir.path().join("last-sync"),
        };
        let account = Account {
            server_url: "https://h.example/".into(),
            token: "t".into(),
            use_custom_ca: true,
            ..Account::default()
        };
        let config = transport_config_for(&paths, &account).expect("config");
        assert!(
            config.extra_ca_pem.is_some(),
            "the CA setting must actually reach the client"
        );

        // Config must build a client. Stopping at struct field blesses config
        // `Transport::new` refuses.
        Transport::new(
            url::Url::parse("https://h.example/").expect("url"),
            ApiToken::new("t"),
            &config,
        )
        .expect("a configured CA must produce a usable transport");

        // Off by default.
        let off = Account {
            use_custom_ca: false,
            ..account
        };
        assert!(transport_config_for(&paths, &off)
            .expect("config")
            .extra_ca_pem
            .is_none());
    }

    #[test]
    fn the_account_file_is_not_world_readable() {
        // §7 relies on filesystem permissions plus Sailfish home encryption:
        // permissions must actually be right.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account.json");
        save_account(
            &path,
            &Account {
                server_url: "https://h.example/".into(),
                token: "secret".into(),
                use_custom_ca: false,
                ..Account::default()
            },
        )
        .expect("write");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "the API key must not be readable by other users"
            );
        }

        let read_back = load_account(&path).expect("read");
        assert_eq!(read_back.token, "secret");
    }

    #[test]
    #[cfg(unix)]
    fn overwriting_an_existing_account_file_still_secures_it() {
        // `OpenOptions::mode` applies only at CREATION: does nothing for
        // existing file (older build, restored backup, hand-written). Overwrite
        // must still leave API key owner-only.
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account.json");
        std::fs::write(&path, "{}").expect("pre-create");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        save_account(
            &path,
            &Account {
                server_url: "https://h.example/".into(),
                token: "secret".into(),
                use_custom_ca: false,
                ..Account::default()
            },
        )
        .expect("write");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "overwriting an existing account file must tighten its permissions, \
             not inherit whatever was there"
        );
    }

    #[test]
    fn a_malformed_account_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account.json");
        std::fs::write(&path, "not json at all").expect("write");
        load_account(&path).unwrap_err();
    }

    #[test]
    fn an_unconfigured_device_reports_no_paths_rather_than_failing() {
        // Timer firing before first run does nothing, quietly.
        //
        // Through `AppPaths::configured` (decision `from_env` returns through).
        // Asserting `!path.exists()` on literal would test `std::path::Path`,
        // pass with `configured` replaced by `Some(self)`.
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("harbour-vuo");
        std::fs::create_dir_all(&base).expect("mkdir");

        assert!(
            AppPaths::under(&base).configured().is_none(),
            "with no account file a background sync must not start"
        );

        std::fs::write(base.join("account.json"), "{}").expect("write");
        let paths = AppPaths::under(&base)
            .configured()
            .expect("an account file means configured");
        assert_eq!(paths.database, base.join("vuo.sqlite"));
        assert_eq!(paths.ca_certificate, base.join("ca.pem"));
    }

    /// §a flush the UI asks for arms the debounce rather than sending.
    ///
    /// Window = MAXIMUM AGE, not idle timeout: difference between "session
    /// costs one request" and "steady tapper never flushed". Rule loop follows:
    /// request finding deadline set leaves it.
    #[test]
    fn a_second_flush_request_does_not_push_the_deadline_back() {
        let started = std::time::Instant::now();
        let mut flush_due: Option<std::time::Instant> = None;

        // First tap arms window.
        if flush_due.is_none() {
            flush_due = Some(started + FLUSH_DEBOUNCE);
        }
        let armed = flush_due.expect("the first request arms the window");

        // Tap half a window later mustn't move it.
        let later = started + FLUSH_DEBOUNCE / 2;
        if flush_due.is_none() {
            flush_due = Some(later + FLUSH_DEBOUNCE);
        }

        assert_eq!(
            flush_due.expect("still armed"),
            armed,
            "re-arming on each request would make this an idle timeout, and a \
             reader marking an article every few seconds would never be flushed"
        );
    }

    /// §both deadlines due at once runs the sync, not the flush.
    ///
    /// Pass replays outbox first; separate flush moment earlier = request for
    /// work sync was about to do (exact cost this avoids).
    #[test]
    fn a_sync_subsumes_a_flush_that_falls_due_with_it() {
        let now = std::time::Instant::now();
        let past = now.checked_sub(std::time::Duration::from_secs(1)).unwrap();

        assert_eq!(
            due_work(Some(past), Some(past), now),
            Some(DueWork::Sync),
            "a sync replays the outbox, so it takes precedence over a flush"
        );
        assert_eq!(due_work(None, Some(past), now), Some(DueWork::Flush));
        assert_eq!(due_work(Some(past), None, now), Some(DueWork::Sync));
    }

    /// §a deadline still in the future is not work.
    #[test]
    fn nothing_is_due_before_its_deadline() {
        let now = std::time::Instant::now();
        let soon = now + std::time::Duration::from_secs(30);

        assert_eq!(due_work(Some(soon), Some(soon), now), None);
        assert_eq!(due_work(None, None, now), None);
    }

    /// §the wait ends on whichever deadline comes first.
    ///
    /// Waiting on sync alone holds reader's marks until next interval (up to an
    /// hour).
    #[test]
    fn the_wait_ends_on_the_nearer_of_the_two_deadlines() {
        let now = std::time::Instant::now();
        let sync_at = now + std::time::Duration::from_secs(3600);
        let flush_at = now + FLUSH_DEBOUNCE;

        assert_eq!(earliest(Some(sync_at), Some(flush_at)), Some(flush_at));
        assert_eq!(earliest(Some(flush_at), Some(sync_at)), Some(flush_at));
        assert_eq!(earliest(None, Some(flush_at)), Some(flush_at));
        assert_eq!(earliest(Some(sync_at), None), Some(sync_at));
        assert_eq!(earliest(None, None), None);
    }

    /// §the debounce is short enough to be invisible and long enough to batch.
    ///
    /// Both ends matter. Too long: mark here shows unread on other device. Too
    /// short: back to one request per tap.
    #[test]
    fn the_flush_window_stays_within_a_reading_session() {
        assert!(
            FLUSH_DEBOUNCE >= std::time::Duration::from_secs(20),
            "shorter than this and a normal reading pace outruns the window"
        );
        assert!(
            FLUSH_DEBOUNCE <= std::time::Duration::from_secs(120),
            "longer than this and the server is visibly behind the phone"
        );
        assert!(
            FLUSH_AFTER_START < FLUSH_DEBOUNCE,
            "an outbox carried over from last time has already waited; it \
             should not wait a whole window again"
        );
    }

    /// §only the worker's OWN work consults the network.
    ///
    /// "Only sync on Wi-Fi" = what Vuo does unprompted. Pulled refresh,
    /// subscription, connection test = reader's decision; phone on cellular
    /// still does them. Structural, not per-arm flag: probe happens where
    /// DEADLINE resolves, every arriving command runs below. Asserts structure.
    #[test]
    fn only_automatic_work_consults_the_network() {
        const SOURCE: &str = include_str!("worker.rs");
        // Test's own mention of call is in this string: count from above it,
        // everything up to test module.
        let production = SOURCE
            .split_once("#[cfg(test)]")
            .map_or(SOURCE, |(before, _)| before);

        assert_eq!(
            production.matches("net::probe()").count(),
            1,
            "the network is read in exactly one place. A second reading is \
             either a command path that should not be gated at all, or two \
             answers for one decision."
        );

        let probe_at = production
            .find("net::probe()")
            .unwrap_or_else(|| panic!("the network is not read at all"));
        let handles_commands_at = production
            .find("match command {")
            .unwrap_or_else(|| panic!("the worker no longer dispatches on a command"));
        assert!(
            probe_at < handles_commands_at,
            "the network is read while a command is being HANDLED. Everything \
             below `match command` includes the commands the user sent, and \
             gating those would mean a pulled refresh doing nothing on a \
             mobile connection, with no way for the reader to tell why."
        );
    }

    /// §a sync refused for the network is rescheduled, not dropped.
    ///
    /// Leaving `next_sync` makes every wait expire at once; loop spins against
    /// `/proc` while phone on cellular.
    #[test]
    fn a_refused_sync_goes_back_on_the_interval() {
        let now = std::time::Instant::now();

        let next = next_interval_after(Some(60), now).expect("an hourly account reschedules");
        assert_eq!(next - now, std::time::Duration::from_secs(3600));

        assert_eq!(
            next_interval_after(None, now),
            None,
            "Manual only has no next sync to go back on"
        );
        assert!(
            next_interval_after(Some(0), now).is_some_and(|at| at > now),
            "a stored zero must not resolve to a deadline that is already past"
        );
    }
}
