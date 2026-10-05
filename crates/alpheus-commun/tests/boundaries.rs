//! Workspace boundaries, read from Cargo.lock: the foundation carries no
//! display dependency, the TUI does not carry Wayland (and, task 3, the
//! daemon does not carry ratatui).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Package → names of its direct dependencies, from Cargo.lock.
pub fn graph() -> BTreeMap<String, BTreeSet<String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let lock: toml::Table = toml::from_str(&text).expect("unreadable Cargo.lock");
    let mut g: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in lock["package"].as_array().expect("[[package]]") {
        let name = p["name"].as_str().expect("name").to_string();
        let deps: Vec<String> = p
            .get("dependencies")
            .and_then(|d| d.as_array())
            .map(|d| d.iter().filter_map(|x| x.as_str()).map(|s| s.split(' ').next().unwrap_or(s).to_string()).collect())
            .unwrap_or_default();
        g.entry(name).or_default().extend(deps);
    }
    g
}

/// Transitive closure of `root`'s dependencies (root excluded).
pub fn closure(g: &BTreeMap<String, BTreeSet<String>>, root: &str) -> BTreeSet<String> {
    assert!(g.contains_key(root), "{root} missing from Cargo.lock");
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(p) = stack.pop() {
        for d in g.get(&p).into_iter().flatten() {
            if seen.insert(d.clone()) {
                stack.push(d.clone());
            }
        }
    }
    seen
}

#[test]
fn the_foundation_carries_no_display_dependency() {
    let f = closure(&graph(), "alpheus-commun");
    for forbidden in ["ratatui", "crossterm", "wayland-client", "smithay-client-toolkit", "cosmic-text"] {
        assert!(!f.contains(forbidden), "alpheus-commun pulls in {forbidden}");
    }
}

#[test]
fn the_tui_does_not_carry_wayland() {
    let f = closure(&graph(), "alpheus");
    assert!(f.contains("alpheus-commun") && f.contains("ratatui"), "{f:?}");
    for forbidden in ["wayland-client", "smithay-client-toolkit", "cosmic-text"] {
        assert!(!f.contains(forbidden), "the TUI pulls in {forbidden}");
    }
}

#[test]
fn the_daemon_does_not_carry_ratatui() {
    let f = closure(&graph(), "alpheus-demon");
    assert!(f.contains("alpheus-commun"), "{f:?}");
    for forbidden in ["ratatui", "crossterm"] {
        assert!(!f.contains(forbidden), "the daemon pulls in {forbidden}");
    }
}

#[test]
fn dbus_stays_in_the_daemon() {
    let g = graph();
    assert!(closure(&g, "alpheus-demon").contains("dbus"), "the daemon reads the system bus");
    for p in ["alpheus-commun", "alpheus"] {
        assert!(!closure(&g, p).contains("dbus"), "{p} pulls in dbus");
    }
}

/// The names that must never come back into the daemon's tree: zbus and
/// its async stack, whose executor thread and `blocking`'s pool thread
/// (1.7 wakes every 500 ms forever) the move to dbus-rs removed. (libdbus
/// and libsystemd are system libraries, not crates: allowed.)
const GONE: [&str; 4] = ["zbus", "blocking", "async-io", "async-executor"];

fn forbidden_in(deps: &BTreeSet<String>) -> Vec<&'static str> {
    GONE.into_iter().filter(|g| deps.contains(*g)).collect()
}

#[test]
fn neither_zbus_nor_blocking_is_in_the_daemon() {
    let found = forbidden_in(&closure(&graph(), "alpheus-demon"));
    assert!(found.is_empty(), "back in alpheus-demon's dependency tree: {found:?}");
}

#[test]
fn the_absence_check_bites_on_a_tree_with_zbus() {
    let tree: BTreeSet<String> = ["dbus", "zbus", "blocking"].map(String::from).into();
    assert_eq!(forbidden_in(&tree), ["zbus", "blocking"]);
}
