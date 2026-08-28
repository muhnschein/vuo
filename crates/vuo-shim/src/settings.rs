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
        if let Ok(account) = worker::load_account(&paths.account) {
            self.serverUrl = QString::from(account.server_url);
            // The key is loaded so the field is not blank when the user opens
            // settings to change something else. It is displayed with
            // echoMode: Password.
            self.apiKey = QString::from(account.token);
        }
        if self.mediaPolicy == 0 && self.serverUrl.to_string().is_empty() {
            // First run: default to Ask rather than Strict, because on a stock
            // Miniflux most images are un-proxied and Strict would blank them.
            self.mediaPolicy = MEDIA_ASK;
        }
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
        let account = Account {
            server_url: self.serverUrl.to_string().trim().to_owned(),
            token: self.apiKey.to_string().trim().to_owned(),
        };
        if account.server_url.is_empty() || account.token.is_empty() {
            return;
        }
        // Written with mode 0600; see worker::save_account.
        let _ = worker::save_account(&paths.account, &account);
        self.changed();
    }

    fn testConnection(&mut self) {
        self.save();
        if let Some(ctx) = self.ctx.clone().or_else(crate::context::current) {
            ctx.send(Command::TestConnection);
        }
    }

    /// Called from the event pump when the worker answers.
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

    /// The configured refresh interval, or `None` for manual-only.
    #[must_use]
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

    #[test]
    fn the_default_media_policy_is_ask_not_strict() {
        // On a stock Miniflux MEDIA_PROXY_MODE is http-only, so most images
        // arrive un-proxied. Strict by default would blank most articles.
        let mut s = Settings::default();
        s.load();
        assert_eq!(s.mediaPolicy, MEDIA_ASK);
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
