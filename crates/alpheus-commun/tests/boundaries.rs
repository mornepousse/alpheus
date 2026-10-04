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

/// Locked version string of `name` (e.g. "1.6.2"), read from Cargo.lock.
pub fn version_of(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    version_of_in(&text, name)
}

/// Same lookup, but against an in-memory Cargo.lock text (used to prove the
/// pinning assertion below actually bites, without editing the real lock).
fn version_of_in(text: &str, name: &str) -> String {
    let lock: toml::Table = toml::from_str(text).expect("unreadable Cargo.lock");
    lock["package"]
        .as_array()
        .expect("[[package]]")
        .iter()
        .find(|p| p["name"].as_str() == Some(name))
        .and_then(|p| p["version"].as_str())
        .unwrap_or_else(|| panic!("{name} missing from Cargo.lock"))
        .to_string()
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
fn zbus_stays_in_the_daemon() {
    let g = graph();
    assert!(closure(&g, "alpheus-demon").contains("zbus"), "the daemon reads the system bus");
    for p in ["alpheus-commun", "alpheus"] {
        assert!(!closure(&g, p).contains("zbus"), "{p} pulls in zbus");
    }
}

/// `blocking` is a transitive dependency (through async-io, used by zbus's
/// async executor). Its 1.7 release keeps one idle thread parked in the pool
/// forever, waking every 500 ms even with nothing to do (measured: ~10
/// wake-ups/5s at rest on an otherwise idle daemon). 1.6.2 lets that idle
/// thread exit after its timeout, so the daemon reaches 0 wake-ups at rest.
/// Pinned with `cargo update -p blocking --precise 1.6.2`; no Cargo.toml
/// entry — alpheus never depends on it directly.
#[test]
fn blocking_stays_on_1_6_to_avoid_an_eternal_idle_wakeup() {
    let v = version_of("blocking");
    assert!(v.starts_with("1.6."), "blocking {v} must stay on 1.6.x (1.7 keeps an idle thread waking every 500 ms forever)");
}

#[test]
fn blocking_pin_check_bites_on_an_unpinned_lock() {
    let fake = r#"
[[package]]
name = "blocking"
version = "1.7.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    let v = version_of_in(fake, "blocking");
    let result = std::panic::catch_unwind(|| {
        assert!(v.starts_with("1.6."), "blocking {v} must stay on 1.6.x (1.7 keeps an idle thread waking every 500 ms forever)");
    });
    assert!(result.is_err(), "the pin check must reject blocking 1.7.0");
}
