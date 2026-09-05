//! The settings screen's backing object.
//!
//! Holds the account and the handful of preferences that change behaviour.
//! §7: *the API key is stored under the app's data directory with restrictive
//! permissions, relying on Sailfish's home encryption. No custom keyring, no
//! SQLCipher, unless a concrete threat model justifies it.*
//!
//! The token deliberately does not live in the SQLite mirror. The mirror is a
//! cache that can be deleted, copied for a bug report, or inspected by a
//! developer; a credential in it would travel with all of that.

// Fires inside code the `QObject` derive generates, not in anything written
// here, and a struct-level attribute does not reach the expanded impls. §9.5
// confines unsafe to this crate for exactly this reason: qmetaobject's
// generated glue does pointer work that clippy reads as suspicious in
// isolation. Only `Settings` trips it -- the list models use
// `qt_base_class!(trait QAbstractListModel)`, which expands differently.
#![allow(clippy::useless_transmute)]

use qmetaobject::*;

use crate::context::AppContext;
use crate::worker::{self, Account, AppPaths, Command};

/// How un-proxied third-party media is treated. Crosses to QML as an integer,
/// because this `qmetaobject` version has no `qml_register_enum` on Qt 5.6.
pub const MEDIA_STRICT: i32 = 0;
pub const MEDIA_ASK: i32 = 1;
pub const MEDIA_ALLOW: i32 = 2;

/// Background refresh intervals, in minutes. Index 0 is "manual only".
pub const SYNC_INTERVALS_MINUTES: [i64; 5] = [0, 15, 30, 60, 360];

/// When an opened article is marked read. Index 0 is "never".
///
/// "Never" sits at index 0 on purpose: `i32::default()` is 0, so any future
/// wiring mistake degrades to the feature doing nothing, rather than to every
/// article the user glances at being marked read and pushed to the server.
/// That is the safe polarity for a destructive default.
pub const MARK_READ_NEVER: i32 = 0;
pub const MARK_READ_IMMEDIATELY: i32 = 1;
/// Seconds for the delayed choices, from index 2 onward.
pub const MARK_READ_DELAYS_SECONDS: [i32; 3] = [5, 15, 30];
/// A new install marks read after 5 seconds: long enough to absorb a mis-tap
/// and a glance at the headline, short enough that a read article does not
/// stay in the unread list.
pub const MARK_READ_DEFAULT_INDEX: i32 = 2;

/// How long an article must be open before it counts as read.
///
/// `None` never, `Some(0)` immediately, `Some(n)` after n seconds. An index a
/// hand-edited account file made up falls back to `None` -- the same
/// conservative direction as the constant above.
#[must_use]
pub fn mark_read_delay_seconds(index: i32) -> Option<i32> {
    match index {
        MARK_READ_NEVER => None,
        MARK_READ_IMMEDIATELY => Some(0),
        other => usize::try_from(other)
            .ok()
            .and_then(|i| i.checked_sub(2))
            .and_then(|i| MARK_READ_DELAYS_SECONDS.get(i))
            .copied(),
    }
}

#[derive(QObject, Default)]
pub struct Settings {
    base: qt_base_class!(trait QObject),

    serverUrl: qt_property!(QString; NOTIFY changed),
    apiKey: qt_property!(QString; NOTIFY changed),
    /// 0 strict, 1 ask (default), 2 allow. See [`MEDIA_ASK`].
    mediaPolicy: qt_property!(i32; NOTIFY changed),
    syncIntervalIndex: qt_property!(i32; NOTIFY changed),
    /// When an opened article is marked read. See [`MARK_READ_NEVER`].
    markReadDelayIndex: qt_property!(i32; NOTIFY changed),
    wifiOnly: qt_property!(bool; NOTIFY changed),
    useCustomCa: qt_property!(bool; NOTIFY changed),
    /// How many local changes are still waiting to reach the server. Shown so
    /// the user can tell "nothing happened" from "not sent yet".
    pendingActions: qt_property!(i32; READ pendingActionsCount NOTIFY changed),
    /// Whether an account is stored at all: a server and a key. What the
    /// root window asks on start-up to choose between the entry list and the
    /// onboarding page, and what the onboarding page asks again on its way
    /// back from Settings. Read from the file each time, so the answer is
    /// right before anything has called `reload`.
    configured: qt_property!(bool; READ is_configured NOTIFY changed),

    changed: qt_signal!(),
    /// Result of a connection test. `ok` false means the message is an error.
    connectionTested: qt_signal!(ok: bool, message: QString),

    save: qt_method!(fn(&mut self)),
    /// Read the stored account into this object's properties.
    ///
    /// QML has to call this, and nothing did. `attach` -- the only caller of
    /// `load` -- has no production callers at all, so the settings screen
    /// never read the account file: every visit showed a blank server address
    /// and a blank API key however many times they had been saved, and every
    /// other control showed a Rust default rather than the stored value. The
    /// Images setting in particular showed "Never load", because `i32::default`
    /// is 0 and the `MEDIA_ASK` default only happens inside the load that was
    /// not running.
    reload: qt_method!(fn(&mut self)),
    testConnection: qt_method!(fn(&mut self)),
    /// Drain the worker's pending result and fire [`connectionTested`].
    ///
    /// A poll for the same reason `EntryModel::pollSync` is one: QML owns this
    /// object, so Rust has no handle to signal into. Before this existed the
    /// worker's answer reached a log line and nothing else, so "Test
    /// connection" gave the user no feedback at all -- right or wrong
    /// credentials, the result Label stayed hidden.
    pollNotice: qt_method!(fn(&mut self) -> bool),

    ctx: Option<std::rc::Rc<AppContext>>,

    /// A result this page produced itself, with no worker involved: "there is
    /// nothing to test yet", "the address is not a URL", "the account could
    /// not be written".
    ///
    /// It waits in a one-shot slot and is drained by the same poll the
    /// worker's answers arrive through, rather than firing `connectionTested`
    /// straight from inside the tap. QML starts that poll *after*
    /// `testConnection()` returns, so a signal emitted during the call would
    /// be missed and the poll would then run forever waiting for an answer
    /// that had already been given.
    local_notice: Option<(bool, String)>,
}

impl Settings {
    pub fn attach(&mut self, ctx: std::rc::Rc<AppContext>) {
        self.ctx = Some(ctx);
        self.load();
        self.changed();
    }

    /// Read the stored account, if there is one.
    fn load(&mut self) {
        let Some(paths) = AppPaths::resolve() else {
            return;
        };
        self.load_from(&paths);
    }

    /// [`Settings::load`] against explicit paths.
    ///
    /// Split out so the settings screen's own read path can be tested. The
    /// no-arg version resolves `$XDG_DATA_HOME`/`$HOME`, so a test that called
    /// it was reading the developer's real home directory: on a machine where
    /// Vuo is actually configured it took the loaded branch and passed with the
    /// default flipped.
    pub fn load_from(&mut self, paths: &AppPaths) {
        if let Ok(account) = worker::load_account(&paths.account) {
            self.serverUrl = QString::from(account.server_url);
            // The key is loaded so the field is not blank when the user opens
            // settings to change something else. It is displayed with
            // echoMode: Password.
            self.apiKey = QString::from(account.token);
            self.useCustomCa = account.use_custom_ca;
            self.mediaPolicy = account.media_policy;
            self.syncIntervalIndex = account.sync_interval_index;
            self.wifiOnly = account.wifi_only;
            self.markReadDelayIndex = account.mark_read_delay_index;
            return;
        }
        // First run: default to Ask rather than Strict, because on a stock
        // Miniflux most images are un-proxied and Strict would blank them.
        self.mediaPolicy = MEDIA_ASK;
        self.markReadDelayIndex = MARK_READ_DEFAULT_INDEX;
    }

    fn is_configured(&self) -> bool {
        AppPaths::resolve()
            .and_then(|paths| worker::load_account(&paths.account).ok())
            .is_some_and(|account| !account.server_url.is_empty() && !account.token.is_empty())
    }

    fn pendingActionsCount(&self) -> i32 {
        self.ctx
            .clone()
            .or_else(crate::context::current)
            .as_ref()
            .and_then(|ctx| ctx.read(|db| vuo_core::db::outbox::len(db.conn()).unwrap_or(0)))
            .map(|n| i32::try_from(n).unwrap_or(i32::MAX))
            .unwrap_or(0)
    }

    fn reload(&mut self) {
        self.load();
        self.changed();
    }

    fn save(&mut self) {
        let Some(paths) = AppPaths::resolve() else {
            tracing::warn!("no data directory: the account cannot be stored");
            return;
        };
        self.save_to(&paths);
    }

    /// [`Settings::save`] against explicit paths.
    ///
    /// The copy from the QML-visible properties into `Account` lives here, so
    /// a test can drive the real one. The round-trip test used to build an
    /// `Account` by hand and round-trip THAT, which exercises serde's derive
    /// rather than the settings screen: dropping three fields from this copy
    /// left it green.
    pub fn save_to(&mut self, paths: &AppPaths) {
        let account = Account {
            server_url: self.serverUrl.to_string().trim().to_owned(),
            token: self.apiKey.to_string().trim().to_owned(),
            use_custom_ca: self.useCustomCa,
            media_policy: self.mediaPolicy,
            sync_interval_index: self.syncIntervalIndex,
            wifi_only: self.wifiOnly,
            mark_read_delay_index: self.markReadDelayIndex,
        };
        if account.server_url.is_empty() || account.token.is_empty() {
            return;
        }
        // Written with mode 0600; see worker::save_account.
        //
        // The result used to be discarded. It cannot be: everything below
        // assumes the file on disk now says what this screen says, and if the
        // write failed the context rebuild would happily come back with the
        // PREVIOUS account and report a confident answer about it.
        if let Err(e) = worker::save_account(&paths.account, &account) {
            tracing::warn!(error = %e, "could not write the account file");
            self.local_notice = Some((false, e.to_string()));
            return;
        }

        // Make the running app match what was just written.
        //
        // This is the hop the first-run failure hid behind. The context — the
        // mirror and the worker thread — was built exactly once, at start-up,
        // from an account file that does not exist until this function has run
        // at least once. So the first time a user filled in a server and a key,
        // there was still no worker: "Test connection" and the pulley menu's
        // Refresh reached nothing and did nothing, with no error and no
        // spinner, until Vuo was restarted.
        //
        // It also matters on every later save. The worker captures the server
        // URL and the API key when it spawns, so without this a test after
        // editing the address would answer for the address it replaced.
        // `refresh` is a no-op when neither changed.
        if let Err(e) = crate::context::refresh(paths) {
            tracing::warn!(error = %e, "the saved account did not yield a usable context");
            self.local_notice = Some((false, e.to_string()));
        }

        // And publish the media policy to the live context, or the open article
        // keeps the policy it was built with. This is the hop that was missing:
        // the control was rendered, persisted, and read back on the next
        // launch, but never reached the transform.
        if let Some(ctx) = self.ctx.clone().or_else(crate::context::current) {
            ctx.set_media_policy(self.mediaPolicy);
            ctx.set_mark_read_delay_index(self.markReadDelayIndex);
            // And the interval, to the worker that keeps it. A context
            // rebuilt just above was told at build time; one that survived
            // the save -- same server, same key -- hears it here.
            ctx.send(worker::Command::SetSyncInterval {
                minutes: sync_interval_minutes_for(self.syncIntervalIndex),
            });
        }
        self.changed();
    }

    fn testConnection(&mut self) {
        let Some(paths) = AppPaths::resolve() else {
            self.local_notice = Some((
                false,
                "could not work out where to store the account".to_owned(),
            ));
            return;
        };
        self.test_connection_with(&paths);
    }

    /// [`Settings::testConnection`] against explicit paths.
    ///
    /// Split out for the same reason `save_to` is: the no-arg version resolves
    /// `$XDG_DATA_HOME`/`$HOME`, so a test that drove it would be reading — and
    /// writing — the developer's real account file.
    ///
    /// Every branch here leaves the user something to read. The button used to
    /// have three ways to do nothing at all: no context to send through, a
    /// worker that had already stopped, and a send whose result was discarded.
    /// "Nothing happened" is the one answer a Test button must never give.
    pub fn test_connection_with(&mut self, paths: &AppPaths) {
        // Each tap starts fresh. An answer left over from the previous one --
        // not yet drained, since the page polls every 400ms -- would otherwise
        // be read as this tap's, and would short-circuit the check below.
        self.local_notice = None;
        self.save_to(paths);

        // `save_to` refreshes the context, so by here there is one unless the
        // account could not be written or does not describe a usable
        // configuration -- in which case it left the reason here, and that
        // reason is better than anything this could add.
        if self.local_notice.is_some() {
            return;
        }

        let Some(ctx) = self.ctx.clone().or_else(crate::context::current) else {
            // `save_to` returns early, without writing, when either field is
            // blank -- so this is what an empty form reaches.
            self.local_notice = Some((
                false,
                "fill in the server address and the API key first".to_owned(),
            ));
            return;
        };

        if !ctx.send(Command::TestConnection) {
            // The worker stops itself if the mirror or the HTTP client could
            // not be built, and a send into its closed channel is silent.
            self.local_notice = Some((
                false,
                "the sync worker is not running; restart Vuo".to_owned(),
            ));
        }
    }

    fn pollNotice(&mut self) -> bool {
        // This page's own result comes first, and is checked before the
        // context is: it exists precisely for the cases where the request
        // never reached a worker, and in the worst of those there is no
        // context to poll at all.
        if let Some((ok, message)) = self.local_notice.take() {
            self.on_connection_tested(ok, &message);
            return true;
        }
        let Some(ctx) = self.ctx.clone().or_else(crate::context::current) else {
            return false;
        };
        let signal = std::sync::Arc::clone(ctx.signal_handle());
        self.poll_notice_from(&signal)
    }

    /// [`Settings::pollNotice`] against an explicit signal, so the worker →
    /// page round trip can be tested without a QML engine or a context.
    pub fn poll_notice_from(&mut self, signal: &crate::context::SyncSignal) -> bool {
        match signal.take_notice() {
            Some(crate::context::Notice::ConnectionTested { ok, message }) => {
                self.on_connection_tested(ok, &message);
                true
            }
            // Not this page's to show; put it back so the page that cares can
            // take it.
            Some(other) => {
                signal.post(other);
                false
            }
            None => false,
        }
    }

    /// Fire the QML-visible signal. Reached through [`Settings::pollNotice`].
    pub fn on_connection_tested(&mut self, ok: bool, message: &str) {
        self.connectionTested(ok, QString::from(message.to_owned()));
    }

    /// The media policy as the core's type.
    #[must_use]
    pub fn media_policy_for(&self, instance: url::Url) -> vuo_core::content::MediaPolicy {
        use vuo_core::content::{MediaPolicy, UnproxiedMedia};
        let fallback = match self.mediaPolicy {
            MEDIA_STRICT => UnproxiedMedia::Strict,
            MEDIA_ALLOW => UnproxiedMedia::Allow,
            _ => UnproxiedMedia::Ask,
        };
        MediaPolicy::ProxyThroughInstance {
            instance,
            extra_trusted: Vec::new(),
            fallback,
        }
    }

    /// The chosen sync interval in minutes, or `None` for "Manual only".
    pub fn sync_interval_minutes(&self) -> Option<i64> {
        sync_interval_minutes_for(self.syncIntervalIndex)
    }
}

/// The sync interval a stored `sync_interval_index` means, in minutes, or
/// `None` for "Manual only". Shared with the context, which reads the index
/// from the account file when it builds the worker and has no `Settings`.
#[must_use]
pub fn sync_interval_minutes_for(index: i32) -> Option<i64> {
    let index = usize::try_from(index).unwrap_or(0);
    match SYNC_INTERVALS_MINUTES.get(index) {
        Some(0) | None => None,
        Some(minutes) => Some(*minutes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(dir: &tempfile::TempDir) -> AppPaths {
        let base = dir.path().join("harbour-vuo");
        std::fs::create_dir_all(&base).expect("mkdir");
        AppPaths::under(base)
    }

    #[test]
    fn every_setting_survives_a_round_trip() {
        // The Images, Background refresh and Wi-Fi-only controls were rendered
        // but never persisted or read: changing them did nothing at all, and
        // nothing in the build said so.
        //
        // The round trip goes through `Settings` itself. It used to build an
        // `Account` literal and round-trip THAT through save_account /
        // load_account, which tests serde's derive rather than the settings
        // screen: dropping `use_custom_ca`, `media_policy` and
        // `sync_interval_index` from `Settings::save`'s copy -- the exact bug
        // in the comment above -- left it green.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);

        let mut written = Settings {
            serverUrl: QString::from("https://h.example/"),
            apiKey: QString::from("t"),
            useCustomCa: true,
            mediaPolicy: MEDIA_ALLOW,
            syncIntervalIndex: 3,
            wifiOnly: true,
            ..Settings::default()
        };
        written.save_to(&paths);

        let mut read = Settings::default();
        read.load_from(&paths);

        assert_eq!(read.serverUrl.to_string(), "https://h.example/");
        assert_eq!(read.apiKey.to_string(), "t");
        assert_eq!(read.mediaPolicy, MEDIA_ALLOW);
        assert_eq!(read.syncIntervalIndex, 3);
        assert!(read.wifiOnly);
        assert!(read.useCustomCa);
    }

    #[test]
    fn an_account_file_from_an_older_build_still_loads() {
        // The new fields must be optional, or upgrading would strand the user
        // with an unreadable account and no obvious way back.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account.json");
        std::fs::write(&path, r#"{"server_url":"https://h.example/","token":"t"}"#).expect("write");
        let read = worker::load_account(&path).expect("an older account file must still load");
        assert_eq!(read.media_policy, MEDIA_ASK, "and get the safe default");
        assert!(!read.wifi_only);
    }

    #[test]
    fn the_default_media_policy_is_ask_not_strict() {
        // On a stock Miniflux MEDIA_PROXY_MODE is http-only, so most images
        // arrive un-proxied. Strict by default would blank most articles.
        //
        // Against an EMPTY directory of its own. Calling the no-arg `load()`,
        // as this used to, reads $XDG_DATA_HOME/$HOME: on a machine where the
        // developer actually runs Vuo it took the loaded branch and passed with
        // the default flipped to Strict.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);
        assert!(!paths.account.exists(), "this must be a first run");

        let mut s = Settings::default();
        s.load_from(&paths);
        assert_eq!(s.mediaPolicy, MEDIA_ASK);
    }

    #[test]
    fn a_worker_result_reaches_the_settings_page() {
        // The worker answers "test this connection" on its own thread. Before
        // `pollNotice` existed that answer reached a log line and nothing
        // else: `on_connection_tested` had ZERO callers, so `connectionTested`
        // never fired and SettingsPage's result Label stayed hidden whether
        // the credentials were right or wrong.
        let signal = std::sync::Arc::new(crate::context::SyncSignal::default());

        let mut s = Settings::default();
        assert!(
            !s.poll_notice_from(&signal),
            "nothing pending: the page must not be told anything"
        );

        // As the worker does, from its own thread.
        let writer = std::sync::Arc::clone(&signal);
        std::thread::spawn(move || {
            writer.post(crate::context::Notice::ConnectionTested {
                ok: true,
                message: "alice".to_owned(),
            });
        })
        .join()
        .expect("worker thread");

        assert!(
            s.poll_notice_from(&signal),
            "the worker's answer must reach the page"
        );
        assert!(
            !s.poll_notice_from(&signal),
            "and it is one-shot: a drained notice must not repeat forever"
        );

        // A notice meant for another page is left where it was.
        signal.post(crate::context::Notice::SubscriptionChanged {
            ok: false,
            message: "no".to_owned(),
        });
        assert!(!s.poll_notice_from(&signal));
        assert!(
            signal.take_notice().is_some(),
            "a notice this page does not handle must stay for the one that does"
        );
    }

    /// §the first run, end to end: an account is configured and the app works.
    ///
    /// This is the defect a real device found. The context — the mirror and
    /// the worker thread — was built exactly once, in `main`, from an account
    /// file that does not exist before the user has been to Settings. On a
    /// first run that build failed, nothing retried it, and every
    /// worker-backed action in the running app silently did nothing: "Test
    /// connection" gave no answer of any kind, and the pulley menu's Refresh
    /// did not even spin. Restarting Vuo fixed it, which is why it survived.
    ///
    /// Nothing could catch it. `main` is in `harbour-vuo`, which has no tests
    /// and which `make check` only lints.
    #[test]
    fn configuring_an_account_makes_the_app_work_without_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);
        assert!(
            crate::context::current().is_none(),
            "a first run starts with no context; that is the whole premise"
        );

        // Exactly what the user types into the two fields, for the setup that
        // found this: plain http to a WireGuard address, no TLS.
        let mut s = Settings {
            serverUrl: QString::from("http://10.77.0.1:8083/"),
            apiKey: QString::from("k"),
            ..Settings::default()
        };
        s.test_connection_with(&paths);

        let ctx = crate::context::current()
            .expect("configuring an account must make the app usable without a restart");
        assert_eq!(ctx.instance().as_str(), "http://10.77.0.1:8083/");
        assert!(
            ctx.send(Command::TestConnection),
            "and there must be a worker listening for the command"
        );
        assert!(
            s.local_notice.is_none(),
            "the request went through, so the page has nothing of its own to report: {:?}",
            s.local_notice
        );
    }

    /// The worker captures the server URL and the API key when it spawns, so a
    /// context outlives the credentials it was built from. Editing either and
    /// testing again used to ask the OLD worker, which answered confidently
    /// about the account the user had just replaced.
    #[test]
    fn changing_the_server_replaces_the_worker_that_answers_for_it() {
        use std::rc::Rc;

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);

        let mut s = Settings {
            serverUrl: QString::from("http://10.77.0.1:8083/"),
            apiKey: QString::from("k"),
            ..Settings::default()
        };
        s.save_to(&paths);
        let first = crate::context::current().expect("a context");

        // A save that changed no credential must NOT churn the worker: the
        // Wi-Fi switch has no business restarting a sync in flight.
        s.wifiOnly = true;
        s.save_to(&paths);
        let after_unrelated_save = crate::context::current().expect("a context");
        assert!(
            Rc::ptr_eq(&first, &after_unrelated_save),
            "an unrelated setting must not restart the worker"
        );

        // A changed address must.
        s.serverUrl = QString::from("http://10.77.0.2:8083/");
        s.save_to(&paths);
        let after_new_server = crate::context::current().expect("a context");
        assert!(
            !Rc::ptr_eq(&first, &after_new_server),
            "a changed server must not keep answering through the old worker"
        );
        assert_eq!(
            after_new_server.instance().as_str(),
            "http://10.77.0.2:8083/"
        );

        // So must a changed key, which does not show up in the origin at all.
        s.apiKey = QString::from("k2");
        s.save_to(&paths);
        let after_new_key = crate::context::current().expect("a context");
        assert!(
            !Rc::ptr_eq(&after_new_server, &after_new_key),
            "a changed API key must not keep answering through the old worker"
        );
    }

    /// "Nothing happened" is the one answer a Test button must never give.
    #[test]
    fn a_connection_test_that_reaches_no_worker_still_answers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);

        // An empty form. `save_to` declines to write it, so nothing downstream
        // exists to send through -- which used to mean the tap did nothing at
        // all, right down to leaving the QML poll spinning for an answer that
        // was never coming.
        let mut s = Settings::default();
        s.test_connection_with(&paths);
        assert!(
            s.local_notice.is_some(),
            "an empty form must be told what is missing, not ignored"
        );
        assert!(s.pollNotice(), "and the page's poll must deliver it");
        assert!(
            !s.pollNotice(),
            "one-shot: a drained result must not repeat forever"
        );

        // An address that is not a URL is the other way to get here, and the
        // likeliest typo: a host and port with no scheme saves fine and then
        // fails to build anything.
        let mut s = Settings {
            serverUrl: QString::from("10.77.0.1:8083"),
            apiKey: QString::from("k"),
            ..Settings::default()
        };
        s.test_connection_with(&paths);
        let (ok, message) = s.local_notice.clone().expect("a reason");
        assert!(!ok);
        assert!(
            message.contains("scheme"),
            "the message must name what is missing: {message}"
        );
    }

    /// §the settings screen shows what is actually stored.
    ///
    /// `load_from` had exactly one caller, `attach`, and `attach` had NO
    /// production callers -- so the page never read the account file. Every
    /// visit showed a blank server address and a blank API key however many
    /// times they had been saved, and every other control showed a Rust
    /// default: Images in particular showed "Never load", because `i32::
    /// default()` is 0 and the `MEDIA_ASK` default only happens inside the
    /// load that was not running. `reload` is what QML now calls.
    #[test]
    fn the_page_shows_the_stored_account_rather_than_rust_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);

        let mut written = Settings {
            serverUrl: QString::from("http://10.77.0.1:8083/"),
            apiKey: QString::from("secretkey"),
            mediaPolicy: MEDIA_ALLOW,
            syncIntervalIndex: 3,
            wifiOnly: true,
            ..Settings::default()
        };
        written.save_to(&paths);

        // A second visit to the page, as a fresh QML-constructed object.
        let mut reopened = Settings::default();
        assert_eq!(
            reopened.serverUrl.to_string(),
            "",
            "a QML-constructed page starts empty; that is the premise"
        );
        reopened.load_from(&paths);

        assert_eq!(reopened.serverUrl.to_string(), "http://10.77.0.1:8083/");
        assert_eq!(reopened.apiKey.to_string(), "secretkey");
        assert_eq!(reopened.mediaPolicy, MEDIA_ALLOW);
        assert_eq!(reopened.syncIntervalIndex, 3);
        assert!(reopened.wifiOnly);
        assert!(
            !reopened.useCustomCa,
            "and nothing turns the CA switch on by itself"
        );
    }

    #[test]
    fn every_mark_read_choice_maps_to_a_delay_and_nonsense_never_marks() {
        // "Never" is index 0 on purpose: `i32::default()` is 0, so a wiring
        // mistake has to degrade to the feature doing nothing rather than to
        // every glanced-at article being marked read and pushed to the server.
        assert_eq!(mark_read_delay_seconds(MARK_READ_NEVER), None);
        assert_eq!(mark_read_delay_seconds(MARK_READ_IMMEDIATELY), Some(0));
        assert_eq!(mark_read_delay_seconds(2), Some(5));
        assert_eq!(mark_read_delay_seconds(3), Some(15));
        assert_eq!(mark_read_delay_seconds(4), Some(30));

        // QML can put any integer in an int property, and an account file can
        // be hand-edited. Every unrecognised value falls the safe way.
        for index in [-1, 5, 99, i32::MAX, i32::MIN] {
            assert_eq!(mark_read_delay_seconds(index), None, "index {index}");
        }

        // And the shipped default is one of the delayed choices, not "never"
        // and not "immediately" -- a mis-tap must be recoverable.
        assert_eq!(mark_read_delay_seconds(MARK_READ_DEFAULT_INDEX), Some(5));
    }

    #[test]
    fn manual_only_means_no_timer() {
        let mut s = Settings {
            syncIntervalIndex: 0,
            ..Settings::default()
        };
        assert_eq!(s.sync_interval_minutes(), None);
        s.syncIntervalIndex = 3;
        assert_eq!(s.sync_interval_minutes(), Some(60));
    }

    #[test]
    fn an_out_of_range_interval_index_does_not_panic() {
        // QML can set any integer on an int property.
        let mut s = Settings::default();
        for index in [-1, 99, i32::MAX, i32::MIN] {
            s.syncIntervalIndex = index;
            let _ = s.sync_interval_minutes();
        }
    }

    /// §the Sync interval setting, from the picker to the worker.
    ///
    /// `sync_interval_minutes` once had ZERO production callers: the user's
    /// choice was rendered, persisted to the account file and read back on
    /// the next launch, and never reached anything. The context now reads
    /// the stored index through `sync_interval_minutes_for` when it builds
    /// the worker, so the two must agree on what every index means.
    #[test]
    fn every_sync_interval_choice_means_the_same_minutes_to_the_worker() {
        // The picker's own indices, so a reordering of SYNC_INTERVALS_MINUTES
        // has to come through here.
        for (index, expected) in [
            (0, None),
            (1, Some(15)),
            (2, Some(30)),
            (3, Some(60)),
            (4, Some(360)),
        ] {
            let s = Settings {
                syncIntervalIndex: index,
                ..Settings::default()
            };
            assert_eq!(s.sync_interval_minutes(), expected, "index {index}");
            assert_eq!(
                sync_interval_minutes_for(index),
                expected,
                "the context must read index {index} the same way"
            );
        }
    }

    #[test]
    fn media_policy_maps_every_index_including_nonsense() {
        use vuo_core::content::{MediaPolicy, UnproxiedMedia};
        let instance = url::Url::parse("https://h.example/").unwrap();
        let mut s = Settings::default();
        for (index, expected) in [
            (MEDIA_STRICT, UnproxiedMedia::Strict),
            (MEDIA_ASK, UnproxiedMedia::Ask),
            (MEDIA_ALLOW, UnproxiedMedia::Allow),
            (99, UnproxiedMedia::Ask),
        ] {
            s.mediaPolicy = index;
            let MediaPolicy::ProxyThroughInstance { fallback, .. } =
                s.media_policy_for(instance.clone())
            else {
                panic!("expected a proxying policy")
            };
            assert_eq!(fallback, expected, "index {index}");
        }
    }
}
