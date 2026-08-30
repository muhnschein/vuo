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

use crate::context::Notice;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use vuo_core::api::{MinifluxClient, Transport, TransportConfig};
use vuo_core::db::outbox::{self, DesiredValue};
use vuo_core::db::{store, Database};
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::redact::ApiToken;
use vuo_core::sync::{self, SyncOptions};

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
    /// Flush the outbox without pulling.
    FlushOutbox,
    /// Subscribe to a feed. The *server* discovers and fetches it; §3 makes
    /// local feed fetching the project's most important boundary.
    Subscribe {
        feed_url: String,
    },
    Unsubscribe {
        feed_id: i64,
    },
    /// Ask the server to scrape the original article (§3: use the server's
    /// endpoint, never a local Readability port).
    FetchOriginal {
        entry_id: i64,
    },
    /// Verify the configured credentials, for the settings screen.
    TestConnection,
    Shutdown,
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
}

impl Drop for CommandGuard<'_> {
    fn drop(&mut self) {
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
    /// Spawn the worker.
    ///
    /// `on_event` is invoked **on the worker thread**. It must not touch a
    /// `QObject`: results the UI has to see travel by
    /// [`crate::context::SyncSignal`] instead, which the Qt thread polls. A
    /// caller that genuinely needs to reach a `QObject` from here has to wrap
    /// it with `qmetaobject::queued_callback` first, and then owes the event
    /// loop that primitive requires.
    pub fn spawn(
        db_path: PathBuf,
        server: url::Url,
        token: ApiToken,
        config: TransportConfig,
        signal: std::sync::Arc<crate::context::SyncSignal>,
        on_event: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Command>();

        let handle = thread::Builder::new()
            .name("vuo-sync".to_owned())
            .spawn(move || {
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

                let mut db = match Database::open(&db_path) {
                    Ok(db) => db,
                    Err(e) => {
                        on_event(Event::SyncFailed {
                            message: e.to_string(),
                        });
                        return;
                    }
                };

                let client = match Transport::new(server, token, &config) {
                    Ok(t) => MinifluxClient::new(t),
                    Err(e) => {
                        on_event(Event::SyncFailed {
                            message: e.to_string(),
                        });
                        return;
                    }
                };

                while let Ok(command) = rx.recv() {
                    // Shutdown is handled above the guard: there is no spinner
                    // to clear for it, and draining whatever is queued behind
                    // it matters more. A `Sync` sitting in the queue when the
                    // context is retired would otherwise leave `running` set
                    // with nothing left alive to clear it.
                    if matches!(command, Command::Shutdown) {
                        while rx.try_recv().is_ok() {}
                        signal.set_running(false);
                        break;
                    }

                    // See CommandGuard: clearing the spinner is structural, so
                    // an arm that returns early -- or one added later -- cannot
                    // leave it spinning. Arms opt into the bump.
                    let mut guard = CommandGuard {
                        signal: &signal,
                        changed: false,
                        clears_spinner: matches!(command, Command::Sync),
                    };
                    match command {
                        // Handled above, before the guard.
                        Command::Shutdown => break,
                        Command::Sync => {
                            on_event(Event::SyncStarted);
                            let options = SyncOptions::default();
                            match runtime.block_on(sync::sync(&mut db, &client, options)) {
                                Ok(report) if report.replay.auth_failed => {
                                    guard.changed = true;
                                    signal.post(Notice::SyncFailed {
                                        auth: true,
                                        message: String::new(),
                                    });
                                    on_event(Event::AuthFailed);
                                }
                                Ok(report) => {
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
                        }
                        Command::Subscribe { feed_url } => {
                            let result = runtime.block_on(client.create_feed(&feed_url, None));
                            match result {
                                Ok(_) => {
                                    // Pull immediately so the new feed's entries
                                    // appear without waiting for the next timer.
                                    let _ = runtime.block_on(sync::sync(
                                        &mut db,
                                        &client,
                                        SyncOptions::default(),
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
                        Command::FetchOriginal { entry_id } => {
                            let id = EntryId(entry_id);
                            match runtime.block_on(client.fetch_original_content(id)) {
                                Ok(content) => {
                                    // Store the scraped body against the entry so
                                    // it survives a restart and stays readable
                                    // offline.
                                    let stored = db.with_tx(|tx| {
                                        tx.execute(
                                            "UPDATE entries SET content = ?2 WHERE id = ?1",
                                            rusqlite::params![id.get(), content.content],
                                        )
                                        .map_err(vuo_core::Error::from)
                                    });
                                    // Same as Unsubscribe: the scraped body is
                                    // in SQLite, so the open article is stale
                                    // until something reloads it.
                                    guard.changed = stored.is_ok();
                                    on_event(Event::OriginalContentFetched {
                                        entry_id,
                                        ok: stored.is_ok(),
                                    });
                                }
                                Err(e) => {
                                    // Reported nothing at all before, so the
                                    // menu item looked identical whether the
                                    // scrape worked, failed, or was never
                                    // reachable.
                                    signal.post(Notice::SyncFailed {
                                        auth: e.is_auth_failure(),
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
                        Command::TestConnection => match runtime.block_on(client.me()) {
                            Ok(user) => {
                                // The username is the user's own, from their own
                                // server, but it is still rendered as plain text.
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
                        },
                        Command::FlushOutbox => {
                            match runtime.block_on(sync::replay::flush(&mut db, &client)) {
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

    /// Tell the worker to stop, without waiting for it to finish.
    ///
    /// [`Drop`] joins, which is what process shutdown wants: the thread must
    /// not outlive the process's use of the mirror. Replacing a live context
    /// is the other case, and there a join is a hazard — it runs on the Qt
    /// thread, and a worker in the middle of a request holds its thread until
    /// the network times out, so the UI would freeze for exactly that long.
    /// Dropping the join handle detaches instead: the thread still receives
    /// `Shutdown` and still winds down, it is simply not waited for.
    pub fn retire(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        // Dropping a `JoinHandle` detaches the thread; it does not stop it.
        // The `Shutdown` above is what stops it.
        let _ = self.handle.take();
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
    /// The sync timer unit itself, which ships in the separate
    /// `harbour-vuo-sync` package.
    ///
    /// Its presence is how the app knows whether background refresh exists on
    /// this device at all. A bare `stat`, deliberately: asking systemd
    /// (`systemctl --user show`) forks, blocks, and answers "not-found" for a
    /// unit it merely has not reloaded yet.
    pub timer_unit: PathBuf,
    /// Where the systemd user drop-in for the sync timer is written.
    ///
    /// Under `XDG_CONFIG_HOME`, not the data dir the rest of these live in:
    /// systemd reads unit configuration from the config hierarchy and would
    /// never look at a file placed beside the database.
    pub timer_dropin_dir: PathBuf,
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
        // Kept under the same base so a test can point everything at one
        // tempdir; `resolve` below puts it in the real systemd location.
        let timer_dropin_dir = base.join("systemd/user/harbour-vuo-sync.timer.d");
        AppPaths {
            database: base.join("vuo.sqlite"),
            account: base.join("account.json"),
            ca_certificate: base.join("ca.pem"),
            // Under the same base so a test can stage one; `resolve` below
            // points at the real packaged location.
            timer_unit: base.join("systemd/user/harbour-vuo-sync.timer"),
            timer_dropin_dir,
        }
    }

    /// Resolve the standard locations, honouring `XDG_DATA_HOME`.
    #[must_use]
    pub fn resolve() -> Option<Self> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?
            .join("harbour-vuo");
        let mut paths = Self::under(base);
        // Where the harbour-vuo-sync package installs the unit; see the spec's
        // %{_userunitdir}.
        paths.timer_unit = PathBuf::from("/usr/lib/systemd/user/harbour-vuo-sync.timer");
        // The real location, which is NOT under the data dir: systemd only
        // reads unit drop-ins out of the config hierarchy.
        paths.timer_dropin_dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?
            .join("systemd/user/harbour-vuo-sync.timer.d");
        Some(paths)
    }

    /// `Some` only once an account has been written.
    ///
    /// Returns `None` when the app has never been set up, which is not an
    /// error: a background timer firing before first run should do nothing
    /// quietly.
    #[must_use]
    pub fn configured(self) -> Option<Self> {
        self.account.exists().then_some(self)
    }

    /// Whether the background-sync package is installed on this device.
    ///
    /// The interval setting drives a systemd timer that ships in
    /// `harbour-vuo-sync`. With that package absent the control governs
    /// nothing, so the Settings page hides the whole section rather than
    /// offering a choice with no effect.
    #[must_use]
    pub fn background_sync_installed(&self) -> bool {
        self.timer_unit.exists()
    }

    /// Resolve paths and confirm an account has been configured.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::resolve()?.configured()
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
    #[serde(default)]
    pub sync_interval_index: i32,
    #[serde(default)]
    pub wifi_only: bool,
    /// When an opened article is marked read. See `settings::MARK_READ_*`.
    #[serde(default = "default_mark_read_delay_index")]
    pub mark_read_delay_index: i32,
}

/// Ask, not Strict. On a stock Miniflux `MEDIA_PROXY_MODE` is `http-only`, so
/// most images arrive un-proxied and Strict would blank them.
fn default_media_policy() -> i32 {
    1
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
            sync_interval_index: 0,
            wifi_only: false,
            mark_read_delay_index: default_mark_read_delay_index(),
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

/// One synchronous sync pass, for the systemd timer.
pub fn sync_once_blocking(paths: &AppPaths) -> vuo_core::Result<vuo_core::sync::SyncReport> {
    let account = load_account(&paths.account)?;
    let server = url::Url::parse(&account.server_url)
        .map_err(|_| vuo_core::Error::Config("the stored server URL is not a URL".to_owned()))?;
    let config = transport_config_for(paths, &account)?;
    let transport = Transport::new(server, ApiToken::new(account.token), &config)?;
    let client = MinifluxClient::new(transport);
    let mut db = Database::open(&paths.database)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| vuo_core::Error::Config(format!("could not start a runtime: {e}")))?;
    runtime.block_on(sync::sync(&mut db, &client, SyncOptions::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            timer_unit: dir.path().join("harbour-vuo-sync.timer"),
            timer_dropin_dir: dir.path().join("tdd"),
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
            timer_unit: dir.path().join("harbour-vuo-sync.timer"),
            timer_dropin_dir: dir.path().join("tdd"),
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
            timer_unit: dir.path().join("harbour-vuo-sync.timer"),
            timer_dropin_dir: dir.path().join("tdd"),
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
}
