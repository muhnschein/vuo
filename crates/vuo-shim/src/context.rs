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
    pub fn new(db: Database, worker: crate::worker::Worker, instance: url::Url) -> Rc<Self> {
        let commands = worker.sender();
        Rc::new(AppContext {
            db: Rc::new(RefCell::new(db)),
            commands,
            instance,
            _worker: worker,
        })
    }

    #[must_use]
    pub fn instance(&self) -> &url::Url {
        &self.instance
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
}
