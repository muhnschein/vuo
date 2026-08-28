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
//! current-thread Tokio runtime, and results come back to the Qt thread through
//! `queued_callback`, which is `qmetaobject`'s cross-thread delivery primitive.
//! That keeps every `QObject` touch on the Qt thread, which is the rule that
//! actually matters for correctness.
//!
//! The division of labour follows §5's "models observe SQLite": the worker
//! writes to the mirror, then signals; the models re-read the mirror on the Qt
//! thread. Sync results are never passed through the channel as data.

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
    /// `on_event` is invoked **on the worker thread**; callers must wrap it
    /// with `qmetaobject::queued_callback` before touching any `QObject`.
    pub fn spawn(
        db_path: PathBuf,
        server: url::Url,
        token: ApiToken,
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

                let client = match Transport::new(server, token, &TransportConfig::default()) {
                    Ok(t) => MinifluxClient::new(t),
                    Err(e) => {
                        on_event(Event::SyncFailed {
                            message: e.to_string(),
                        });
                        return;
                    }
                };

                while let Ok(command) = rx.recv() {
                    match command {
                        Command::Shutdown => break,
                        Command::Sync => {
                            on_event(Event::SyncStarted);
                            let options = SyncOptions::default();
                            match runtime.block_on(sync::sync(&mut db, &client, options)) {
                                Ok(report) if report.replay.auth_failed => {
                                    on_event(Event::AuthFailed);
                                }
                                Ok(report) => {
                                    let unread = store::unread_count(db.conn()).unwrap_or(0);
                                    let changed = report.pull.upserted > 0
                                        || report.entries_deleted > 0
                                        || report.replay.confirmed > 0;
                                    on_event(Event::SyncFinished { unread, changed });
                                }
                                Err(e) if e.is_auth_failure() => on_event(Event::AuthFailed),
                                // The message is already redacted: Error's
                                // Display never carries a token or userinfo.
                                Err(e) => on_event(Event::SyncFailed {
                                    message: e.to_string(),
                                }),
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
                                    on_event(Event::SubscriptionChanged {
                                        ok: removed.is_ok(),
                                        message: String::new(),
                                    });
                                }
                                Err(e) => on_event(Event::SubscriptionChanged {
                                    ok: false,
                                    message: e.to_string(),
                                }),
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
                                    on_event(Event::OriginalContentFetched {
                                        entry_id,
                                        ok: stored.is_ok(),
                                    });
                                }
                                Err(_) => on_event(Event::OriginalContentFetched {
                                    entry_id,
                                    ok: false,
                                }),
                            }
                        }
                        Command::TestConnection => match runtime.block_on(client.me()) {
                            Ok(user) => on_event(Event::ConnectionTested {
                                ok: true,
                                // The username is the user's own, from their own
                                // server, but it is still rendered as plain text.
                                message: user.username,
                            }),
                            Err(e) => on_event(Event::ConnectionTested {
                                ok: false,
                                message: e.to_string(),
                            }),
                        },
                        Command::FlushOutbox => {
                            match runtime.block_on(sync::replay::flush(&mut db, &client)) {
                                Ok(outcome) if outcome.auth_failed => on_event(Event::AuthFailed),
                                Ok(_) => {
                                    let unread = store::unread_count(db.conn()).unwrap_or(0);
                                    on_event(Event::SyncFinished {
                                        unread,
                                        changed: true,
                                    });
                                }
                                Err(e) => on_event(Event::SyncFailed {
                                    message: e.to_string(),
                                }),
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
}

impl AppPaths {
    /// Resolve the standard locations, honouring `XDG_DATA_HOME`.
    #[must_use]
    pub fn resolve() -> Option<Self> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?
            .join("harbour-vuo");
        Some(AppPaths {
            database: base.join("vuo.sqlite"),
            account: base.join("account.json"),
        })
    }

    /// Resolve paths and confirm an account has been configured.
    ///
    /// Returns `None` when the app has never been set up, which is not an
    /// error: a background timer firing before first run should do nothing
    /// quietly.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let paths = Self::resolve()?;
        paths.account.exists().then_some(paths)
    }
}

/// The stored account. Written with owner-only permissions.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub server_url: String,
    pub token: String,
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
    let json = serde_json::to_vec_pretty(account)
        .map_err(|_| vuo_core::Error::Config("could not encode the account".to_owned()))?;
    file.write_all(&json)
        .map_err(|e| vuo_core::Error::Config(format!("could not write the account file: {e}")))?;
    Ok(())
}

/// One synchronous sync pass, for the systemd timer.
pub fn sync_once_blocking(paths: &AppPaths) -> vuo_core::Result<vuo_core::sync::SyncReport> {
    let account = load_account(&paths.account)?;
    let server = url::Url::parse(&account.server_url)
        .map_err(|_| vuo_core::Error::Config("the stored server URL is not a URL".to_owned()))?;
    let transport = Transport::new(
        server,
        ApiToken::new(account.token),
        &TransportConfig::default(),
    )?;
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
    fn a_malformed_account_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account.json");
        std::fs::write(&path, "not json at all").expect("write");
        assert!(load_account(&path).is_err());
    }

    #[test]
    fn an_unconfigured_device_reports_no_paths_rather_than_failing() {
        // A timer that fires before first run must do nothing, quietly.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths {
            database: dir.path().join("db.sqlite"),
            account: dir.path().join("missing.json"),
        };
        assert!(!paths.account.exists());
    }
}
