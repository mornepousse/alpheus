//! Tests of the diagnostics tools' pure parts: the scan table and its argv, the subnet read from
//! `/proc/net/route` text, and the typed text's split. No program is ever started.

use super::*;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn each_scan_builds_the_scripts_command() {
    let net = Some("192.168.1.0/24");
    let t = Some("10.0.0.5");
    let cases: [(Scan, Option<&str>, Vec<String>); 10] = [
        (Scan::HostDiscovery, None, v(&["nmap", "-sn", "192.168.1.0/24"])),
        (Scan::Quick, t, v(&["nmap", "-F", "--", "10.0.0.5"])),
        (Scan::Full, t, v(&["nmap", "-p-", "--", "10.0.0.5"])),
        (Scan::Services, t, v(&["nmap", "-sV", "--", "10.0.0.5"])),
        (Scan::Scripts, t, v(&["nmap", "-sC", "-sV", "--", "10.0.0.5"])),
        (Scan::Ports, t, v(&["nmap", "-p", "22,80", "--", "10.0.0.5"])),
        (Scan::Udp, t, v(&["sudo", "nmap", "-sU", "--top-ports", "20", "--", "10.0.0.5"])),
        (Scan::Vuln, t, v(&["nmap", "--script", "vuln", "--", "10.0.0.5"])),
        (Scan::Os, t, v(&["sudo", "nmap", "-O", "--", "10.0.0.5"])),
        (Scan::Aggressive, t, v(&["sudo", "nmap", "-A", "--", "10.0.0.5"])),
    ];
    for (scan, target, want) in cases {
        assert_eq!(scan_argv(scan, target, Some("22,80"), net), Some(want), "{scan:?}");
    }
}

#[test]
fn the_scans_are_the_scripts_ten_in_its_order() {
    let labels: Vec<&str> = Scan::ALL.iter().map(|s| s.label()).collect();
    assert_eq!(labels.len(), 10);
    assert_eq!(labels[0], "Host discovery (ping sweep)");
    assert_eq!(labels[5], "Specific ports");
    for s in Scan::ALL {
        assert_eq!(Scan::from_label(s.label()), Some(s), "{s:?} round-trips");
    }
    assert_eq!(Scan::from_label("nope"), None);
    assert!(!Scan::HostDiscovery.asks_target());
    assert!(Scan::ALL[1..].iter().all(|s| s.asks_target()));
}

#[test]
fn a_target_starting_with_a_dash_is_a_host_never_an_option() {
    let a = scan_argv(Scan::Quick, Some("-oN /tmp/x"), None, None).unwrap();
    assert_eq!(a, v(&["nmap", "-F", "--", "-oN /tmp/x"]), "after `--`, one element");
}

#[test]
fn a_typed_target_is_trimmed_before_it_reaches_nmap() {
    let a = scan_argv(Scan::Quick, Some("  10.0.0.5 \t"), None, None).unwrap();
    assert_eq!(a, v(&["nmap", "-F", "--", "10.0.0.5"]), "no space around the host");
}

#[test]
fn host_discovery_without_a_subnet_runs_nothing() {
    assert_eq!(scan_argv(Scan::HostDiscovery, None, None, None), None);
    // A scan that needs a target or ports and has none builds nothing either.
    assert_eq!(scan_argv(Scan::Quick, None, None, None), None);
    assert_eq!(scan_argv(Scan::Ports, Some("h"), None, None), None);
    assert_eq!(scan_argv(Scan::Ports, None, Some("22"), None), None);
}

#[test]
fn the_typed_text_of_a_port_scan_is_a_target_then_the_ports() {
    assert_eq!(split_ports("10.0.0.5 22,80"), Some(("10.0.0.5", "22,80")));
    assert_eq!(split_ports("  host   22  "), Some(("host", "22")));
    assert_eq!(split_ports("host"), None, "ports missing");
    assert_eq!(split_ports("   "), None);
}

#[test]
fn only_digits_commas_and_dashes_are_ports() {
    for ok in ["22", "22,80,443", "1-1024", "22,8000-8100"] {
        assert!(valid_ports(ok), "{ok}");
    }
    for bad in ["", "-oN /tmp/x", "--script=x", "22 80", "22;ls", "T:22", "U:53", "$(id)", "-p-"] {
        assert!(!valid_ports(bad), "{bad}");
    }
}

const ROUTE: &str = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
wlan0\t0000A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n\
docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n\
wlan0\t0000FEA9\t00000000\t0001\t0\t0\t1000\t0000FFFF\t0\t0\t0\n";

#[test]
fn the_subnet_is_the_connected_route_of_the_default_interface() {
    assert_eq!(subnet_from_route(ROUTE), Some("192.168.0.0/24".to_string()));
}

#[test]
fn a_link_local_route_with_a_better_metric_is_never_the_net() {
    let route = format!("{ROUTE}wlan0\t0000FEA9\t00000000\t0001\t0\t0\t10\t0000FFFF\t0\t0\t0\n");
    assert_eq!(subnet_from_route(&route), Some("192.168.0.0/24".to_string()));
}

#[test]
fn no_default_route_no_connected_route_or_a_tiny_or_huge_net_is_no_subnet() {
    let head = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n";
    assert_eq!(subnet_from_route(""), None);
    assert_eq!(subnet_from_route(head), None);
    let only_default = format!("{head}wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n");
    assert_eq!(subnet_from_route(&only_default), None, "no connected route");
    // A VPN's point-to-point /32.
    let p2p = format!(
        "{head}tun0\t00000000\t0101A8C0\t0003\t0\t0\t50\t00000000\t0\t0\t0\ntun0\t0101A8C0\t00000000\t0001\t0\t0\t50\t00000000\t0\t0\t0\n"
    );
    assert_eq!(subnet_from_route(&p2p), None);
    let slash32 = format!(
        "{head}tun0\t00000000\t0101A8C0\t0003\t0\t0\t50\t00000000\t0\t0\t0\ntun0\t0101A8C0\t00000000\t0001\t0\t0\t50\tFFFFFFFF\t0\t0\t0\n"
    );
    assert_eq!(subnet_from_route(&slash32), None, "a /32 is no net to sweep");
    let slash8 = format!(
        "{head}e0\t00000000\t0100000A\t0003\t0\t0\t50\t00000000\t0\t0\t0\ne0\t0000000A\t00000000\t0001\t0\t0\t50\t000000FF\t0\t0\t0\n"
    );
    assert_eq!(subnet_from_route(&slash8), None, "a /8 is not swept");
    // A hole in the mask.
    let holey = format!(
        "{head}e0\t00000000\t0100000A\t0003\t0\t0\t50\t00000000\t0\t0\t0\ne0\t0000A8C0\t00000000\t0001\t0\t0\t50\t00FF00FF\t0\t0\t0\n"
    );
    assert_eq!(subnet_from_route(&holey), None);
}

#[test]
fn the_real_subnet_reads_the_route_file_and_a_missing_one_is_none() {
    let dir = std::env::temp_dir().join(format!("alpheus-diag-route-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("route");
    std::fs::write(&file, ROUTE).unwrap();
    assert_eq!(ProcSubnet::at(file.clone()).default_v4(), Some("192.168.0.0/24".to_string()));
    assert_eq!(ProcSubnet::at(dir.join("absent")).default_v4(), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_usb_events_script_is_the_scripts() {
    assert_eq!(
        usb_events_argv(),
        v(&[
            "sh",
            "-c",
            "trap 'trap - EXIT INT TERM; kill 0' EXIT INT TERM; udevadm monitor --udev --subsystem-match=usb & journalctl -kf -n 30 --grep=usb"
        ])
    );
}
