//! Settings screen's backing object.
//!
//! Holds account and few behaviour-changing preferences. §7: *the API key is
//! stored under the app's data directory with restrictive permissions, relying
//! on Sailfish's home encryption. No custom keyring, no `SQLCipher`, unless a
//! concrete threat model justifies it.*
//!
//! Token not in SQLite mirror on purpose: mirror = cache that gets deleted,
//! copied for bug report, inspected by developer; credential would travel with
//! it.

// Fires inside code `QObject` derive generates; struct-level attribute doesn't
// reach expanded impls. §9.5 confines unsafe to this crate for this:
// qmetaobject glue does pointer work clippy reads as suspicious alone. Only
// `Settings` trips it; list models use `qt_base_class!(trait
// QAbstractListModel)`, expands differently.
#![allow(clippy::useless_transmute)]

use qmetaobject::{qt_base_class, qt_method, qt_property, qt_signal, QObject, QString};

use crate::context::AppContext;
use crate::worker::{self, Account, AppPaths, Command};

/// How un-proxied third-party media treated. Integer to QML: this `qmetaobject`
/// has no `qml_register_enum` on Qt 5.6.
pub const MEDIA_STRICT: i32 = 0;
/// Un-proxied media waits behind a tap-to-load placeholder. The default.
pub const MEDIA_ASK: i32 = 1;
/// Un-proxied media loads without asking.
pub const MEDIA_ALLOW: i32 = 2;

/// Background refresh intervals, in minutes. Index 0 is "manual only".
pub const SYNC_INTERVALS_MINUTES: [i64; 5] = [0, 15, 30, 60, 360];

/// New install syncs hourly.
///
/// Not index 0: index 0 = "Manual only" = `i32::default()`, a reader that never
/// refreshes until user finds pulley (behaviour feed reader must not ship
/// with). Hourly: often enough list is worth opening, rare enough to be
/// invisible on phone battery and self-hosted server.
pub const SYNC_INTERVAL_DEFAULT_INDEX: i32 = 3;

/// When opened article marked read. Index 0 = "never".
///
/// "Never" at index 0 on purpose: `i32::default()` is 0, so future wiring
/// mistake degrades to feature doing nothing, not every glanced article marked
/// read and pushed to server. Safe polarity for destructive default.
pub const MARK_READ_NEVER: i32 = 0;
/// Mark read as soon as the article opens.
pub const MARK_READ_IMMEDIATELY: i32 = 1;
/// Seconds for the delayed choices, from index 2 onward.
pub const MARK_READ_DELAYS_SECONDS: [i32; 3] = [5, 15, 30];
/// New install marks read after 5 seconds: absorbs mis-tap and headline glance;
/// read article doesn't linger in unread list.
pub const MARK_READ_DEFAULT_INDEX: i32 = 2;

/// How long read, unfavourited article kept in local mirror, days.
///
/// Index 0 = "keep everything", same reason as [`MARK_READ_NEVER`]:
/// `i32::default()` is 0, so wiring mistake, hand-edited account file, or
/// account predating setting degrades to deleting nothing. Destructive
/// default's safe polarity destroys nothing.
pub const RETENTION_DAYS: [i64; 5] = [0, 30, 90, 180, 365];

/// New install keeps everything.
///
/// Not recommendation (a month suits most), but mirror is reader's; update
/// starting to delete articles because author thought so = surprise, not
/// setting.
pub const RETENTION_DEFAULT_INDEX: i32 = 0;

/// Retention window stored `retention_index` means, seconds.
///
/// `None` for "keep everything" and for unknown index: same conservative
/// direction.
#[must_use]
pub fn retention_seconds_for(index: i32) -> Option<i64> {
    usize::try_from(index)
        .ok()
        .and_then(|i| RETENTION_DAYS.get(i))
        .copied()
        .filter(|days| *days > 0)
        .and_then(|days| days.checked_mul(24 * 60 * 60))
}

/// How long article must be open before counting read.
///
/// `None` never, `Some(0)` immediately, `Some(n)` after n seconds. Index made
/// up by hand-edited account file → `None`: same conservative direction.
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

/// Settings screen's backing object; see module docs.
// Bools are `qt_property!`s, each own setting/state QML binds by name; bitfield
// hides them from QML.
#[allow(clippy::struct_excessive_bools)]
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
    /// How long read articles are kept locally. See [`RETENTION_DAYS`].
    retentionIndex: qt_property!(i32; NOTIFY changed),
    wifiOnly: qt_property!(bool; NOTIFY changed),
    /// Raise notification when sync finds new articles while Vuo on cover. Read
    /// by root window, not worker: worker only writes mirror; notification is
    /// UI's to raise.
    notifyNewArticles: qt_property!(bool; NOTIFY changed),
    /// How often root window polls mirror while app NOT active, ms; 0 = don't
    /// poll.
    ///
    /// Derived from sync interval: only thing changing cover while away =
    /// automatic sync finishing. See [`cover_poll_ms_for`].
    coverPollMs: qt_property!(i32; READ cover_poll_ms NOTIFY changed),
    useCustomCa: qt_property!(bool; NOTIFY changed),
    /// Local changes still waiting to reach server. Lets user tell "nothing
    /// happened" from "not sent yet".
    pendingActions: qt_property!(i32; READ pendingActionsCount NOTIFY changed),
    /// Whether account stored at all (server and key). Root window asks on
    /// start-up to pick entry list vs onboarding; onboarding asks again
    /// returning from Settings. Read from file each time: right before anything
    /// called `reload`.
    configured: qt_property!(bool; READ is_configured NOTIFY changed),

    changed: qt_signal!(),
    /// Result of a connection test. `ok` false means the message is an error.
    connectionTested: qt_signal!(ok: bool, message: QString),

    save: qt_method!(fn(&mut self)),
    /// Read stored account into properties.
    ///
    /// QML calls this on page open. Without it page shows blank server and key
    /// however often saved, and Rust defaults for other controls (Images "Never
    /// load": `i32::default()` is 0; `MEDIA_ASK` default only set inside load).
    reload: qt_method!(fn(&mut self)),
    testConnection: qt_method!(fn(&mut self)),
    /// Drain worker's pending result, fire [`connectionTested`].
    ///
    /// Poll, same reason as `EntryModel::pollSync`: QML owns object, Rust has
    /// no handle to signal into. Without it worker's answer reaches only log;
    /// "Test connection" gives no feedback either way.
    pollNotice: qt_method!(fn(&mut self) -> bool),

    ctx: Option<std::rc::Rc<AppContext>>,

    /// Result this page produced itself, no worker: "nothing to test yet",
    /// "address not URL", "account couldn't be written".
    ///
    /// Waits in one-shot slot, drained by same poll as worker answers, not
    /// fired from inside tap. QML starts poll *after* `testConnection()`
    /// returns: signal during call would be missed, poll runs forever for
    /// answer already given.
    local_notice: Option<(bool, String)>,
}

impl Settings {
    /// Give object context explicitly, load stored account. Tests only; QML
    /// calls `reload`.
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

    /// `Settings::load` against explicit paths.
    ///
    /// Split so screen's read path testable. No-arg version resolves
    /// `$XDG_DATA_HOME`/`$HOME`: test calling it reads developer's real home
    /// (configured Vuo → loaded branch, passes with default flipped).
    pub fn load_from(&mut self, paths: &AppPaths) {
        // First run gets `Account::default()`: one home of out-of-box settings,
        // same values serde fills for fields older account file lacks. No
        // separate default list here: two lists drift (e.g. first run left at
        // `i32::default()` "Manual only" while missing field got real default).
        let account = worker::load_account(&paths.account).unwrap_or_default();
        self.serverUrl = QString::from(account.server_url);
        // Key loaded so field isn't blank when user changes something else.
        // Displayed with echoMode: Password.
        self.apiKey = QString::from(account.token);
        self.useCustomCa = account.use_custom_ca;
        self.mediaPolicy = account.media_policy;
        self.syncIntervalIndex = account.sync_interval_index;
        self.wifiOnly = account.wifi_only;
        self.markReadDelayIndex = account.mark_read_delay_index;
        self.retentionIndex = account.retention_index;
        self.notifyNewArticles = account.notify_new_articles;
    }

    // Method: QML calls it on object. Reads account file, not object, so fresh
    // setup seen without reload.
    #[allow(clippy::unused_self)]
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
            .map_or(0, |n| i32::try_from(n).unwrap_or(i32::MAX))
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

    /// `Settings::save` against explicit paths.
    ///
    /// Copy from QML-visible properties into `Account` lives here: tests drive
    /// real one. Hand-built `Account` round trip tests serde derive, not
    /// settings screen.
    pub fn save_to(&mut self, paths: &AppPaths) {
        self.store(paths);
        // Last, nothing after. See `store`.
        self.changed();
    }

    /// Write account, make running app match. Emits nothing.
    ///
    /// Split from [`Settings::save_to`] so `changed()` emitted ONCE, by
    /// outermost QML-invoked method, as final act.
    ///
    /// Not tidiness. `qmetaobject` dispatches every QML call by building fresh
    /// `&mut Self` from raw pointer (deliberately, so re-entrant calls work):
    /// emitting mid-method hands QML object Rust still holds exclusively. Every
    /// binding reading `settings.pendingActions` (and `configured`, flipping on
    /// first save) re-evaluates inside emit, each a second live `&mut Settings`
    /// over same memory. Emitting then returning into `test_connection_with`
    /// (reads/writes `self`) would stretch aliasing over rest of tap, under
    /// `lto = true, opt-level = "z"`, where compiler assumes it can't happen.
    fn store(&mut self, paths: &AppPaths) {
        tracing::info!("saving the account");
        let account = Account {
            server_url: self.serverUrl.to_string().trim().to_owned(),
            token: self.apiKey.to_string().trim().to_owned(),
            use_custom_ca: self.useCustomCa,
            media_policy: self.mediaPolicy,
            sync_interval_index: self.syncIntervalIndex,
            wifi_only: self.wifiOnly,
            mark_read_delay_index: self.markReadDelayIndex,
            retention_index: self.retentionIndex,
            notify_new_articles: self.notifyNewArticles,
        };
        if account.server_url.is_empty() || account.token.is_empty() {
            return;
        }
        // Mode 0600; see worker::save_account.
        //
        // Result can't be discarded: everything below assumes file says what
        // screen says; failed write → context rebuild returns PREVIOUS account,
        // answers confidently about it.
        if let Err(e) = worker::save_account(&paths.account, &account) {
            tracing::warn!(error = %e, "could not write the account file");
            self.local_notice = Some((false, e.to_string()));
            return;
        }
        tracing::info!("the account file is written");

        // Make running app match what was written.
        //
        // First-run hop: context (mirror, worker thread) needs account file
        // that doesn't exist until this runs once. Without refresh here, first
        // server and key entry leaves no worker: "Test connection" and pulley
        // Refresh silently do nothing until restart.
        //
        // Every later save too: worker captures server URL and API key at
        // spawn; without this, test after editing address answers for replaced
        // address. `refresh` no-op when neither changed.
        if let Err(e) = crate::context::refresh(paths) {
            tracing::warn!(error = %e, "the saved account did not yield a usable context");
            self.local_notice = Some((false, e.to_string()));
        }

        // Publish media policy to live context, else open article keeps policy
        // it was built with. Control must reach transform, not just persist.
        if let Some(ctx) = self.ctx.clone().or_else(crate::context::current) {
            ctx.set_media_policy(self.mediaPolicy);
            ctx.set_mark_read_delay_index(self.markReadDelayIndex);
            // Interval, to worker keeping it. Context rebuilt above told at
            // build; one surviving save (same server, key) hears it here.
            ctx.send(Command::SetSyncInterval {
                minutes: sync_interval_minutes_for(self.syncIntervalIndex),
            });
            // Retention window, applied by worker at end of next pass. Same
            // hop, same reason.
            ctx.send(Command::SetRetention {
                seconds: retention_seconds_for(self.retentionIndex),
            });
            // "Only sync on Wi-Fi", which worker checks against network it
            // reads when its work falls due.
            ctx.send(Command::SetWifiOnly {
                enabled: self.wifiOnly,
            });
        }
        tracing::info!("the settings screen has published the saved account");
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

    /// `Settings::testConnection` against explicit paths.
    ///
    /// Split, same reason as `save_to`: no-arg version resolves
    /// `$XDG_DATA_HOME`/`$HOME`; test would read and write developer's real
    /// account.
    ///
    /// Every branch leaves user something to read (no context, stopped worker,
    /// failed send all answer). "Nothing happened" = one answer Test button
    /// must never give.
    pub fn test_connection_with(&mut self, paths: &AppPaths) {
        // Each tap starts fresh. Leftover answer from previous tap (not yet
        // drained; page polls every 400ms) would read as this tap's,
        // short-circuiting check below.
        self.local_notice = None;
        tracing::info!("testing the connection");
        // `store`, not `save_to`: emit belongs at end of this method. See
        // `store`.
        self.store(paths);
        self.ask_the_worker();
        // Last, nothing after. Line below reads no `self`.
        self.changed();
        tracing::info!("the settings screen has finished the test");
    }

    /// Send the test, or record why it could not be sent. Emits nothing.
    fn ask_the_worker(&mut self) {
        // `store` refreshes context: one exists here unless account couldn't be
        // written or isn't usable config; then it left reason here, better than
        // anything added.
        if self.local_notice.is_some() {
            return;
        }

        let Some(ctx) = self.ctx.clone().or_else(crate::context::current) else {
            // `store` returns early without writing when either field blank:
            // empty form reaches this.
            self.local_notice = Some((
                false,
                "fill in the server address and the API key first".to_owned(),
            ));
            return;
        };

        if ctx.send(Command::TestConnection) {
            tracing::info!("the test is queued for the worker");
        } else {
            // Worker stops itself if mirror or HTTP client couldn't be built;
            // send into closed channel silent.
            self.local_notice = Some((
                false,
                "the sync worker is not running; restart Vuo".to_owned(),
            ));
        }
    }

    fn pollNotice(&mut self) -> bool {
        // Page's own result first, checked before context: exists for cases
        // where request never reached worker; worst of those has no context to
        // poll.
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

    /// `Settings::pollNotice` against explicit signal: worker → page round trip
    /// testable without QML engine or context.
    pub fn poll_notice_from(&mut self, signal: &crate::context::SyncSignal) -> bool {
        match signal.take_notice() {
            Some(crate::context::Notice::ConnectionTested { ok, message }) => {
                self.on_connection_tested(ok, &message);
                true
            }
            // Not this page's; put back for page that cares.
            Some(other) => {
                signal.post(other);
                false
            }
            None => false,
        }
    }

    /// Fire the QML-visible signal. Reached through `Settings::pollNotice`.
    pub fn on_connection_tested(&mut self, ok: bool, message: &str) {
        self.connectionTested(ok, QString::from(message.to_owned()));
    }

    /// The chosen sync interval in minutes, or `None` for "Manual only".
    #[must_use]
    pub fn sync_interval_minutes(&self) -> Option<i64> {
        sync_interval_minutes_for(self.syncIntervalIndex)
    }

    fn cover_poll_ms(&self) -> i32 {
        cover_poll_ms_for(self.syncIntervalIndex)
    }
}

/// Quarter of sync interval, ms; 0 for "Manual only".
///
/// # What it's for
///
/// Root window's poll = how models learn worker wrote mirror. Fixed 1.5 s poll
/// on minimised app = forty wakeups a minute for hourly event. Stopping
/// outright freezes cover (only thing minimised Vuo still shows): sync
/// finishing while away leaves unread count stale until app opens.
///
/// So poll slows to cadence of only event it could see. Quarter of interval
/// bounds cover staleness at quarter of sync gap, same proportion at every
/// setting:
///
/// | Sync           | Poll         | Wakeups an hour |
/// | -------------- | ------------ | --------------- |
/// | Manual only    | never        | 0               |
/// | Every 15 min   | 3 min 45 s   | 16              |
/// | Every 30 min   | 7 min 30 s   | 8               |
/// | Hourly         | 15 min       | 4               |
/// | Every 6 hours  | 90 min       | 0.67            |
///
/// Against 2,400/hour at 1.5 s, worst of those = 150-fold cut.
///
/// **Manual only polls not at all.** Nothing syncs alone; nothing for tick to
/// find. Refresh from cover's own action not covered and needn't be: root
/// window keeps fast poll for that one sync; see `harbour-vuo.qml`.
#[must_use]
pub fn cover_poll_ms_for(index: i32) -> i32 {
    let Some(minutes) = sync_interval_minutes_for(index) else {
        return 0;
    };
    // Saturating throughout: `index` from stored file; quarter of six hours =
    // 5.4M ms (fits i32), but mustn't depend on table never growing.
    minutes
        .saturating_mul(60_000)
        .checked_div(4)
        .unwrap_or(0)
        .try_into()
        .unwrap_or(i32::MAX)
}

/// Sync interval stored `sync_interval_index` means, minutes, or `None` for
/// "Manual only". Shared with context, which reads index from account file when
/// building worker, has no `Settings`.
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

    /// §what a new install is set to before anyone has chosen anything.
    ///
    /// Two of these separate working-out-of-box from looking broken: never
    /// refreshing alone, and blanking every image. Both index 0 of their lists,
    /// index 0 = `i32::default()`: unapplied default doesn't look wrong, looks
    /// like first entry.
    #[test]
    fn a_first_run_is_set_up_to_work_before_anyone_configures_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);

        // Settings screen's read, no account file.
        let mut s = Settings::default();
        s.load_from(&paths);
        assert_eq!(s.mediaPolicy, MEDIA_ASK, "images: ask each site");
        assert_eq!(
            sync_interval_minutes_for(s.syncIntervalIndex),
            Some(60),
            "sync: hourly"
        );
        assert_eq!(s.markReadDelayIndex, MARK_READ_DEFAULT_INDEX);
        assert!(
            !s.notifyNewArticles,
            "notifications: off until the reader asks for them"
        );

        // What setup dialog stores. Shows none of these controls: account
        // written carries whatever object holds (before `reload`, zero for
        // all).
        //
        // Discard port, not unroutable address: saving builds real worker,
        // hourly means sync due. Port 9 refuses at once; 10.77.0.1 holds thread
        // for connect timeout, binary can't exit until released.
        s.serverUrl = QString::from("http://127.0.0.1:9/");
        s.apiKey = QString::from("k");
        s.save_to(&paths);
        let stored = worker::load_account(&paths.account).expect("the account file");
        assert_eq!(stored.media_policy, MEDIA_ASK);
        assert_eq!(stored.sync_interval_index, SYNC_INTERVAL_DEFAULT_INDEX);
        assert_eq!(stored.mark_read_delay_index, MARK_READ_DEFAULT_INDEX);
        assert!(!stored.notify_new_articles);
    }

    /// QML-invoked method emits signal LAST, touches `self` no more.
    ///
    /// `qmetaobject` dispatches every QML call by building fresh `&mut Self`
    /// from raw pointer, on purpose (generated code carries FIXME: skips
    /// `RefCell::borrow_mut` so re-entrant calls work). Emitted signal runs QML
    /// synchronously; every binding reading THIS object's property returns
    /// through dispatch as second live `&mut Settings` over same memory.
    /// Aliasing compiler may assume away; device package built `lto = true,
    /// opt-level = "z"`, where it will.
    ///
    /// Emitting still needed (how QML hears anything): rule = emit once, at
    /// end, done with `self`.
    ///
    /// Read from source: nothing at runtime to assert on.
    #[test]
    fn a_method_that_emits_is_finished_with_itself() {
        const SOURCE: &str = include_str!("settings.rs");
        const EMIT: &str = "self.changed()";

        // Methods QML enters. Each emits exactly once, last.
        for signature in ["fn save_to(", "fn test_connection_with(", "fn reload("] {
            let body = method_body(SOURCE, signature);
            let emit = body
                .rfind(EMIT)
                .unwrap_or_else(|| panic!("{signature} no longer emits `changed`"));
            let after = &body[emit + EMIT.len()..];
            assert!(
                !after.contains("self."),
                "{signature} goes on using `self` after emitting:{after}"
            );
        }

        // Helpers they're split into emit nothing, else split bought nothing.
        for signature in ["fn store(", "fn ask_the_worker("] {
            let body = method_body(SOURCE, signature);
            assert!(
                !body.contains(EMIT),
                "{signature} exists so that the emit can be someone else's last act"
            );
        }
    }

    /// The text of one method, from its signature to its closing brace.
    fn method_body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} is gone; the rule it carries is not"));
        let rest = &source[start..];
        let end = rest
            .find("\n    }\n")
            .unwrap_or_else(|| panic!("{signature} has no closing brace at method indent"));
        &rest[..end]
    }

    fn temp_paths(dir: &tempfile::TempDir) -> AppPaths {
        let base = dir.path().join("harbour-vuo");
        std::fs::create_dir_all(&base).expect("mkdir");
        AppPaths::under(base)
    }

    #[test]
    fn every_setting_survives_a_round_trip() {
        // Images, Background refresh, Wi-Fi-only controls persisted and read
        // back.
        //
        // Round trip through `Settings` itself, not `Account` literal through
        // save_account / load_account (that tests serde derive): dropping
        // `use_custom_ca`, `media_policy`, `sync_interval_index` from
        // `Settings::save`'s copy must fail here.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);

        let mut written = Settings {
            serverUrl: QString::from("https://h.example/"),
            apiKey: QString::from("t"),
            useCustomCa: true,
            mediaPolicy: MEDIA_ALLOW,
            syncIntervalIndex: 3,
            wifiOnly: true,
            notifyNewArticles: true,
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
        assert!(read.notifyNewArticles);
    }

    #[test]
    fn an_account_file_from_an_older_build_still_loads() {
        // New fields optional, else upgrade strands user with unreadable
        // account, no obvious way back.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account.json");
        std::fs::write(&path, r#"{"server_url":"https://h.example/","token":"t"}"#).expect("write");
        let read = worker::load_account(&path).expect("an older account file must still load");
        assert_eq!(read.media_policy, MEDIA_ASK, "and get the safe default");
        assert!(!read.wifi_only);
        assert!(
            !read.notify_new_articles,
            "an upgrade does not start raising notifications by itself"
        );
    }

    #[test]
    fn the_default_media_policy_is_ask_not_strict() {
        // Stock Miniflux MEDIA_PROXY_MODE http-only: most images un-proxied.
        // Strict default would blank most articles.
        //
        // Against EMPTY directory of own. No-arg `load()` reads
        // $XDG_DATA_HOME/$HOME: on developer machine running Vuo, loaded branch
        // passes with default flipped.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);
        assert!(!paths.account.exists(), "this must be a first run");

        let mut s = Settings::default();
        s.load_from(&paths);
        assert_eq!(s.mediaPolicy, MEDIA_ASK);
    }

    #[test]
    fn a_worker_result_reaches_the_settings_page() {
        // Worker answers "test this connection" on own thread; `pollNotice`
        // carries it to `on_connection_tested`, firing `connectionTested` so
        // SettingsPage's result Label shows.
        let signal = std::sync::Arc::new(crate::context::SyncSignal::default());

        let mut s = Settings::default();
        assert!(
            !s.poll_notice_from(&signal),
            "nothing pending: the page must not be told anything"
        );

        // As worker does, from own thread.
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

        // Notice for another page left in place.
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
    /// Context (mirror, worker thread) built in `main` from account file absent
    /// before Settings visited. Must be rebuilt on first save, else every
    /// worker-backed action silently does nothing ("Test connection" no answer,
    /// pulley Refresh no spin) until restart.
    ///
    /// `main` lives in `harbour-vuo`, no tests, `make check` only lints:
    /// covered here.
    #[test]
    fn configuring_an_account_makes_the_app_work_without_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);
        assert!(
            crate::context::current().is_none(),
            "a first run starts with no context; that is the whole premise"
        );

        // Exactly what user types for that setup: plain http to WireGuard
        // address, no TLS.
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

    /// Worker captures server URL and API key at spawn; context outlives
    /// credentials. Editing either and testing again must ask NEW worker, not
    /// old one answering for replaced account.
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

        // Save changing no credential must NOT churn worker: Wi-Fi switch has
        // no business restarting sync in flight.
        s.wifiOnly = true;
        s.save_to(&paths);
        let after_unrelated_save = crate::context::current().expect("a context");
        assert!(
            Rc::ptr_eq(&first, &after_unrelated_save),
            "an unrelated setting must not restart the worker"
        );

        // Changed address must.
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

        // So must changed key (not visible in origin).
        s.apiKey = QString::from("k2");
        s.save_to(&paths);
        let after_new_key = crate::context::current().expect("a context");
        assert!(
            !Rc::ptr_eq(&after_new_server, &after_new_key),
            "a changed API key must not keep answering through the old worker"
        );
    }

    /// "Nothing happened" = one answer Test button must never give.
    #[test]
    fn a_connection_test_that_reaches_no_worker_still_answers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = temp_paths(&dir);

        // Empty form. `save_to` declines to write it, nothing downstream to
        // send through; must still answer, else QML poll spins for answer never
        // coming.
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

        // Address that isn't URL: other way here, likeliest typo. Host:port
        // without scheme saves fine, then fails to build anything.
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
    /// QML calls `reload`, which reads account file: page shows saved server
    /// and key, and stored values for every control, not Rust defaults (Images
    /// not "Never load" from `i32::default()` 0).
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

        // Second visit to page, as fresh QML-constructed object.
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
        // "Never" index 0 on purpose: `i32::default()` is 0; wiring mistake
        // degrades to feature doing nothing, not every glanced article marked
        // read and pushed.
        assert_eq!(mark_read_delay_seconds(MARK_READ_NEVER), None);
        assert_eq!(mark_read_delay_seconds(MARK_READ_IMMEDIATELY), Some(0));
        assert_eq!(mark_read_delay_seconds(2), Some(5));
        assert_eq!(mark_read_delay_seconds(3), Some(15));
        assert_eq!(mark_read_delay_seconds(4), Some(30));

        // QML can put any integer in int property; account file hand-editable.
        // Every unrecognised value falls safe way.
        for index in [-1, 5, 99, i32::MAX, i32::MIN] {
            assert_eq!(mark_read_delay_seconds(index), None, "index {index}");
        }

        // Shipped default is a delayed choice, not "never" or "immediately":
        // mis-tap recoverable.
        assert_eq!(mark_read_delay_seconds(MARK_READ_DEFAULT_INDEX), Some(5));
    }

    /// §retention degrades to keeping everything, never to deleting.
    ///
    /// Only setting here destroying user data: direction wrong answers fall is
    /// property worth asserting. Index 0 = "keep everything" because
    /// `i32::default()` is 0: account predating setting, hand-edited one,
    /// future wiring mistake all land there.
    #[test]
    fn an_unknown_retention_index_keeps_everything() {
        assert_eq!(retention_seconds_for(RETENTION_DEFAULT_INDEX), None);
        assert_eq!(retention_seconds_for(0), None, "keep everything");
        assert_eq!(retention_seconds_for(1), Some(30 * 24 * 60 * 60));
        assert_eq!(retention_seconds_for(4), Some(365 * 24 * 60 * 60));
        assert_eq!(retention_seconds_for(99), None, "past the end of the list");
        assert_eq!(
            retention_seconds_for(-1),
            None,
            "and before the start of it"
        );
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
        // QML can set any integer on int property.
        let mut s = Settings::default();
        for index in [-1, 99, i32::MAX, i32::MIN] {
            s.syncIntervalIndex = index;
            let _ = s.sync_interval_minutes();
        }
    }

    /// §the Sync interval setting, from the picker to the worker.
    ///
    /// Context reads stored index through `sync_interval_minutes_for` when
    /// building worker; `Settings` and context must agree on every index.
    #[test]
    fn every_sync_interval_choice_means_the_same_minutes_to_the_worker() {
        // Picker's own indices: reordering SYNC_INTERVALS_MINUTES must come
        // through here.
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

    /// §the cover's cadence follows the cadence it exists to notice.
    ///
    /// Numbers are the point: poll far slower than sync = cover with
    /// yesterday's count; far faster = the wakeups this removes. Quarter is
    /// neither, at every setting.
    #[test]
    fn the_idle_poll_is_a_quarter_of_the_sync_interval() {
        // Picker's own indices, own order.
        assert_eq!(
            cover_poll_ms_for(0),
            0,
            "Manual only syncs never, so a poll would find nothing"
        );
        assert_eq!(cover_poll_ms_for(1), 15 * 60_000 / 4);
        assert_eq!(cover_poll_ms_for(2), 30 * 60_000 / 4);
        assert_eq!(cover_poll_ms_for(3), 60 * 60_000 / 4);
        assert_eq!(cover_poll_ms_for(4), 360 * 60_000 / 4);
    }

    /// §every choice the picker offers yields a usable Timer interval.
    ///
    /// QML `Timer` takes `int`; zero interval on repeating timer = wakeup per
    /// event-loop pass. Only interval allowed zero is one whose timer isn't
    /// running.
    #[test]
    fn no_sync_choice_produces_a_spinning_timer() {
        for index in 0..i32::try_from(SYNC_INTERVALS_MINUTES.len()).unwrap_or(0) {
            let ms = cover_poll_ms_for(index);
            if sync_interval_minutes_for(index).is_none() {
                assert_eq!(ms, 0, "index {index} is Manual only");
                continue;
            }
            assert!(
                ms >= 60_000,
                "index {index} polls every {ms} ms, which is not an idle cadence"
            );
        }
    }

    /// §an index the table does not have is "do not poll", not a panic.
    ///
    /// From stored file: user-editable, partial write can truncate.
    #[test]
    fn an_impossible_stored_index_does_not_poll() {
        assert_eq!(cover_poll_ms_for(-1), 0);
        assert_eq!(cover_poll_ms_for(99), 0);
        assert_eq!(cover_poll_ms_for(i32::MAX), 0);
    }
}
