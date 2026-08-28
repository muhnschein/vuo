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
use vuo_core::db::{Database, store};
use vuo_core::model::{EntryId, EntryStatus};
use vuo_core::redact::ApiToken;
use vuo_core::sync::{self, SyncOptions};

/// What the UI can ask the worker to do.
#[derive(Debug)]
pub enum Command {
    /// Run a full sync pass.
    Sync,
    /// Flush the outbox without pulling.
    FlushOutbox,
    Shutdown,
}

/// What the worker reports back.
#[derive(Debug, Clone)]
pub enum Event {
    SyncStarted,
    SyncFinished { unread: i64, changed: bool },
    /// Already-redacted, user-presentable text. Rendered as plain text (§9.3).
    SyncFailed { message: String },
    /// The API key was rejected; the UI should send the user to settings.
    AuthFailed,
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
                        on_event(Event::SyncFailed { message: e.to_string() });
                        return;
                    }
                };

                let client = match Transport::new(server, token, &TransportConfig::default()) {
                    Ok(t) => MinifluxClient::new(t),
                    Err(e) => {
                        on_event(Event::SyncFailed { message: e.to_string() });
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
                                Err(e) => on_event(Event::SyncFailed { message: e.to_string() }),
                            }
                        }
                        Command::FlushOutbox => {
                            match runtime.block_on(sync::replay::flush(&mut db, &client)) {
                                Ok(outcome) if outcome.auth_failed => on_event(Event::AuthFailed),
                                Ok(_) => {
                                    let unread = store::unread_count(db.conn()).unwrap_or(0);
                                    on_event(Event::SyncFinished { unread, changed: true });
                                }
                                Err(e) => on_event(Event::SyncFailed { message: e.to_string() }),
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
pub fn apply_local_status(db: &mut Database, id: EntryId, status: EntryStatus) -> vuo_core::Result<()> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue(tx, id, DesiredValue::Status(status), now))
}

pub fn apply_local_starred(db: &mut Database, id: EntryId, starred: bool) -> vuo_core::Result<()> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue(tx, id, DesiredValue::Starred(starred), now))
}

pub fn apply_local_mark_feed_read(db: &mut Database, feed_id: i64) -> vuo_core::Result<usize> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue_mark_feed_read(tx, feed_id, now))
}

pub fn apply_local_mark_category_read(db: &mut Database, category_id: i64) -> vuo_core::Result<usize> {
    let now = chrono_now();
    db.with_tx(|tx| outbox::queue_mark_category_read(tx, category_id, now))
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
