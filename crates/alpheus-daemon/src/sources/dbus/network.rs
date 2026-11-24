//! NetworkManager: the kind of the primary connection, how many other VPNs
//! are active, connectivity. No name is read: the bar shows icons only.
//!
//! Never:
//! - read a connection's name, uuid or address: the module draws icons and a count, so nothing
//!   that names a network is kept here (the Wi-Fi detail reads SSIDs, in `wifi.rs`, only while the
//!   panel is open);
//! - act: `nmcli` does that, from the quick panel, through the executor.
//!
//! See: docs/features/bar.md (the network module), docs/features/quick-settings.md.

use super::{Bus, NM, optional};
use crate::system::{Link, NetState};
use std::io;

pub const PATH: &str = "/org/freedesktop/NetworkManager";
pub const INTERFACE: &str = "org.freedesktop.NetworkManager";
pub const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
/// NMActiveConnectionState: activated.
const ACTIVATED: i64 = 2;
/// NMConnectivityState: portal, limited.
const LIMITED: [i64; 2] = [2, 3];

/// None: not shown (loopback).
fn link(kind: &str, vpn: bool) -> Option<Link> {
    if vpn {
        return Some(Link::Vpn);
    }
    match kind {
        "802-11-wireless" => Some(Link::Wifi),
        "802-3-ethernet" => Some(Link::Ethernet),
        // WireGuard tunnels have Vpn = false (survey).
        "vpn" | "wireguard" => Some(Link::Vpn),
        "loopback" => None,
        _ => Some(Link::Other),
    }
}

pub fn read(bus: &mut dyn Bus) -> io::Result<NetState> {
    // Every call below goes to NetworkManager's unique name: if it stops
    // meanwhile, the call fails instead of starting it again.
    let Some(nm) = bus.owner(NM)? else { return Ok(NetState::Absent) };
    let root = bus.get_all(&nm, PATH, INTERFACE)?;
    let primary = root.path("PrimaryConnection").map(String::from);
    let limited = root.int("Connectivity").is_some_and(|c| LIMITED.contains(&c));
    let mut online = None;
    let mut vpns = 0;
    for path in root.paths("ActiveConnections") {
        let Some(a) = optional(bus.get_all(&nm, path, ACTIVE))? else { continue };
        if a.int("State") != Some(ACTIVATED) {
            continue;
        }
        let Some(kind) = link(a.str("Type").unwrap_or(""), a.bool("Vpn").unwrap_or(false)) else { continue };
        if primary.as_deref() == Some(path.as_str()) {
            online = Some(kind);
        } else if kind == Link::Vpn {
            vpns += 1;
        }
    }
    Ok(match online {
        Some(link) => NetState::Online { link, vpns, limited },
        None => NetState::Offline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::dbus::fake::FakeBus;
    use crate::sources::dbus::props::Prop;

    const AC: &str = "/org/freedesktop/NetworkManager/ActiveConnection";

    fn primary(bus: &mut FakeBus, n: u32) {
        bus.props_mut(PATH, INTERFACE).0.insert("PrimaryConnection".into(), Prop::Path(format!("{AC}/{n}")));
    }

    #[test]
    fn the_workstation_ethernet_primary_and_two_vpns() {
        let mut bus = FakeBus::workstation();
        assert_eq!(read(&mut bus).unwrap(), NetState::Online { link: Link::Ethernet, vpns: 2, limited: false });
    }

    #[test]
    fn wifi_primary_and_no_name_read() {
        let mut bus = FakeBus::workstation();
        primary(&mut bus, 9);
        assert_eq!(read(&mut bus).unwrap(), NetState::Online { link: Link::Wifi, vpns: 2, limited: false });
        assert!(bus.calls.iter().all(|c| !c.contains("AccessPoint")), "icons only: the access point is never read: {:?}", bus.calls);
    }

    #[test]
    fn a_vpn_as_primary_and_the_other_vpns() {
        let mut bus = FakeBus::workstation();
        primary(&mut bus, 3);
        assert_eq!(read(&mut bus).unwrap(), NetState::Online { link: Link::Vpn, vpns: 1, limited: false });
    }

    #[test]
    fn limited_connectivity_offline_and_vanishing_connections() {
        let mut bus = FakeBus::workstation();
        bus.props_mut(PATH, INTERFACE).0.insert("Connectivity".into(), Prop::Int(3));
        assert!(matches!(read(&mut bus).unwrap(), NetState::Online { limited: true, .. }));
        bus.props.remove(&(format!("{AC}/2"), ACTIVE.to_string()));
        let NetState::Online { vpns, .. } = read(&mut bus).unwrap() else { panic!("online") };
        assert_eq!(vpns, 1, "a connection gone between two reads is skipped");
        bus.props_mut(PATH, INTERFACE).0.insert("PrimaryConnection".into(), Prop::Path("/".into()));
        assert_eq!(read(&mut bus).unwrap(), NetState::Offline);
    }

    #[test]
    fn networkmanager_absent_only_asks_who_owns_the_name() {
        let mut bus = FakeBus::empty();
        assert_eq!(read(&mut bus).unwrap(), NetState::Absent);
        assert_eq!(bus.calls, ["GetNameOwner org.freedesktop.NetworkManager"]);
    }

    #[test]
    fn an_outage_is_an_error() {
        let mut bus = FakeBus::workstation();
        bus.failing.insert(PATH.into());
        assert_eq!(read(&mut bus).unwrap_err().kind(), io::ErrorKind::Other, "the root object");
        let mut bus = FakeBus::workstation();
        bus.failing.insert(format!("{AC}/2"));
        assert_eq!(read(&mut bus).unwrap_err().kind(), io::ErrorKind::Other, "an active connection: a timeout is not a connection gone");
    }
}
