//! What kind of network the phone is on, and whether an automatic sync may use
//! it.
//!
//! # Why this is read from the kernel rather than from Qt
//!
//! The obvious answer is `QNetworkConfigurationManager`: it is Qt, Harbour
//! allows `libQt5Network`, and it reports a bearer type. It was the wrong
//! answer here for two reasons.
//!
//! **Freshness.** A `QObject` lives on the Qt thread, so its answer would have
//! to be pushed to the sync worker as a command. The worker syncs on its own
//! cadence *while the app is minimised*, which is exactly the window in which
//! nothing is pushing anything: the reader walks out of Wi-Fi range, no tap
//! happens, and the worker acts on a reading taken an hour ago. Reading the
//! kernel from the worker means the answer is taken at the moment the decision
//! is made, which is the only moment it is worth taking.
//!
//! **Verifiability.** §8's governing rule is that `make check` runs what CI
//! runs with no phone. A bearer query cannot be exercised that way; two files
//! under a directory this module is handed can, and the tests below do.
//!
//! It also needs no C++ glue, no `cpp_build`, no new dependency, and nothing
//! from Harbour's allowed-libraries list that is not already linked.
//!
//! # What is actually read
//!
//! `/proc/net/route` and `/proc/net/ipv6_route` for a default route, and
//! `/sys/class/net/<iface>/` for whether the interface carrying it is Wi-Fi.
//! `phy80211` is the modern marker and `wireless` the old one; between them
//! they are what `iw` and NetworkManager look at.
//!
//! # The rule that matters most
//!
//! **A reading that could not be taken never blocks a sync.** Every failure
//! path here -- an unreadable `/proc`, a kernel that lays these files out
//! differently, a route through something this does not recognise -- lands on
//! [`Network::Unknown`], and [`Network::allows`] lets `Unknown` through. The
//! worst case is that Vuo behaves exactly as it did before this module
//! existed. The alternative failure mode -- a misread that silently stops
//! syncing on a phone with a perfectly good connection -- is one the user
//! cannot diagnose and would not report as anything but "it stopped working".

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
    /// A default route over something else. Cellular, on a phone -- the
    /// connection the user may be paying for by the byte.
    Metered,
    /// The route files were read and hold no default route at all. A sync
    /// would spend a DNS lookup and a connect timeout to discover this.
    Offline,
    /// Could not be determined. Never blocks anything; see the module docs.
    Unknown,
}

impl Network {
    /// Whether an automatic sync may run over this.
    ///
    /// `wifi_only` is the user's "Only sync on Wi-Fi" setting. It is a promise
    /// about which network Vuo uses, so it is taken literally: on a metered
    /// connection nothing automatic goes out, the outbox included. Nothing is
    /// lost by that -- the outbox is durable and idempotent, and it goes on the
    /// next Wi-Fi or the next manual refresh, which is never gated.
    #[must_use]
    pub fn allows(self, wifi_only: bool) -> bool {
        match self {
            // The reading failed. Behave as though this module were not here.
            Network::Unknown => true,
            Network::Wifi => true,
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

    // Neither file readable is not "offline"; it is "this does not work here".
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

    // Wi-Fi wins where there are several. A phone holding both a Wi-Fi and a
    // cellular default route is using the Wi-Fi one -- and reporting the more
    // permissive of the two is the direction that cannot strand a reader.
    if ifaces.iter().any(|iface| is_wireless(root, iface)) {
        Network::Wifi
    } else {
        Network::Metered
    }
}

/// Interfaces carrying an IPv4 default route.
///
/// The columns are `Iface Destination Gateway Flags RefCnt Use Metric Mask`.
/// A default route is destination and mask both zero, and `RTF_UP` set.
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

/// Interfaces carrying an IPv6 default route.
///
/// The interface is the LAST column, and the first two are the destination and
/// its prefix length: `::/0` is thirty-two zeros followed by `00`.
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

/// Whether the kernel calls this interface wireless.
///
/// `phy80211` is a symlink, so this asks for the link itself rather than
/// following it: a dangling one still says what the interface is.
fn is_wireless(root: &Path, iface: &str) -> bool {
    // The interface name reaches this from a kernel file, but it ends up in a
    // path, so a name carrying separators must not be able to walk out of
    // /sys/class/net.
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
    /// A phone associated to a Wi-Fi network it cannot route through -- a
    /// captive portal, an access point with no uplink -- has a subnet route
    /// and no default. Counting it would report Wi-Fi and sync into nothing.
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
    /// Cellular is routinely IPv6-only. Reading only /proc/net/route would
    /// report such a phone offline and stop syncing it altogether.
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
    /// The single most important case in this module. `Offline` stops
    /// automatic syncing; a kernel whose files are not where this looks, or a
    /// sandbox that will not show them, must not be able to stop it.
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
        // Wi-Fi only OFF: anything but a connection that does not exist.
        assert!(Network::Wifi.allows(false));
        assert!(Network::Metered.allows(false));
        assert!(Network::Unknown.allows(false));
        assert!(!Network::Offline.allows(false));

        // Wi-Fi only ON: the setting is a promise, so metered is refused.
        assert!(Network::Wifi.allows(true));
        assert!(!Network::Metered.allows(true));
        assert!(!Network::Offline.allows(true));
        assert!(
            Network::Unknown.allows(true),
            "a probe that failed must fall back to syncing, not to silence"
        );
    }
}
