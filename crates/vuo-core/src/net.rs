//! What network phone is on; whether automatic sync may use it.
//!
//! # Why kernel, not Qt
//!
//! Obvious answer `QNetworkConfigurationManager` (Qt, Harbour allows
//! `libQt5Network`, reports bearer type). Wrong here, two reasons.
//!
//! **Freshness.** `QObject` lives on Qt thread; answer would be pushed to
//! worker as command. Worker syncs on own cadence *while minimised*, exactly
//! when nothing pushes: reader leaves Wi-Fi, no tap, worker acts on hour-old
//! reading. Reading kernel from worker = answer taken when decision made, only
//! moment worth it.
//!
//! **Verifiability.** §8: `make check` runs what CI runs, no phone. Bearer
//! query can't be exercised that way; two files under a handed directory can
//! (tests below).
//!
//! Also no C++ glue, no `cpp_build`, no new dependency, nothing from Harbour's
//! allowed-libraries list not already linked.
//!
//! # What is read
//!
//! `/proc/net/route` and `/proc/net/ipv6_route` for default route;
//! `/sys/class/net/<iface>/` for whether carrying interface is Wi-Fi.
//! `phy80211` modern marker, `wireless` old one: what `iw` and `NetworkManager`
//! check.
//!
//! # Rule that matters most
//!
//! **Reading that couldn't be taken never blocks sync.** Every failure path
//! (unreadable `/proc`, different kernel layout, unrecognised route) lands on
//! [`Network::Unknown`]; [`Network::allows`] lets `Unknown` through. Worst
//! case: Vuo behaves as without this module. Other failure mode (misread
//! silently stops syncing on good connection) undiagnosable by user; reported
//! only as "it stopped working".

use std::path::Path;

/// The loopback interface, which is never a way to reach anything.
const LOOPBACK: &str = "lo";

/// `RTF_UP` in `/proc/net/route`'s flags column.
const RTF_UP: u32 = 0x1;

/// What the phone's default route runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    /// A default route over an interface the kernel calls wireless.
    Wifi,
    /// Default route over something else. Cellular on phone: maybe paid per
    /// byte.
    Metered,
    /// Route files read, no default route. Sync would spend DNS lookup and
    /// connect timeout discovering this.
    Offline,
    /// Could not be determined. Never blocks anything; see the module docs.
    Unknown,
}

impl Network {
    /// Whether automatic sync may run over this.
    ///
    /// `wifi_only` = user's "Only sync on Wi-Fi". Promise about which network
    /// Vuo uses, taken literally: metered → nothing automatic goes out, outbox
    /// included. Nothing lost: outbox durable and idempotent, goes on next
    /// Wi-Fi or manual refresh (never gated).
    #[must_use]
    pub fn allows(self, wifi_only: bool) -> bool {
        match self {
            // Unknown: reading failed. Behave as if module absent.
            Network::Unknown | Network::Wifi => true,
            Network::Metered => !wifi_only,
            Network::Offline => false,
        }
    }
}

/// Read the current network from the running kernel.
#[must_use]
pub fn probe() -> Network {
    probe_under(Path::new("/"))
}

/// [`probe`] against an explicit root, so the tests can lay one out.
#[must_use]
pub fn probe_under(root: &Path) -> Network {
    let v4 = std::fs::read_to_string(root.join("proc/net/route"));
    let v6 = std::fs::read_to_string(root.join("proc/net/ipv6_route"));

    // Neither file readable ≠ "offline"; = "doesn't work here".
    if v4.is_err() && v6.is_err() {
        return Network::Unknown;
    }

    let mut ifaces = Vec::new();
    if let Ok(text) = &v4 {
        ifaces.extend(default_route_ifaces_v4(text));
    }
    if let Ok(text) = &v6 {
        ifaces.extend(default_route_ifaces_v6(text));
    }

    if ifaces.is_empty() {
        return Network::Offline;
    }

    // Wi-Fi wins among several. Phone with Wi-Fi and cellular default routes
    // uses Wi-Fi; reporting more permissive can't strand reader.
    if ifaces.iter().any(|iface| is_wireless(root, iface)) {
        Network::Wifi
    } else {
        Network::Metered
    }
}

/// Interfaces carrying IPv4 default route.
///
/// Columns `Iface Destination Gateway Flags RefCnt Use Metric Mask`. Default =
/// destination and mask zero, `RTF_UP` set.
fn default_route_ifaces_v4(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let iface = columns.next()?;
            let destination = columns.next()?;
            let _gateway = columns.next()?;
            let flags = columns.next()?;
            let _refcnt = columns.next()?;
            let _use = columns.next()?;
            let _metric = columns.next()?;
            let mask = columns.next()?;

            let is_default = destination.trim_start_matches('0').is_empty()
                && mask.trim_start_matches('0').is_empty();
            let up = u32::from_str_radix(flags, 16).ok()? & RTF_UP != 0;
            (is_default && up && iface != LOOPBACK).then(|| iface.to_owned())
        })
        .collect()
}

/// Interfaces carrying IPv6 default route.
///
/// Interface = LAST column; first two = destination and prefix length: `::/0` =
/// thirty-two zeros then `00`.
fn default_route_ifaces_v6(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let columns: Vec<&str> = line.split_whitespace().collect();
            let destination = columns.first()?;
            let prefix_length = columns.get(1)?;
            let iface = columns.last()?;

            let is_default = destination.trim_start_matches('0').is_empty()
                && prefix_length.trim_start_matches('0').is_empty();
            (is_default && *iface != LOOPBACK).then(|| (*iface).to_owned())
        })
        .collect()
}

/// Whether kernel calls interface wireless.
///
/// `phy80211` is symlink: asks for link itself, doesn't follow; dangling one
/// still says what interface is.
fn is_wireless(root: &Path, iface: &str) -> bool {
    // Interface name from kernel file but ends up in path: separators must not
    // walk out of /sys/class/net.
    if iface.is_empty() || iface.contains('/') || iface.contains("..") {
        return false;
    }
    let base = root.join("sys/class/net").join(iface);
    ["phy80211", "wireless"]
        .iter()
        .any(|marker| std::fs::symlink_metadata(base.join(marker)).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay out the parts of /proc and /sys this module reads.
    fn fake_root(v4: Option<&str>, v6: Option<&str>, wireless: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let proc = dir.path().join("proc/net");
        std::fs::create_dir_all(&proc).expect("mkdir proc");
        if let Some(text) = v4 {
            std::fs::write(proc.join("route"), text).expect("write route");
        }
        if let Some(text) = v6 {
            std::fs::write(proc.join("ipv6_route"), text).expect("write ipv6_route");
        }
        for iface in wireless {
            let base = dir.path().join("sys/class/net").join(iface);
            std::fs::create_dir_all(base.join("phy80211")).expect("mkdir phy80211");
        }
        dir
    }

    const HEADER: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT";

    #[test]
    fn a_default_route_over_a_wireless_interface_is_wifi() {
        let dir = fake_root(
            Some(&format!(
                "{HEADER}\n\
                 wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                 wlan0\t0002A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n"
            )),
            None,
            &["wlan0"],
        );
        assert_eq!(probe_under(dir.path()), Network::Wifi);
    }

    #[test]
    fn a_default_route_over_anything_else_is_metered() {
        let dir = fake_root(
            Some(&format!(
                "{HEADER}\nrmnet0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n"
            )),
            None,
            &[],
        );
        assert_eq!(probe_under(dir.path()), Network::Metered);
    }

    /// §a route that is not a DEFAULT route is not a way out.
    ///
    /// Wi-Fi without uplink (captive portal, AP with no uplink): subnet route,
    /// no default. Counting it reports Wi-Fi, syncs into nothing.
    #[test]
    fn a_subnet_route_without_a_default_is_offline() {
        let dir = fake_root(
            Some(&format!(
                "{HEADER}\nwlan0\t0002A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n"
            )),
            None,
            &["wlan0"],
        );
        assert_eq!(probe_under(dir.path()), Network::Offline);
    }

    /// §a default route that is DOWN is not a way out either.
    #[test]
    fn a_default_route_without_rtf_up_is_offline() {
        let dir = fake_root(
            Some(&format!(
                "{HEADER}\nwlan0\t00000000\t0102A8C0\t0002\t0\t0\t600\t00000000\t0\t0\t0\n"
            )),
            None,
            &["wlan0"],
        );
        assert_eq!(probe_under(dir.path()), Network::Offline);
    }

    /// §an IPv6-only connection is seen.
    ///
    /// Cellular routinely IPv6-only. Reading only /proc/net/route reports
    /// offline, stops syncing.
    #[test]
    fn an_ipv6_only_default_route_is_found() {
        let dir = fake_root(
            Some(&format!("{HEADER}\n")),
            Some(
                "00000000000000000000000000000000 00 \
                 00000000000000000000000000000000 00 \
                 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 rmnet0\n",
            ),
            &[],
        );
        assert_eq!(probe_under(dir.path()), Network::Metered);
    }

    /// §loopback is never a network.
    #[test]
    fn a_loopback_route_is_not_a_connection() {
        let dir = fake_root(
            Some(&format!(
                "{HEADER}\nlo\t00000000\t00000000\t0003\t0\t0\t600\t00000000\t0\t0\t0\n"
            )),
            None,
            &[],
        );
        assert_eq!(probe_under(dir.path()), Network::Offline);
    }

    /// §a reading that could not be taken is Unknown, NOT Offline.
    ///
    /// Most important case here. `Offline` stops automatic sync; kernel with
    /// files elsewhere, or sandbox hiding them, must not stop it.
    #[test]
    fn an_unreadable_proc_is_unknown_rather_than_offline() {
        let dir = fake_root(None, None, &[]);
        assert_eq!(
            probe_under(dir.path()),
            Network::Unknown,
            "a probe that cannot read the kernel must not be able to stop Vuo syncing"
        );
    }

    /// §Wi-Fi wins when both are up.
    #[test]
    fn wifi_is_preferred_where_both_carry_a_default_route() {
        let dir = fake_root(
            Some(&format!(
                "{HEADER}\n\
                 rmnet0\t00000000\t0102A8C0\t0003\t0\t0\t700\t00000000\t0\t0\t0\n\
                 wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n"
            )),
            None,
            &["wlan0"],
        );
        assert_eq!(probe_under(dir.path()), Network::Wifi);
    }

    /// §an interface name from a kernel file cannot walk out of /sys.
    #[test]
    fn an_interface_name_is_never_a_path() {
        let dir = fake_root(None, None, &[]);
        assert!(!is_wireless(dir.path(), "../../../etc"));
        assert!(!is_wireless(dir.path(), "wlan0/../../.."));
        assert!(!is_wireless(dir.path(), ""));
    }

    /// §what each reading allows, and that Unknown allows everything.
    #[test]
    fn the_policy_never_blocks_on_a_reading_it_could_not_take() {
        // Wi-Fi only OFF: anything but nonexistent connection.
        assert!(Network::Wifi.allows(false));
        assert!(Network::Metered.allows(false));
        assert!(Network::Unknown.allows(false));
        assert!(!Network::Offline.allows(false));

        // Wi-Fi only ON: setting is promise, metered refused.
        assert!(Network::Wifi.allows(true));
        assert!(!Network::Metered.allows(true));
        assert!(!Network::Offline.allows(true));
        assert!(
            Network::Unknown.allows(true),
            "a probe that failed must fall back to syncing, not to silence"
        );
    }
}
