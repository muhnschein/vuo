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

#[derive(QObject, Default)]
pub struct Settings {
    base: qt_base_class!(trait QObject),

    serverUrl: qt_property!(QString; NOTIFY changed),
    apiKey: qt_property!(QString; NOTIFY changed),
    /// 0 strict, 1 ask (default), 2 allow. See [`MEDIA_ASK`].
    mediaPolicy: qt_property!(i32; NOTIFY changed),
    syncIntervalIndex: qt_property!(i32; NOTIFY changed),
    wifiOnly: qt_property!(bool; NOTIFY changed),
    useCustomCa: qt_property!(bool; NOTIFY changed),
    /// How many local changes are still waiting to reach the server. Shown so
    /// the user can tell "nothing happened" from "not sent yet".
    pendingActions: qt_property!(i32; READ pendingActionsCount NOTIFY changed),

    changed: qt_signal!(),
    /// Result of a connection test. `ok` false means the message is an error.
    connectionTested: qt_signal!(ok: bool, message: QString),

    save: qt_method!(fn(&mut self)),
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
            return;
        }
        // First run: default to Ask rather than Strict, because on a stock
        // Miniflux most images are un-proxied and Strict would blank them.
        self.mediaPolicy = MEDIA_ASK;
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

    fn save(&mut self) {
        let Some(paths) = AppPaths::resolve() else {
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
        };
        if account.server_url.is_empty() || account.token.is_empty() {
            return;
        }
        // Written with mode 0600; see worker::save_account.
        let _ = worker::save_account(&paths.account, &account);
        // And publish it to the live context, or the open article keeps the
        // policy it was built with. This is the hop that was missing: the
        // control was rendered, persisted, and read back on the next launch,
        // but never reached the transform.
        if let Some(ctx) = self.ctx.clone().or_else(crate::context::current) {
            ctx.set_media_policy(self.mediaPolicy);
        }
        // And make the Sync interval choice take effect. Until this call the
        // setting was rendered, persisted and read back and reached NOTHING:
        // the timer unit's hardcoded OnUnitActiveSec=30min governed every
        // device regardless of what the user picked.
        self.apply_sync_interval(paths);
        self.changed();
    }

    fn testConnection(&mut self) {
        self.save();
        if let Some(ctx) = self.ctx.clone().or_else(crate::context::current) {
            ctx.send(Command::TestConnection);
        }
    }

    fn pollNotice(&mut self) -> bool {
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

    /// The systemd drop-in that makes the chosen interval take effect.
    ///
    /// `None` (the "Manual only" choice) produces a drop-in that disables the
    /// periodic run rather than deleting the file: an absent drop-in means
    /// "whatever the package shipped", which is a 30-minute timer, and silently
    /// syncing every 30 minutes is the opposite of what the user asked for.
    ///
    /// Pure, so the mapping is testable without a systemd on the box; the
    /// writer below is the only part that touches the filesystem.
    #[must_use]
    pub fn timer_dropin(minutes: Option<i64>) -> String {
        // Built line by line rather than as one continued literal: a `\`
        // continuation inside a format! string is easy to lose to a reformat,
        // and losing it silently indents every line of a systemd unit.
        let Some(m) = minutes else {
            return [
                "# Written by Vuo: the Sync interval setting is \"Manual only\".",
                "# The timer stays installed but never fires on its own; the",
                "# user syncs with the pulley menu.",
                "[Timer]",
                "OnBootSec=",
                "OnUnitActiveSec=",
                "",
            ]
            .join("\n");
        };
        // Proportional jitter: a 15-minute interval must not carry the unit's
        // fixed 5-minute spread, and a 6-hourly one deserves more.
        let jitter = (m / 6).clamp(1, 30);
        [
            "# Written by Vuo from the Sync interval setting. Edits are".to_owned(),
            "# overwritten the next time that setting is saved.".to_owned(),
            "[Timer]".to_owned(),
            "# The unit ships a default; clear it before setting ours, or".to_owned(),
            "# systemd keeps BOTH and fires on the shorter one.".to_owned(),
            "OnUnitActiveSec=".to_owned(),
            format!("OnUnitActiveSec={m}min"),
            "RandomizedDelaySec=".to_owned(),
            format!("RandomizedDelaySec={jitter}min"),
            String::new(),
        ]
        .join("\n")
    }

    /// Write the drop-in and ask systemd to pick it up.
    ///
    /// Best-effort: on a desktop with no user systemd this does nothing and
    /// says so in the log rather than failing the save. The setting is still
    /// persisted either way.
    fn apply_sync_interval(&self, paths: &AppPaths) {
        let body = Self::timer_dropin(self.sync_interval_minutes());
        let dir = &paths.timer_dropin_dir;
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, dir = %dir.display(), "could not create the timer drop-in directory");
            return;
        }
        let file = dir.join("50-vuo-interval.conf");
        if let Err(e) = std::fs::write(&file, body) {
            tracing::warn!(error = %e, file = %file.display(), "could not write the timer drop-in");
            return;
        }
        // `daemon-reload` then `restart`: a reload alone leaves the running
        // timer on its old schedule until it next fires.
        //
        // On a detached thread, because `save` is called from QML and therefore
        // runs on the Qt UI thread. `Command::status()` blocks until the child
        // exits, and `daemon-reload` on a phone is not instant -- waiting for
        // two of them would freeze the Settings page for as long as systemd
        // takes. Nothing here needs the exit status; the drop-in is already on
        // disk and applies at the next login even if this never runs.
        std::thread::spawn(|| {
            for args in [
                ["--user", "daemon-reload"].as_slice(),
                ["--user", "restart", "harbour-vuo-sync.timer"].as_slice(),
            ] {
                match std::process::Command::new("systemctl").args(args).status() {
                    Ok(status) if status.success() => {}
                    Ok(status) => {
                        tracing::info!(?args, %status, "systemctl declined; the drop-in applies at next login");
                    }
                    Err(e) => {
                        tracing::info!(error = %e, "no systemctl here; the drop-in applies wherever one runs");
                        return;
                    }
                }
            }
        });
    }

    /// The chosen sync interval in minutes, or `None` for "Manual only".
    pub fn sync_interval_minutes(&self) -> Option<i64> {
        let index = usize::try_from(self.syncIntervalIndex).unwrap_or(0);
        match SYNC_INTERVALS_MINUTES.get(index) {
            Some(0) | None => None,
            Some(minutes) => Some(*minutes),
        }
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

    /// §the Sync interval setting, from the picker to systemd.
    ///
    /// `sync_interval_minutes` had ZERO production callers: the user's choice
    /// was rendered, persisted to the account file and read back on the next
    /// launch, and never reached anything. Every device ran the timer unit's
    /// hardcoded `OnUnitActiveSec=30min` no matter what was picked -- the same
    /// defect shape as the Images setting.
    #[test]
    fn every_sync_interval_choice_produces_a_drop_in_that_says_what_it_means() {
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
        }

        // "Manual only" must actively stop the timer. An EMPTY drop-in would
        // leave the packaged 30-minute default in force, which is the one
        // outcome the user explicitly did not ask for.
        let manual = Settings::timer_dropin(None);
        assert!(manual.contains("OnUnitActiveSec="));
        assert!(
            !manual.contains("OnUnitActiveSec=30min"),
            "manual-only must not leave a periodic run: {manual}"
        );

        // A chosen interval must CLEAR the shipped value before setting its
        // own: systemd accumulates OnUnitActiveSec= across drop-ins and fires
        // on the shortest, so appending 360min to a shipped 30min still syncs
        // every 30 minutes.
        let six_hourly = Settings::timer_dropin(Some(360));
        let clear = six_hourly
            .find("OnUnitActiveSec=\n")
            .expect("a clearing line");
        let set = six_hourly
            .find("OnUnitActiveSec=360min")
            .expect("our value");
        assert!(
            clear < set,
            "the clearing line must come FIRST, or systemd keeps both and takes \
             the shorter one:\n{six_hourly}"
        );

        // Jitter is proportional, not the unit's fixed 5 minutes.
        assert!(Settings::timer_dropin(Some(15)).contains("RandomizedDelaySec=2min"));
        assert!(Settings::timer_dropin(Some(360)).contains("RandomizedDelaySec=30min"));

        // No line may be indented. This is not fussiness: the first version of
        // timer_dropin was one `\`-continued literal, rustfmt collapsed it, and
        // every line silently gained seventeen spaces. Every `contains` check
        // above still passed, because `contains` does not care what precedes
        // the match -- so the unit file was wrong and the tests were green.
        for minutes in [None, Some(15), Some(360)] {
            for line in Settings::timer_dropin(minutes).lines() {
                assert_eq!(
                    line,
                    line.trim_start(),
                    "a systemd unit line must not be indented: {line:?}"
                );
            }
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
