//! Spec budget with every module of delivery 3: startup PSS < 15 MB (firm),
//! < 30 MB at steady state and no growth, under a busy compositor and a
//! busy PipeWire; at rest with the resources module hidden, no wake-up —
//! none at all for the sound and D-Bus threads; with resources, the loop
//! wakes at the configured interval, no faster; threads counted, none left
//! behind when the system bus restarts. The system bus is a private
//! dbus-daemon (no NetworkManager, UPower nor BlueZ on it: those modules
//! stay hidden), pw-dump a script printing the fixtures.
//!
//! **Release only.** These budgets are tuned against the release binary —
//! the daemon ships in release, and a debug build inflates PSS by roughly
//! 2x (measured 2026-10-05: "everything" startup 17.4 MB debug vs 7.8 MB
//! release), which would either fail a budget tuned for release or pass
//! one loosened to fit debug, neither of them the figure that matters.
//! `require_release` below panics with this same command if it's missing:
//! `nix develop -c cargo test --release -p alpheus-demon --test memory -- --ignored --test-threads=1`

mod common;

use common::{Daemon, DaemonOptions, FakeHyprland, PrivateBus, PwDump, SIGNATURE, Sway, Workstation, start_daemon_with};
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_secs(10);
/// Startup PSS budget: firm (Mae, 2026-10-04, Q13). Never raised: a figure
/// above it is reported to her with `startup_pss_per_source`'s breakdown.
const STARTUP_MAX_KB: u64 = 15 * 1024;

/// Refuses to measure PSS on a debug build (module doc: roughly 2x
/// inflated, in either direction against a budget tuned for release).
/// Takes the flag rather than reading `cfg!` itself so the decision stays
/// a plain, testable function.
fn require_release(debug_assertions: bool) {
    assert!(
        !debug_assertions,
        "run with --release: these PSS budgets are tuned for the release binary (debug roughly doubles it) — \
         nix develop -c cargo test --release -p alpheus-demon --test memory -- --ignored --test-threads=1"
    );
}

#[test]
fn require_release_panics_only_on_a_debug_build() {
    require_release(false);
    let panicked = std::panic::catch_unwind(|| require_release(true)).is_err();
    assert!(panicked, "a debug build must refuse to run these budget tests");
}

/// Every module but the resources, which tick by design. `interval = 1`: a
/// timer started although the module is hidden would tick 5 times in 5 s.
const AT_REST: &str =
    "modules = [\"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \"clock\"]\n[resources]\ninterval = 1\n";

/// 24 different glyphs per title: over 700 titles, the 4,096 md icons of
/// the Nerd Font all cycle through (the glyph cache must stay bounded).
fn varied_glyphs(n: u64) -> String {
    (0..24u64).filter_map(|i| char::from_u32(0xF0001 + ((n * 24 + i) % 4096) as u32)).collect()
}

fn event(n: u64) -> String {
    match n % 4 {
        0 => format!("activewindow>>firefox,Title {n} {} — 日本語 🦀\n", varied_glyphs(n)),
        1 => "workspacev2>>2,2\n".to_string(),
        2 => format!("windowtitlev2>>583d9c60da90,Title {n}\n"),
        _ => "urgent>>583d9d5fd540\n".to_string(),
    }
}

fn pump(hypr: &FakeHyprland, duration: Duration, n: &mut u64) {
    let t = Instant::now();
    while t.elapsed() < duration {
        hypr.write(&event(*n));
        *n += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn start(name: &str, outputs: u32, bus: &PrivateBus, bar_toml: Option<&str>, pw_dump: PwDump) -> (Sway, FakeHyprland, Daemon) {
    let sway = Sway::start(name, outputs);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml, system_bus: Some(&bus.address), pw_dump });
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn pss_under_30mb_and_stable() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("memory");
    let _workstation = Workstation::serve(&bus);
    let (sway, hypr, d) = start("memory", 3, &bus, Some("[resources]\ninterval = 1\n"), PwDump::Busy);
    // 3 s: startup is over (fonts, pw-dump's first array, the D-Bus initial
    // state, their release_memory): `initial` is a settled figure, which the
    // return-margin check below needs.
    std::thread::sleep(Duration::from_secs(3));
    let initial = d.pss_kb();
    // Fonts: regular + bold ≈ 2 MB (the whole font database: ≈ 47 MB);
    // plan 1 measured ≈ 12 MB here; the D-Bus source and the sound source
    // must fit in what is left (task 16, budget decisions).
    assert!(initial < STARTUP_MAX_KB, "startup: PSS {initial} KB >= 15 MB: run startup_pss_per_source and report to Mae, never raise");
    let mut n = 0;
    pump(&hypr, Duration::from_secs(10), &mut n); // warm-up
    // A first unplug/replug cycle before `baseline` (see plan 1, task 13):
    // `baseline` and `end` are read in comparable states.
    sway.command("output HEADLESS-2 disable");
    std::thread::sleep(Duration::from_secs(1));
    sway.command("output HEADLESS-2 enable");
    std::thread::sleep(Duration::from_secs(1));
    let baseline = d.pss_kb();
    for _ in 0..4 {
        pump(&hypr, Duration::from_secs(8), &mut n);
        sway.command("output HEADLESS-2 disable");
        std::thread::sleep(Duration::from_secs(1));
        sway.command("output HEADLESS-2 enable");
        std::thread::sleep(Duration::from_secs(1));
    }
    let end = d.pss_kb();
    eprintln!(
        "PSS: {initial} KB at startup, {baseline} KB after warm-up, {end} KB after {n} events, 5 plug cycles, pw-dump deltas every 10 ms"
    );
    assert!(end < 30 * 1024, "PSS {end} KB >= 30 MB");
    assert!(end <= baseline + 1024, "growth: {baseline} -> {end} KB");
    const RETURN_MARGIN_KB: u64 = 2 * 1024;
    assert!(end.abs_diff(initial) <= RETURN_MARGIN_KB, "PSS does not come back close to the start: {initial} KB -> {end} KB");
    assert!(d.log().iter().all(|l| !l.contains("sound source down")), "pw-dump kept up: {:?}", d.log());
}

/// Startup PSS per source, against the firm 15 MB budget: plan 1's modules
/// alone, then each new source on its own, then everything. Printed for
/// the task report (step 6): which source costs what.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn startup_pss_per_source() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("per-source");
    let _workstation = Workstation::serve(&bus);
    let cases = [
        ("plan 1", "\"workspaces\", \"window\", \"clock\""),
        ("+ sound", "\"workspaces\", \"window\", \"volume\", \"clock\""),
        ("+ D-Bus", "\"workspaces\", \"window\", \"network\", \"bluetooth\", \"battery\", \"clock\""),
        ("+ resources", "\"workspaces\", \"window\", \"resources\", \"clock\""),
        ("everything", "\"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \"resources\", \"clock\""),
    ];
    let mut report = Vec::new();
    for (i, (what, modules)) in cases.iter().enumerate() {
        let toml = format!("modules = [{modules}]\n");
        let (_sway, _hypr, d) = start(&format!("per-source-{i}"), 3, &bus, Some(&toml), PwDump::Silent);
        std::thread::sleep(Duration::from_secs(3)); // as in pss_under_30mb_and_stable
        report.push((*what, d.pss_kb()));
    }
    eprintln!("startup PSS per source (KB): {report:?}");
    for (what, kb) in &report {
        assert!(*kb < STARTUP_MAX_KB, "{what}: {kb} KB >= 15 MB — report to Mae with these figures, never raise: {report:?}");
    }
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("rest");
    let _workstation = Workstation::serve(&bus);
    let (_sway, _hypr, d) = start("rest", 3, &bus, Some(AT_REST), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(2)); // end of startup
    let before = d.wakeups();
    let sound_before = d.thread_wakeups("sound").expect("the sound thread runs");
    let dbus_before = d.thread_wakeups("D-Bus").expect("the D-Bus thread runs");
    let threads = d.threads();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.wakeups().saturating_sub(before);
    let sound = d.thread_wakeups("sound").unwrap().saturating_sub(sound_before);
    let dbus = d.thread_wakeups("D-Bus").unwrap().saturating_sub(dbus_before);
    eprintln!("wake-ups at rest (5 s): {delta} in all, sound thread {sound}, D-Bus thread {dbus}; {threads} threads");
    // The sound and D-Bus sources are up and block without a timeout: a
    // source down would retry (1 s, 2 s…) and wake the daemon.
    assert!(d.log().iter().all(|l| !l.contains("source down")), "every source up: {:?}", d.log());
    // Nothing changes on the bus nor in PipeWire: a single wake-up of one of
    // these threads is a poll (a loop with a timeout somewhere).
    assert_eq!((sound, dbus), (0, 0), "a source thread wakes at rest");
    // Plan 1 observed 0 to 2; a 1 Hz tick would add ≈ 5.
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest");
    // Loop, guard, compositor, one rayon, sound, D-Bus (libdbus has no
    // thread of its own), plus transient command reapers.
    assert!(threads <= 12, "{threads} threads");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn resources_tick_at_the_configured_interval() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("cadence");
    let toml = "modules = [\"workspaces\", \"resources\", \"clock\"]\n[resources]\ninterval = 1\n";
    // One output: one redraw round trip per tick, not three.
    let (_sway, _hypr, d) = start("cadence", 1, &bus, Some(toml), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(2));
    let before = d.main_thread_wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.main_thread_wakeups().saturating_sub(before);
    eprintln!("loop wake-ups in 5 s at interval = 1: {delta}");
    // 5 ticks: each wakes the loop once for its timer, and a reading that
    // changes the bar adds further round trips (redraw commit, buffer
    // release, frame callback) — measured steadily at ~3 context switches
    // per tick with 1 output (2026-10-05, release, this machine: 15, 15,
    // 17, 15 over 4 runs). ≥ 4: it really ticks every second; ≤ 20: never
    // faster — halving the period (the `ToDuration(period / 2)` mutation)
    // measured 27, comfortably outside this range. The old 4..=12 could
    // no longer hold the steady ~15-17 and was widened from measurements,
    // not loosened to paper over a bug.
    assert!((4..=20).contains(&delta), "{delta} loop wake-ups in 5 s at interval = 1");
}

/// The D-Bus source reconnects after the system bus restarts (dbus.service
/// restarted, a crash): each reconnection must leave no thread behind.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn no_thread_leak_across_system_bus_restarts() {
    require_release(cfg!(debug_assertions));
    let mut bus = PrivateBus::start("restarts");
    let (_sway, _hypr, d) = start("restarts", 1, &bus, Some(AT_REST), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(2));
    let before = d.threads();
    for round in 1..=2 {
        bus.restart();
        // Down, then reconnected at the first retry (1 s, then 2 s). The
        // "down" log line itself is rate-limited (dbus-fixes-report, M2):
        // only the first outage ever, or one after a SUCCEEDED session,
        // is logged (`first || succeeded` in `run_supervised`); a session
        // counts as succeeded only once it has run for `STABLE_DURATION`
        // (10 s) — this private bus has no NetworkManager/UPower/BlueZ to
        // generate a second publication, so the reconnected session must
        // simply stay up that long before the next restart, or its outage
        // would be silently folded into the previous "down" count instead
        // of logging a fresh line (the brief's "5 s is plenty" undershoots
        // STABLE_DURATION; adapted here to the real rate-limiting rule).
        std::thread::sleep(Duration::from_secs(11));
        let downs = d.log().iter().filter(|l| l.contains("D-Bus source down")).count();
        assert_eq!(downs, round, "one outage per restart, then reconnected: {:?}", d.log());
    }
    let after = d.threads();
    eprintln!("threads: {before} before, {after} after two system bus restarts");
    assert_eq!(after, before, "a reconnection leaves no thread behind");
    assert!(after <= 12, "{after} threads");
}

/// The D-Bus source costs exactly one thread, its own (`D-Bus`): no
/// executor, reactor or pool thread behind it (zbus had two, plus
/// `blocking`'s). Same bar but for the three D-Bus modules, on the same
/// recorded workstation.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn the_d_bus_source_costs_one_thread() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("one-thread");
    let _workstation = Workstation::serve(&bus);
    let without = "modules = [\"workspaces\", \"window\", \"clock\"]\n";
    let with = "modules = [\"workspaces\", \"window\", \"network\", \"bluetooth\", \"battery\", \"clock\"]\n";
    let (_s1, _h1, a) = start("one-thread-a", 1, &bus, Some(without), PwDump::Silent);
    let (_s2, _h2, b) = start("one-thread-b", 1, &bus, Some(with), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(3)); // both settled, the D-Bus initial state read
    assert!(b.log().iter().all(|l| !l.contains("source down")), "the D-Bus source up: {:?}", b.log());
    let (ta, tb) = (a.threads(), b.threads());
    eprintln!("threads: {ta} without the D-Bus modules, {tb} with them");
    assert_eq!(tb, ta + 1, "the D-Bus source must cost its own thread and nothing else");
}
