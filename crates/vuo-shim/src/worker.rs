//! The background sync worker.
//!
//! # Why a worker thread rather than futures on the Qt event loop
//!
//! `qmetaobject` does expose `execute_async`, which polls a future from Qt's
//! event loop, and at first glance that is exactly what §5's "async runtime
//! driven from the Qt event loop" describes. It does not work for Vuo's sync,
//! and the reason is worth recording so nobody re-tries it.
//!
//! `reqwest`'s async client needs a Tokio *reactor* — the IO driver that wakes
//! its futures on socket readiness. Polling a reqwest future from Qt's event
//! loop, with no Tokio runtime entered, panics at the first socket
//! registration. There is no reactor to register with.
//!
//! So the network and database work runs on a dedicated thread with its own
//! current-thread Tokio runtime, and nothing it produces touches a `QObject`
//! directly. That is the rule that actually matters for correctness, and it is
//! kept structurally rather than by marshalling: results reach the UI through
//! [`crate::context::SyncSignal`], which the Qt thread polls, so every
//! `QObject` touch already happens on the thread that owns it.
//!
//! `on_event` is therefore a log sink and nothing more. It is deliberately not
//! wrapped in `queued_callback` — that would tie building a worker to a live Qt
//! event loop, which start-up has and the settings screen, rebuilding one after
//! an account is saved, cannot promise.
//!
//! The division of labour follows §5's "models observe SQLite": the worker
//! writes to the mirror, then signals; the models re-read the mirror on the Qt
//! thread. Sync results are never passed through the channel as data.

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

/// How long a mark-read or a star waits before the worker sends it.
///
/// The outbox already batches WITHIN a flush -- `replay::flush` groups pending
/// intents by desired value, so a thousand marks leave as a handful of
/// requests. What it did not do was batch the flushes themselves: the UI fires
/// [`Command::FlushOutbox`] from every tap, and the worker acted on each one,
/// so a reading session's thirty marks were thirty separate requests however
/// well each was batched internally.
///
/// On a phone that is the expensive part. A request is not just its bytes: it
/// re-arms the radio's connected state and, on cellular, holds it there for
/// the tail timer afterwards. Thirty of them spread over a few minutes keeps
/// the modem out of idle for most of that time.
///
/// The window is a MAXIMUM AGE, not an idle timeout: the first request arms
/// it and later ones do not push it back, so an intent is never delayed by
/// more than this however steadily the reader taps. Forty-five seconds
/// collapses a normal reading session into one or two requests while staying
/// short enough that a reader who marks something and immediately checks
/// another device sees it there.
///
/// Nothing is lost if the window does not elapse. The intent is already in the
/// outbox, which is durable and idempotent and survives the process being
/// killed; [`FLUSH_AFTER_START`] is what picks it up next time.
pub const FLUSH_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(45);

/// How soon after an account is configured an outbox left over from last time
/// is sent.
///
/// The debounce means a reader who closes Vuo mid-session leaves intents
/// behind. Waiting for the next interval to carry them would be up to an hour,
/// so start-up looks at the outbox and arms a flush if there is anything in
/// it. One request per launch, and only when there is something to send.
///
/// Deliberately not a flush on [`Command::Shutdown`]: `Worker::drop` sends
/// that and then JOINS, on whatever thread is dropping the worker -- the Qt
/// thread, when the settings screen rebuilds the context. A network request
/// there would freeze the UI for as long as the phone's signal took, up to the
/// transport's whole 120-second budget.
pub const FLUSH_AFTER_START: std::time::Duration = std::time::Duration::from_secs(10);

/// What the worker does when its wait ends with no command having arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueWork {
    Sync,
    Flush,
}

/// The nearer of two deadlines, if there is one.
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

/// The next interval sync after `now`, or `None` for "Manual only".
///
/// The floor of one minute is not a policy: `interval` comes from a stored
/// index and a corrupt or hand-edited account file could hold zero, which
/// would make the worker sync in a tight loop.
#[must_use]
pub fn next_interval_after(
    interval_minutes: Option<i64>,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    interval_minutes
        .map(|minutes| now + std::time::Duration::from_secs(minutes.max(1).unsigned_abs() * 60))
}

/// Which piece of scheduled work has fallen due at `now`.
///
/// A sync replays the outbox as its first step, so when both deadlines have
/// passed the sync is the one to run and the waiting flush is subsumed by it
/// rather than being sent separately a moment earlier.
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

/// Everything the worker needs in order to serve one account.
///
/// Handed over by [`Command::Configure`] rather than captured when the thread
/// starts, because the thread now starts before there is an account to capture.
pub struct WorkerAccount {
    /// The mirror. The worker opens its own connection to it; the Qt thread
    /// has a second one, and WAL is what makes that safe.
    pub database: PathBuf,
    pub server: url::Url,
    pub token: ApiToken,
    pub transport: TransportConfig,
}

impl std::fmt::Debug for WorkerAccount {
    /// Opaque, for the reason [`ApiToken`] is.
    ///
    /// The token redacts itself, but nothing else here does, and a DERIVED
    /// `Debug` on this struct puts all of it in any log line that formats a
    /// `Command`. The server is a `Url` and prints whatever userinfo it
    /// carries -- `https://user:pass@host/` is a thing people paste into an
    /// address field, and §9.1 keeps it out of the settings screen's error
    /// text for exactly that reason. A log line is read by more people than an
    /// error label: it goes to journald, and into bug reports. The mirror's
    /// path is under the user's home directory, and the transport config
    /// carries a whole CA certificate.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerAccount { .. }")
    }
}

/// What the UI can ask the worker to do.
///
/// Only operations that genuinely need the network are here. Local mutations
/// (mark read, star) are applied inline on the Qt thread instead -- see
/// [`apply_local_status`] -- because routing a fast local transaction through
/// a channel would put a thread hop between the user's tap and the UI
/// updating, for no benefit.
#[derive(Debug)]
pub enum Command {
    /// Run a full sync pass.
    Sync,
    /// Ask for the outbox to be flushed, without pulling.
    ///
    /// A REQUEST, not an order. Every mark-read and every star sends one, and
    /// each used to be its own HTTPS round trip: a reader working through
    /// thirty articles made thirty of them, and on a cellular connection each
    /// one re-arms the radio's connected state and its tail timer. The worker
    /// coalesces them instead -- see [`FLUSH_DEBOUNCE`].
    FlushOutbox,
    /// Subscribe to a feed. The *server* discovers and fetches it; §3 makes
    /// local feed fetching the project's most important boundary.
    Subscribe {
        feed_url: String,
    },
    Unsubscribe {
        feed_id: i64,
    },
    /// Change a feed's settings on the server, then mirror the result.
    UpdateFeed {
        feed_id: i64,
        update: vuo_core::api::client::FeedPatch,
    },
    /// Ask the server to scrape the original article (§3: use the server's
    /// endpoint, never a local Readability port).
    FetchOriginal {
        entry_id: i64,
    },
    /// Verify the configured credentials, for the settings screen.
    TestConnection,
    /// Serve this account from now on, in place of whatever came before.
    ///
    /// Boxed because it is much the largest thing a `Command` can carry, and
    /// every other variant would otherwise be padded out to its size in the
    /// channel.
    Configure(Box<WorkerAccount>),
    /// How often the worker is to sync on its own, in minutes; `None` for
    /// "Manual only". Sent when the context is built and whenever Settings
    /// is saved. The worker schedules its next sync from the last one it
    /// knows of (the stamp beside the mirror), so a restart does not sync
    /// at once if the mirror is fresh.
    SetSyncInterval {
        minutes: Option<i64>,
    },
    /// How long read, unfavourited articles are kept in the mirror, in
    /// seconds; `None` for "keep everything". Sent alongside
    /// [`Command::SetSyncInterval`] and applied at the end of the next pass.
    SetRetention {
        seconds: Option<i64>,
    },
    /// Whether the user has asked that Vuo sync only over Wi-Fi.
    ///
    /// Only the AUTOMATIC work consults it -- the interval sync and the
    /// coalesced outbox flush. Anything the user started themselves goes out
    /// whatever the network: a refresh they pulled for, a feed they
    /// subscribed to, a connection they asked to test. The setting is about
    /// what Vuo does on its own.
    SetWifiOnly {
        enabled: bool,
    },
    Shutdown,
}

impl Command {
    /// A fixed name, for logs.
    ///
    /// Never `{:?}` a `Command` into a log line. `Configure` carries the
    /// account, and while `WorkerAccount` is opaque above, a variant added
    /// later would not be -- and the thing being logged here is which command
    /// arrived, which is a word, not a payload.
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
    SyncStarted,
    SyncFinished {
        unread: i64,
        changed: bool,
    },
    /// Already-redacted, user-presentable text. Rendered as plain text (§9.3).
    SyncFailed {
        message: String,
    },
    /// The API key was rejected; the UI should send the user to settings.
    AuthFailed,
    /// A subscribe or unsubscribe finished. `message` is empty on success.
    SubscriptionChanged {
        ok: bool,
        message: String,
    },
    /// A feed's settings were changed. `message` is empty on success.
    FeedUpdated {
        feed_id: i64,
        ok: bool,
        message: String,
    },
    /// The server returned scraped content for an entry.
    OriginalContentFetched {
        entry_id: i64,
        ok: bool,
    },
    /// Result of a settings-screen connection test.
    ConnectionTested {
        ok: bool,
        message: String,
    },
}

/// Clears the sync spinner when a command's iteration ends, however it ends.
///
/// Deliberately a `Drop` impl and not a call. Clearing used to be one more
/// thing each arm had to remember, and two arms did not: a refresh that failed
/// -- the timeout on a dropped VPN, say -- left `running` true for the life of
/// the process, so the entry list and the cover both span forever and the
/// "Nothing to read" placeholder stayed suppressed behind them. An arm added
/// later would have had to remember too. This way the flag is cleared because
/// the iteration ended, which is not something a future edit can forget.
struct CommandGuard<'a> {
    signal: &'a crate::context::SyncSignal,
    /// Set by an arm that actually wrote to the mirror.
    ///
    /// A bump makes every model `reload()`, which is a full reset on a plain
    /// `ListView` and therefore scrolls the list back to the top. Bumping for
    /// a command that changed nothing would move the page under the reader for
    /// no reason, so this stays opt-in.
    changed: bool,
    /// Whether this command owns the spinner.
    ///
    /// Only a user-initiated `Sync` raises it (`EntryModel::requestSync`), so
    /// only that command may lower it. Decided from the command itself at
    /// construction, so an opportunistic `FlushOutbox` fired by a star tap
    /// physically cannot switch off the spinner of a refresh already running.
    clears_spinner: bool,
    /// Whether this command is big enough to be worth returning its peak to
    /// the kernel. See [`crate::memory`].
    ///
    /// Decided from the command for the same reason `clears_spinner` is, and
    /// true for the two that inflate, parse and then drop a whole corpus or a
    /// whole article body. Not for the small ones: a star tap sends
    /// `FlushOutbox`, and walking every arena on every tap would buy nothing
    /// and cost the one thread that must never make the UI wait.
    trims_allocator: bool,
}

impl Drop for CommandGuard<'_> {
    fn drop(&mut self) {
        // Before the signal, not after. A bump sets every model reloading on
        // the Qt thread, and the trim takes each arena's lock as it goes --
        // so doing it first is the difference between the UI waiting on the
        // allocator and the allocator being done before the UI asks.
        if self.trims_allocator {
            crate::memory::release_free_memory();
        }
        // Clear BEFORE bumping, and the order is load-bearing. `pollSync`
        // spends a generation the first time it sees it, so a poll landing
        // between a bump and a clear would read `running` as still true and
        // could never revisit that generation -- the spinner would survive its
        // own clear.
        if self.clears_spinner {
            self.signal.set_running(false);
        }
        if self.changed {
            self.signal.bump();
        }
    }
}

/// A handle to the worker thread.
/// Write an accepted feed update into the mirror.
///
/// Only the fields the update actually carried: a `None` here means the user
/// did not touch that setting, and writing a default over it would revert a
/// value set from the web UI.
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
        // The same placeholder trick `upsert_feed` uses: the categories
        // listing is fetched separately, so a category chosen here may not be
        // in the mirror yet and the foreign key would reject the update.
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
    /// The server answered, but with nothing. Miniflux returns 200 with an
    /// empty `content` when its scraper rules match nothing on the page --
    /// paywalls and JS-rendered articles do this routinely.
    Empty,
    /// Byte-for-byte what is already stored, so there is nothing to show for
    /// the tap. Distinguished from `Empty` because it is not a failure: the
    /// feed already carried the full article.
    Unchanged,
}

/// Decide whether a scraped body should replace the stored one.
///
/// Pure, and separated from the command handler, because the interesting cases
/// are all about *what the server returned* and none of them need a database,
/// a runtime or a network to exercise.
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
    /// Start the worker thread, with no account yet.
    ///
    /// **Called once, at start-up, before the UI exists** -- see
    /// [`crate::context::start_worker`]. It used to be called from wherever an
    /// account first appeared, which on a first run is a QML tap, and creating
    /// a thread there killed the process outright on a Jolla Phone 2026: the
    /// last thing logged was the line before `thread::Builder::spawn`, and
    /// neither the parent's next line nor the thread's first one ever
    /// arrived. The same call from `main` works on every launch. So the thread
    /// is created while the process is still small and single-purpose, and an
    /// account reaches it afterwards as [`Command::Configure`] -- which is a
    /// channel send, and cannot fail that way.
    ///
    /// It also removes the churn that went with rebuilding: changing the
    /// server used to retire one thread and start another, leaving the retired
    /// one detached and mid-request. One thread now serves each account in
    /// turn.
    ///
    /// `on_event` is invoked **on the worker thread**. It must not touch a
    /// `QObject`: results the UI has to see travel by
    /// [`crate::context::SyncSignal`] instead, which the Qt thread polls. A
    /// caller that genuinely needs to reach a `QObject` from here has to wrap
    /// it with `qmetaobject::queued_callback` first, and then owes the event
    /// loop that primitive requires.
    pub fn spawn(
        signal: std::sync::Arc<crate::context::SyncSignal>,
        on_event: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Command>();

        let handle = thread::Builder::new()
            .name("vuo-sync".to_owned())
            .spawn(move || {
                // First statement in the thread, and the parent logs one the
                // instant `spawn` returns. Which of the two arrives -- or
                // neither -- is what says whether a death here belongs to the
                // Qt thread or to this one; nothing else distinguishes them
                // once the process is gone. See docs/testing.md.
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

                // The account being served, or `None` until one is given.
                //
                // A fresh install reaches this point with nothing configured
                // at all, and that is an ordinary state, not a failure: the
                // thread waits here rather than exiting, so that the account
                // the user is about to type does not need a thread created for
                // it. `last_sync` sits beside the mirror, as `AppPaths::under`
                // lays it out.
                let mut db: Option<Database> = None;
                let mut client: Option<MinifluxClient> = None;
                let mut last_sync = PathBuf::new();

                // The worker's own sync, on the interval the user chose.
                //
                // Vuo is one process -- Harbour allows no other -- so there
                // is no timer outside it, and the worker keeps the cadence
                // itself: it waits for a command only until the next sync is
                // due, and then runs one as if `Command::Sync` had arrived.
                // Manual and automatic syncs are the same code; the only
                // difference is who asked.
                let mut interval: Option<i64> = None;
                let mut next_sync: Option<std::time::Instant> = None;
                // The retention window, or `None` for "keep everything" --
                // which is what an install that has never opened Settings
                // means, and what every version before this one did.
                let mut retention: Option<i64> = None;
                // When the coalesced outbox flush falls due, or `None` while
                // nothing is waiting to be sent. See [`FLUSH_DEBOUNCE`].
                let mut flush_due: Option<std::time::Instant> = None;
                // The user's "Only sync on Wi-Fi". Consulted against the
                // network read at the moment a piece of AUTOMATIC work falls
                // due, never against one pushed from the Qt thread -- the
                // whole window this matters in is the one where the app is
                // minimised and nothing is pushing anything. See
                // `vuo_core::net`.
                let mut wifi_only = false;
                loop {
                    // Only an account being served has work that can fall
                    // due. Without this filter, an interval set before an
                    // account arrives makes every wait expire immediately and
                    // the loop spins.
                    let due = earliest(next_sync, flush_due).filter(|_| client.is_some());
                    // Set only when the command below was produced by a
                    // deadline rather than sent by the UI. A flush the UI
                    // ASKED for arms the debounce; a flush the debounce
                    // produced is the one that actually runs.
                    let mut flush_fell_due = false;
                    let command = match due {
                        Some(at) => {
                            let wait = at.saturating_duration_since(std::time::Instant::now());
                            match rx.recv_timeout(wait) {
                                Ok(command) => command,
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    let now = std::time::Instant::now();
                                    let work = due_work(next_sync, flush_due, now);
                                    // Read HERE, once, for whichever piece of
                                    // work fell due. Not at start-up and not
                                    // on a tap: the reader walks out of Wi-Fi
                                    // range with the app on its cover, and a
                                    // reading taken any earlier than this is
                                    // a reading of a network that has gone.
                                    if !vuo_core::net::probe().allows(wifi_only) {
                                        tracing::info!(
                                            work = ?work,
                                            "the network does not allow automatic work; \
                                             leaving it for the next one"
                                        );
                                        // The flush simply waits: a later tap
                                        // re-arms it, and a sync carries the
                                        // outbox anyway. The sync goes back on
                                        // its own interval, which is also when
                                        // the network is looked at again -- no
                                        // polling, and no wakeup that was not
                                        // already going to happen.
                                        flush_due = None;
                                        if matches!(work, Some(DueWork::Sync)) {
                                            next_sync = next_interval_after(interval, now);
                                        }
                                        continue;
                                    }
                                    match work {
                                        Some(DueWork::Flush) => {
                                            flush_due = None;
                                            flush_fell_due = true;
                                            Command::FlushOutbox
                                        }
                                        // `None` cannot happen -- the wait
                                        // ended because one of the two
                                        // deadlines passed -- but a sync is
                                        // the safe reading of it either way,
                                        // and it is what the loop did before
                                        // there was a second deadline.
                                        _ => {
                                            // As `requestSync` does before
                                            // sending, so the cover and the
                                            // list show it.
                                            signal.set_running(true);
                                            Command::Sync
                                        }
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

                    // A flush the UI asked for only arms the window; the
                    // deadline above is what sends. Re-arming on each request
                    // would be an idle timeout, and a reader tapping steadily
                    // would then never reach the end of one -- so the first
                    // request sets the deadline and later ones find it
                    // already set and leave it alone.
                    if matches!(command, Command::FlushOutbox) && !flush_fell_due {
                        if flush_due.is_none() {
                            flush_due = Some(std::time::Instant::now() + FLUSH_DEBOUNCE);
                        }
                        continue;
                    }
                    // Shutdown is handled above the guard: there is no spinner
                    // to clear for it, and draining whatever is queued behind
                    // it matters more. A `Sync` sitting in the queue when the
                    // worker is told to stop would otherwise leave `running`
                    // set with nothing left alive to clear it.
                    if matches!(command, Command::Shutdown) {
                        while rx.try_recv().is_ok() {}
                        signal.set_running(false);
                        break;
                    }

                    // The two commands that do not need an account, taken
                    // before the guard because neither touches the mirror and
                    // neither owns the spinner. Both `continue`, so the value
                    // they move out of `command` is not wanted below.
                    match command {
                        Command::Configure(account) => {
                            last_sync = account.database.with_file_name("last-sync");
                            // Both of these are reported and then left as
                            // `None`: an account that cannot be opened or
                            // whose transport will not build is a dead one,
                            // and serving the PREVIOUS account's data under
                            // the new account's name would be worse than
                            // serving nothing.
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
                            // The stamp belongs to the account, so the first
                            // sync of a new one is scheduled from ITS history.
                            next_sync =
                                next_sync_delay(interval, read_sync_time(&last_sync), chrono_now())
                                    .map(|delay| std::time::Instant::now() + delay);
                            // Anything the last session left unsent goes
                            // shortly after start-up rather than waiting for
                            // the next interval, which may be an hour away.
                            // See [`FLUSH_AFTER_START`].
                            flush_due = db
                                .as_ref()
                                .and_then(|db| outbox::len(db.conn()).ok())
                                .filter(|pending| *pending > 0)
                                .map(|_| std::time::Instant::now() + FLUSH_AFTER_START);
                            continue;
                        }
                        Command::SetSyncInterval { minutes } => {
                            interval = minutes;
                            next_sync =
                                next_sync_delay(interval, read_sync_time(&last_sync), chrono_now())
                                    .map(|delay| std::time::Instant::now() + delay);
                            continue;
                        }
                        Command::SetRetention { seconds } => {
                            // Recorded, not acted on. Pruning here would
                            // delete the reader's articles the instant they
                            // tapped Save, in front of them; at the end of a
                            // pass it happens with everything else the sync
                            // changed, once.
                            retention = seconds;
                            continue;
                        }
                        Command::SetWifiOnly { enabled } => {
                            wifi_only = enabled;
                            continue;
                        }
                        _ => {}
                    }

                    // Everything else needs an account. Before there is one
                    // there is nothing to answer with, and dropping the
                    // command is right: the UI cannot send one until a context
                    // exists, and a context is only built once an account has
                    // been handed over.
                    let (Some(db), Some(client)) = (db.as_mut(), client.as_ref()) else {
                        tracing::warn!(
                            command = command.name(),
                            "no account is configured; dropping the command"
                        );
                        continue;
                    };

                    // See CommandGuard: clearing the spinner is structural, so
                    // an arm that returns early -- or one added later -- cannot
                    // leave it spinning. Arms opt into the bump.
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
                        // All three are handled above, before the guard.
                        Command::Shutdown
                        | Command::Configure(_)
                        | Command::SetSyncInterval { .. }
                        | Command::SetRetention { .. }
                        | Command::SetWifiOnly { .. } => {}
                        Command::Sync => {
                            // A pass replays the outbox as its first step, so
                            // whatever the debounce was holding goes with it
                            // and must not be sent again a moment later.
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
                                    // So the next start schedules from this
                                    // sync rather than syncing at once.
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
                                    // `sync` commits incrementally, so a run
                                    // that failed late may still have written.
                                    guard.changed = true;
                                    signal.post(Notice::SyncFailed {
                                        auth: true,
                                        message: String::new(),
                                    });
                                    on_event(Event::AuthFailed);
                                }
                                Err(e) => {
                                    guard.changed = true;
                                    // Already redacted: Error's Display never
                                    // carries a token or userinfo. Still
                                    // foreign text, so the page renders it as
                                    // plain text.
                                    let message = e.to_string();
                                    signal.post(Notice::SyncFailed {
                                        auth: false,
                                        message: message.clone(),
                                    });
                                    on_event(Event::SyncFailed { message });
                                }
                            }
                            // Whatever it did, the next one is an interval
                            // away: a server that is down is not asked again
                            // every few seconds.
                            next_sync = next_interval_after(interval, std::time::Instant::now());
                        }
                        Command::Subscribe { feed_url } => {
                            let result = runtime.block_on(client.create_feed(&feed_url, None));
                            match result {
                                Ok(_) => {
                                    // Pull immediately so the new feed's entries
                                    // appear without waiting for the next timer.
                                    let _ = runtime.block_on(sync::sync(
                                        db,
                                        client,
                                        SyncOptions {
                                            retention_secs: retention,
                                            ..SyncOptions::default()
                                        },
                                    ));
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
                                    // This DID change the mirror. Without the
                                    // bump the generation never moved, so
                                    // `FeedModel::pollSync` reported nothing
                                    // and the deleted feed's row stayed in the
                                    // list until some later sync.
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
                            // The opposite order would show the new name at
                            // once and then have to take it back when the PUT
                            // failed -- and a feed that silently reverted its
                            // own name a second after being renamed is worse
                            // than one that took a moment to change it. There
                            // is no outbox row for this: unlike a read mark,
                            // a rename is not something the user does dozens
                            // of times offline, and replaying one has no
                            // conflict story worth the machinery.
                            match runtime.block_on(client.update_feed(feed_id, &update)) {
                                Ok(()) => {
                                    // Patch the mirror rather than re-fetching
                                    // the feed list: the fields are exactly
                                    // the ones just accepted, and a full pull
                                    // for one rename is a second round trip
                                    // the user is waiting on.
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
                                            // Store the scraped body against the
                                            // entry so it survives a restart and
                                            // stays readable offline.
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
                                            // Same as Unsubscribe: the scraped
                                            // body is in SQLite, so the open
                                            // article is stale until something
                                            // reloads it.
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
                                            // Nothing worth storing. Writing it
                                            // anyway is how "fetch original"
                                            // used to ERASE a perfectly good
                                            // article: a server that scrapes a
                                            // paywall or a JS-only page answers
                                            // 200 with an empty body, and that
                                            // empty body went straight over the
                                            // feed's own content.
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
                                    // Addressed to the article that asked, NOT
                                    // posted as a `SyncFailed` notice: that put
                                    // "Refresh failed" across the top of the
                                    // ENTRY LIST for a scrape the user started
                                    // from inside an article, blaming a refresh
                                    // that never ran.
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
                                    // The username is the user's own, from
                                    // their own server, but it is still
                                    // rendered as plain text.
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
                            // Belt and braces: the deadline is cleared where
                            // it fires, and this arm is also reachable from a
                            // command sent before there was an account to
                            // serve it.
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
                                    // A flush that confirmed or dropped rows
                                    // changed the mirror -- `flush` deletes
                                    // confirmed outbox rows and discards ones
                                    // the server refused for good. Neither was
                                    // ever reported, so `pendingActions` went
                                    // stale and a dropped intent vanished in
                                    // silence.
                                    guard.changed = outcome.confirmed > 0 || outcome.dropped > 0;
                                    let unread = store::unread_count(db.conn()).unwrap_or(0);
                                    on_event(Event::SyncFinished {
                                        unread,
                                        changed: guard.changed,
                                    });
                                }
                                Err(e) => {
                                    // Only a PERMANENT failure is worth a
                                    // notice. This command is fired from every
                                    // star and every mark-read, so reporting a
                                    // routine offline flush would put an error
                                    // on screen for each tap -- while the
                                    // outbox is doing exactly what it exists
                                    // for and will replay on the next sync.
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
        // A failed spawn is `None` and no panic, so say which happened: a
        // worker that was never created behaves exactly like one that died.
        if handle.is_some() {
            tracing::info!("the sync worker thread is spawned");
        } else {
            tracing::warn!("the sync worker thread could not be created");
        }

        Worker { tx, handle }
    }

    /// Ask the worker to do something. Returns `false` if it has stopped.
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
        let _ = self.tx.send(Command::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Apply a local mutation immediately, on the calling (Qt) thread.
///
/// Deliberately synchronous and deliberately not routed through the worker.
/// The write is a fast local transaction, and doing it inline is what lets the
/// UI update in the same frame as the tap. The server hears about it on the
/// next flush; that is the whole point of the outbox.
pub fn apply_local_status(
    db: &mut Database,
    id: EntryId,
    status: EntryStatus,
) -> vuo_core::Result<()> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue(tx, id, DesiredValue::Status(status), now))
}

pub fn apply_local_starred(db: &mut Database, id: EntryId, starred: bool) -> vuo_core::Result<()> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue(tx, id, DesiredValue::Starred(starred), now))
}

/// Apply the same status to many entries at once, in one transaction.
///
/// Used by "mark all read" in a scope that has no server-side equivalent
/// (unread, starred, all), where the intent has to be expanded over the
/// concrete entries the user is actually looking at.
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

pub fn apply_local_mark_feed_read(db: &mut Database, feed_id: i64) -> vuo_core::Result<usize> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue_mark_feed_read(tx, feed_id, now))
}

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
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Where Vuo keeps its data and credentials on a device.
///
/// §7: *the API key is stored under the app's data directory with restrictive
/// permissions, relying on Sailfish's home encryption. No custom keyring, no
/// SQLCipher, unless a concrete threat model justifies it.*
///
/// The token deliberately does **not** live in the SQLite mirror. The mirror is
/// a cache that can be deleted, copied for debugging, or handed to a developer
/// with a bug report; a credential in it would travel with all of that.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub database: PathBuf,
    pub account: PathBuf,
    /// Where a user-supplied CA certificate is expected, for a self-hosted
    /// instance with a private certificate authority (§9.1). A fixed path
    /// rather than a file picker: it is a rare, deliberate act, and a path the
    /// user chose would be one more thing to validate.
    pub ca_certificate: PathBuf,
    /// When the mirror was last synced, as Unix seconds in a file.
    ///
    /// Written after every successful sync and read when the worker starts,
    /// so the first automatic sync of a session is scheduled from the last
    /// one rather than run the moment the app opens. A file beside the
    /// database rather than a row in it: one small read, no query.
    pub last_sync: PathBuf,
}

impl AppPaths {
    /// The standard layout under an explicit base directory.
    ///
    /// Split out from [`AppPaths::resolve`] so that the layout and the
    /// configured-or-not decision can be tested without mutating process-wide
    /// environment, which no test in a threaded runner can do safely.
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

    /// Resolve the standard locations, honouring `XDG_DATA_HOME`.
    ///
    /// `<data>/harbour-vuo/harbour-vuo`, two levels, because that is the one
    /// directory the sandbox lets the app write: Sailjail whitelists
    /// `$HOME/.local/share/<OrganizationName>/<ApplicationName>` from the
    /// desktop entry's `[X-Sailjail]` section, and both names are
    /// `harbour-vuo` there.
    #[must_use]
    pub fn resolve() -> Option<Self> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?
            .join("harbour-vuo/harbour-vuo");
        Some(Self::under(base))
    }

    /// `Some` only once an account has been written.
    ///
    /// Returns `None` when the app has never been set up, which is not an
    /// error: it is what the first run looks like.
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

/// How long the worker waits before its next automatic sync, given the
/// chosen interval and when the mirror was last synced (Unix seconds), at
/// `now`.
///
/// `None` for the interval is "Manual only": never. A mirror never synced, or
/// synced longer ago than the interval, is due straight away -- after a short
/// pause, so an app that has just opened draws its list before the network
/// is touched. Otherwise the wait is what remains of the interval.
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

/// Note that the mirror was just synced. Best-effort: a stamp that could not
/// be written costs one extra background run, not data.
pub fn record_sync_time(stamp: &std::path::Path) {
    if let Err(e) = std::fs::write(stamp, format!("{}\n", chrono_now())) {
        tracing::info!(error = %e, path = %stamp.display(), "could not record the sync time");
    }
}

/// The stored account. Written with owner-only permissions.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Account {
    pub server_url: String,
    pub token: String,
    /// Whether to trust the CA certificate at [`AppPaths::ca_certificate`].
    ///
    /// Off unless the user turns it on *and* the file exists. §9.1 offers a
    /// per-host CA precisely so that nobody needs an "ignore certificate
    /// errors" switch; there is no such switch, and this is not one.
    #[serde(default)]
    pub use_custom_ca: bool,
    /// 0 strict, 1 ask, 2 allow. See `settings::MEDIA_*`.
    #[serde(default = "default_media_policy")]
    pub media_policy: i32,
    /// Index into `settings::SYNC_INTERVALS_MINUTES`.
    #[serde(default = "default_sync_interval_index")]
    pub sync_interval_index: i32,
    #[serde(default)]
    pub wifi_only: bool,
    /// When an opened article is marked read. See `settings::MARK_READ_*`.
    #[serde(default = "default_mark_read_delay_index")]
    pub mark_read_delay_index: i32,
    /// How long read, unfavourited articles are kept locally. Index into
    /// `settings::RETENTION_DAYS`; 0 keeps everything.
    #[serde(default)]
    pub retention_index: i32,
}

/// Ask, not Strict. On a stock Miniflux `MEDIA_PROXY_MODE` is `http-only`, so
/// most images arrive un-proxied and Strict would blank them.
fn default_media_policy() -> i32 {
    crate::settings::MEDIA_ASK
}

/// Hourly, not "Manual only". See `settings::SYNC_INTERVAL_DEFAULT_INDEX`.
fn default_sync_interval_index() -> i32 {
    crate::settings::SYNC_INTERVAL_DEFAULT_INDEX
}

/// After 5 seconds. An account file written before this setting existed gets
/// the same default a new install would, rather than silently "never".
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
        }
    }
}

/// Read the account file.
pub fn load_account(path: &std::path::Path) -> vuo_core::Result<Account> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| vuo_core::Error::Config(format!("could not read the account file: {e}")))?;
    serde_json::from_str(&raw)
        .map_err(|_| vuo_core::Error::Config("the account file is malformed".to_owned()))
}

/// Write the account file with mode 0600.
///
/// The permissions are set *before* the secret is written, not after: a file
/// created world-readable and chmod'ed afterwards is readable for the window
/// in between, and on a shared device that window is enough.
///
/// `OpenOptions::mode` applies only when the file is CREATED, so it does
/// nothing for an account file that already exists -- one left behind by an
/// older build, restored from a backup, or written by hand. That path is
/// covered by tightening the mode explicitly after the open, which is safe in
/// the same sense: `truncate(true)` has already emptied the file, so the
/// permissions are narrowed before any secret goes in.
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
        // The file is empty at this point, so this narrows before the token is
        // written rather than after.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| {
                vuo_core::Error::Config(format!("could not secure the account file: {e}"))
            })?;
    }

    let json = serde_json::to_vec_pretty(account)
        .map_err(|_| vuo_core::Error::Config("could not encode the account".to_owned()))?;
    file.write_all(&json)
        .map_err(|e| vuo_core::Error::Config(format!("could not write the account file: {e}")))?;
    Ok(())
}

/// Build the transport config for an account, including its CA if configured.
///
/// The CA is read only for an `https` server. A private certificate authority
/// is a statement about TLS, and a plain-`http` instance -- one reached over a
/// VPN, say, where the tunnel is the encryption -- performs no handshake for it
/// to apply to. Demanding the file anyway made the setting able to break a
/// configuration it has no bearing on: with the switch left on and no `ca.pem`
/// present, connecting to an `http://` server failed with "the custom CA
/// certificate ... could not be read", which names a file the user has no
/// reason to have and a problem they do not have.
pub fn transport_config_for(
    paths: &AppPaths,
    account: &Account,
) -> vuo_core::Result<TransportConfig> {
    let mut config = TransportConfig::default();
    let uses_tls = url::Url::parse(&account.server_url)
        .map(|u| u.scheme() == "https")
        .unwrap_or(false);
    if account.use_custom_ca && uses_tls {
        let pem = std::fs::read(&paths.ca_certificate).map_err(|e| {
            // Loud, never silent. Falling back to the platform roots here would
            // be an "ignore certificate errors" switch in effect: the user
            // would believe their private CA was in use when it was not.
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
    /// `Command` derives `Debug`, and one arm of it carries the whole account.
    /// A log line reaches journald and travels in bug reports, so what a
    /// `{:?}` on a command prints is a disclosure question, not a formatting
    /// one. `ApiToken` redacts itself; the SERVER is a `Url` and prints its
    /// userinfo, which is how `https://user:pass@host/` gets into a log from a
    /// field the user pasted it into.
    #[test]
    fn a_command_never_prints_the_account_it_carries() {
        let account = WorkerAccount {
            database: PathBuf::from("/home/defaultuser/.local/share/harbour-vuo/vuo.sqlite"),
            server: url::Url::parse("https://alice:hunter2@miniflux.example/").expect("url"),
            token: ApiToken::new("s3cr3t-key"),
            transport: TransportConfig::default(),
        };
        let printed = format!("{:?}", Command::Configure(Box::new(account)));
        // `probe`, not `secret`. These are fixtures -- the point of the test is
        // that they do NOT escape -- and a variable called `secret` formatted
        // into an assertion message is read by a scanner as a credential
        // reaching a log, which is the opposite of what this proves.
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
        // And the name a log line SHOULD carry is still there.
        assert_eq!(Command::TestConnection.name(), "TestConnection");
    }

    /// §the worker syncs on its own, on the interval the user chose.
    ///
    /// Vuo is one process, so the cadence is the worker's to keep. The rule
    /// is pure so it can be stated without a clock: never for "Manual only";
    /// soon for a mirror never synced or overdue, but not at once, so the
    /// list is drawn before the network is touched; otherwise what remains.
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

        // And the stamp round-trips through the file the worker reads.
        let dir = tempfile::tempdir().expect("tempdir");
        let stamp = dir.path().join("last-sync");
        assert_eq!(read_sync_time(&stamp), None, "no stamp yet");
        record_sync_time(&stamp);
        let recorded = read_sync_time(&stamp).expect("a stamp");
        assert!((chrono_now() - recorded).abs() < 5);
    }

    #[test]
    fn a_missing_custom_ca_fails_loudly_rather_than_falling_back() {
        // §9.1 gives TLS verification no toggle, and a silent fallback to the
        // platform roots would be one in effect: the user would believe their
        // private CA was in use when it was not.
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

    /// A real self-signed CA, so the fixture is one `Transport::new` accepts.
    ///
    /// The previous fixture was the single line `-----BEGIN CERTIFICATE-----`,
    /// which `reqwest::Certificate::from_pem` rejects -- so the configuration
    /// the test blessed could not in fact build a client.
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
    /// Reported from a device: a refresh that timed out on a dropped VPN left
    /// the entry list and the cover spinning forever. Two arms of the Sync
    /// match simply did not call the old `finish` closure, and nothing made
    /// that a compile error. The guard exists so that clearing happens because
    /// the iteration ended.
    #[test]
    fn the_command_guard_clears_the_spinner_however_the_arm_ends() {
        use crate::context::SyncSignal;

        let signal = SyncSignal::default();

        // A user-initiated Sync raises the flag from the Qt thread first.
        signal.set_running(true);
        {
            let _guard = CommandGuard {
                signal: &signal,
                changed: false,
                clears_spinner: true,
                trims_allocator: false,
            };
            // An arm that reports a failure and sets nothing at all.
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

        // A command that does not own the spinner must not lower one that a
        // refresh already running raised -- an opportunistic FlushOutbox is
        // fired by every star tap.
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

        // And an early `?`-style exit still clears, because Drop runs.
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
        // Found on a device. With the switch on and no ca.pem present, a
        // plain-http instance -- one reached over WireGuard, where the tunnel
        // is the encryption -- failed to connect at all, with "the custom CA
        // certificate at ... could not be read". No handshake happens on http
        // for a CA to apply to, so the setting must not be able to break a
        // configuration it has no bearing on.
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

        // And the same account over https still fails loudly: §9.1's rule that
        // a private CA is never silently ignored is about TLS, and holds there.
        let https = Account {
            server_url: "https://10.77.0.1:8083/".into(),
            ..account
        };
        assert!(transport_config_for(&paths, &https).is_err());
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

        // And the config must be one a client can be built from. Stopping at
        // the struct field blesses a configuration that `Transport::new`
        // refuses, which is the opposite of what this test's name claims.
        vuo_core::api::Transport::new(
            url::Url::parse("https://h.example/").expect("url"),
            vuo_core::redact::ApiToken::new("t"),
            &config,
        )
        .expect("a configured CA must produce a usable transport");

        // And off by default.
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
        // §7 relies on filesystem permissions plus Sailfish's home encryption,
        // so the permissions have to actually be right.
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
        // `OpenOptions::mode` applies only at CREATION, so it does nothing for
        // a file that already exists -- one from an older build, a restored
        // backup, or written by hand. Overwriting it used to leave the API key
        // world-readable.
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
        assert!(load_account(&path).is_err());
    }

    #[test]
    fn an_unconfigured_device_reports_no_paths_rather_than_failing() {
        // A timer that fires before first run must do nothing, quietly.
        //
        // This has to go through `AppPaths::configured` -- the decision
        // `from_env` returns through. Building an `AppPaths` literal pointing
        // at a file that does not exist and asserting `!path.exists()`, as
        // this test used to, is an assertion about `std::path::Path` and
        // passes with `configured` replaced by `Some(self)`.
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
    /// The window is a MAXIMUM AGE and not an idle timeout, which is the whole
    /// difference between "a reading session costs one request" and "a reader
    /// who taps steadily is never flushed at all". Expressed here as the rule
    /// the loop actually follows: a request that finds a deadline already set
    /// leaves it where it is.
    #[test]
    fn a_second_flush_request_does_not_push_the_deadline_back() {
        let started = std::time::Instant::now();
        let mut flush_due: Option<std::time::Instant> = None;

        // The first tap arms the window.
        if flush_due.is_none() {
            flush_due = Some(started + FLUSH_DEBOUNCE);
        }
        let armed = flush_due.expect("the first request arms the window");

        // A tap half a window later must not move it.
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
    /// A pass replays the outbox as its first step, so sending the flush
    /// separately a moment earlier would be one request for work the sync was
    /// about to do anyway -- which is the exact cost this whole mechanism
    /// exists to avoid.
    #[test]
    fn a_sync_subsumes_a_flush_that_falls_due_with_it() {
        let now = std::time::Instant::now();
        let past = now - std::time::Duration::from_secs(1);

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
    /// Waiting on the sync alone is what the loop did before the flush had a
    /// deadline of its own, and it would hold a reader's marks until the next
    /// interval -- up to an hour.
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
    /// Both ends matter. Too long and a reader who marks something here and
    /// looks at another device sees it unread; too short and a reading session
    /// is back to one request per tap.
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
    /// "Only sync on Wi-Fi" is a statement about what Vuo does unprompted. A
    /// refresh the reader pulled for, a feed they subscribed to, a connection
    /// they asked to test: those are their decision, and a phone on cellular
    /// must still do them. The distinction is structural rather than a flag
    /// each arm remembers to check -- the probe happens where a DEADLINE is
    /// resolved, and every arriving command runs below that -- so this asserts
    /// the structure.
    #[test]
    fn only_automatic_work_consults_the_network() {
        const SOURCE: &str = include_str!("worker.rs");
        // The test's own mention of the call is in this string, so count from
        // above it: everything up to the test module.
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
    /// Leaving `next_sync` where it was would make every wait expire
    /// immediately and spin the loop against `/proc` for as long as the phone
    /// stayed on cellular.
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
