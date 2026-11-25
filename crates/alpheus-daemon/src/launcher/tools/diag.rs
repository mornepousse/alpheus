//! The diagnostics tools of the Tools menu: the network scans, the USB events script, and the subnet
//! the host discovery sweeps.
//!
//! This is the pure half of the Diagnostics built-ins (`catalog.rs` lists them, `app.rs` spawns them).
//! `Scan` is the ten nmap scans of the decision "The diagnostics tools run in the held terminal, and
//! the scan's target is typed" (docs/decisions.md), in that order; `scan_argv` builds the exact argv of one,
//! from a target and ports typed in the box, and the subnet when it is a sweep. `Subnet` is the seam
//! to the network: its real form (`ProcSubnet`) reads `/proc/net/route` once, when the sweep is
//! chosen, and takes the connected route of the default route's interface, so nothing parses the
//! output of `ip`.
//!
//! Never:
//! - go through a shell for a scan: each value (target, ports, subnet) is one whole argv element,
//!   and a target comes after `--`, so one that starts with `-` is a host, never an nmap option;
//! - sweep what is not a local net: a route with no connected line, or a net outside /16 to /30
//!   (a /31, a /32, anything wider than /16), gives no subnet, and the box says "no network";
//! - run anything or log anything: `app.rs` starts the argv, in the launcher's held terminal, and
//!   logs a fixed word.
//!
//! See: docs/features/tools-menu.md ("The diagnostics tools"), docs/decisions.md ("The diagnostics
//! tools run in the held terminal, and the scan's target is typed").

use crate::sources::resources::default_route;
use std::path::PathBuf;

/// The ten nmap scans, in the order of the menu (docs/decisions.md, "The diagnostics tools run in the
/// held terminal, and the scan's target is typed").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    HostDiscovery,
    Quick,
    Full,
    Services,
    Scripts,
    Ports,
    Udp,
    Vuln,
    Os,
    Aggressive,
}

impl Scan {
    pub const ALL: [Scan; 10] = [
        Scan::HostDiscovery,
        Scan::Quick,
        Scan::Full,
        Scan::Services,
        Scan::Scripts,
        Scan::Ports,
        Scan::Udp,
        Scan::Vuln,
        Scan::Os,
        Scan::Aggressive,
    ];

    /// The choice's text, as listed in the box and kept in the history key.
    pub fn label(self) -> &'static str {
        match self {
            Scan::HostDiscovery => "Host discovery (ping sweep)",
            Scan::Quick => "Quick scan (common ports)",
            Scan::Full => "Full scan (all ports)",
            Scan::Services => "Services and versions",
            Scan::Scripts => "Default scripts and versions",
            Scan::Ports => "Specific ports",
            Scan::Udp => "UDP scan (top 20, sudo)",
            Scan::Vuln => "Vulnerabilities (--script vuln)",
            Scan::Os => "OS detection (sudo)",
            Scan::Aggressive => "Aggressive scan -A (sudo)",
        }
    }

    pub fn from_label(label: &str) -> Option<Scan> {
        Scan::ALL.into_iter().find(|s| s.label() == label)
    }

    /// Every scan but the sweep asks for a target.
    pub fn asks_target(self) -> bool {
        self != Scan::HostDiscovery
    }
}

/// The text of the prompt of a scan that asks for a target alone.
pub const TARGET_PROMPT: &str = "Target (IP or host)";
/// The text of the "Specific ports" prompt: one answer, the target then the ports.
pub const PORTS_PROMPT: &str = "Target, then ports (host 22,80,443)";

/// The target and the ports of a "Specific ports" answer: the first word, then the rest. None when
/// the ports are missing.
pub fn split_ports(text: &str) -> Option<(&str, &str)> {
    let (target, ports) = text.trim().split_once(char::is_whitespace)?;
    let ports = ports.trim();
    (!ports.is_empty()).then_some((target, ports))
}

/// Whether `ports` is only digits, commas and dashes (`22,80,8000-8100`). The text becomes the value
/// of `-p`, so anything else (`-oN /tmp/x`, `--script=x`, a space) could pass nmap an option. nmap's
/// `T:`/`U:` prefixes are left out on purpose: the box takes plain port lists.
pub fn valid_ports(ports: &str) -> bool {
    !ports.is_empty() && ports.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '-')
}

/// The argv of `scan`: None when something it needs is missing (the target, the ports, or the subnet
/// of a sweep). A target comes after `--`.
pub fn scan_argv(scan: Scan, target: Option<&str>, ports: Option<&str>, subnet: Option<&str>) -> Option<Vec<String>> {
    let nmap = |sudo: bool, flags: &[&str], target: &str| -> Vec<String> {
        let front: &[&str] = if sudo { &["sudo", "nmap"] } else { &["nmap"] };
        front.iter().chain(flags).chain(&["--", target]).map(|s| s.to_string()).collect()
    };
    if scan == Scan::HostDiscovery {
        return Some(vec!["nmap".to_string(), "-sn".to_string(), subnet?.to_string()]);
    }
    let target = target.map(str::trim).filter(|t| !t.is_empty())?;
    Some(match scan {
        Scan::HostDiscovery => return None,
        Scan::Quick => nmap(false, &["-F"], target),
        Scan::Full => nmap(false, &["-p-"], target),
        Scan::Services => nmap(false, &["-sV"], target),
        Scan::Scripts => nmap(false, &["-sC", "-sV"], target),
        Scan::Ports => nmap(false, &["-p", ports.filter(|p| !p.trim().is_empty())?], target),
        Scan::Udp => nmap(true, &["-sU", "--top-ports", "20"], target),
        Scan::Vuln => nmap(false, &["--script", "vuln"], target),
        Scan::Os => nmap(true, &["-O"], target),
        Scan::Aggressive => nmap(true, &["-A"], target),
    })
}

/// The argv of the USB events built-in (`sh -c`, because two streams share one terminal; the
/// script is fixed, nothing typed goes in it). The trap stops `udevadm` with the terminal. It differs
/// from the plain `trap "kill 0" EXIT INT TERM` on purpose: `kill 0` signals the whole
/// group, itself included, so the TERM re-ran the trap without end and bash crashed on every close;
/// the trap is cleared first.
pub fn usb_events_argv() -> Vec<String> {
    [
        "sh",
        "-c",
        "trap 'trap - EXIT INT TERM; kill 0' EXIT INT TERM; udevadm monitor --udev --subsystem-match=usb & journalctl -kf -n 30 --grep=usb",
    ]
    .map(String::from)
    .to_vec()
}

/// Where the host discovery's subnet comes from.
pub trait Subnet {
    /// The IPv4 net of the default route's interface, as `a.b.c.0/n`; None without one.
    fn default_v4(&self) -> Option<String>;
}

/// The real subnet: `/proc/net/route`.
pub struct ProcSubnet {
    path: PathBuf,
}

impl ProcSubnet {
    pub fn new() -> ProcSubnet {
        ProcSubnet::at(PathBuf::from("/proc/net/route"))
    }

    pub fn at(path: PathBuf) -> ProcSubnet {
        ProcSubnet { path }
    }
}

impl Default for ProcSubnet {
    fn default() -> Self {
        ProcSubnet::new()
    }
}

impl Subnet for ProcSubnet {
    fn default_v4(&self) -> Option<String> {
        std::fs::read_to_string(&self.path).ok().and_then(|t| subnet_from_route(&t))
    }
}

/// The widest net a sweep takes, and the narrowest (a /32 is one host).
const WIDEST: u32 = 16;
const NARROWEST: u32 = 30;

/// The net of the default route's interface in `/proc/net/route` text: among that interface's
/// connected lines (a destination and a mask, no gateway, up), the lowest metric, link-local
/// (169.254/16) left out. Addresses are the kernel's native-order words printed in hex, so their
/// network-order bytes are the word's native bytes, on either byte order.
pub fn subnet_from_route(route: &str) -> Option<String> {
    let iface = default_route(route)?;
    route
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (name, dest, gateway, flags) = (f.next()?, f.next()?, f.next()?, f.next()?);
            let metric = f.nth(2)?.parse::<u32>().ok()?;
            let mask = f.next()?;
            let up = u32::from_str_radix(flags, 16).ok()? & 1 != 0;
            if name != iface || !up || gateway != "00000000" {
                return None;
            }
            let [a, b, c, d] = u32::from_str_radix(dest, 16).ok()?.to_ne_bytes();
            let mask = u32::from_be_bytes(u32::from_str_radix(mask, 16).ok()?.to_ne_bytes());
            let prefix = mask.leading_ones();
            // A contiguous mask: its ones, then its zeros.
            if mask == 0 || prefix + mask.trailing_zeros() != 32 || !(WIDEST..=NARROWEST).contains(&prefix) || [a, b] == [169, 254] {
                return None;
            }
            Some((metric, format!("{a}.{b}.{c}.{d}/{prefix}")))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, net)| net)
}

#[cfg(test)]
mod tests;
