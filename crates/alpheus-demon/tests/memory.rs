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

use alpheus_commun::launcher_ipc::{Reply, Request, encode_request, socket_path};
use common::{Daemon, DaemonOptions, FakeHyprland, PrivateBus, PwDump, SIGNATURE, Sway, launcher, start_daemon_with, wtype, wtype_spawn};
use std::io::Write;
use std::os::unix::net::UnixStream;
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
    let d = start_daemon_with(&sway, &DaemonOptions { bar_toml, system_bus: Some(&bus.address), pw_dump, ..DaemonOptions::default() });
    assert!(d.wait_for(&format!("bar placed on HEADLESS-{outputs}"), DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// ≈ 300 synthetic applications, each with an action: the size of a real
/// desktop's catalog (Mae's has about that many). Synthetic only (no capture).
fn many_apps() -> Vec<(String, String)> {
    (0..300)
        .map(|i| {
            (
                format!("alpheus-synthetic-{i}.desktop"),
                format!(
                    "[Desktop Entry]\nType=Application\nName=Synthetic app {i}\nGenericName=Generic thing {i}\nKeywords=one;two;three;\nExec=synthetic-{i} %U\nActions=new;\n\n[Desktop Action new]\nName=New window\nExec=synthetic-{i} --new\n"
                ),
            )
        })
        .collect()
}

fn start_launcher(name: &str, bus: &PrivateBus, bar_toml: Option<&str>, apps: &[(String, String)]) -> (Sway, FakeHyprland, Daemon) {
    let files: Vec<(&str, &str)> = apps.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    let sway = Sway::start(name, 1);
    let hypr = FakeHyprland::start(&sway.runtime, SIGNATURE);
    let options = DaemonOptions { bar_toml, system_bus: Some(&bus.address), desktop_files: &files, ..DaemonOptions::default() };
    let d = start_daemon_with(&sway, &options);
    assert!(d.wait_for("bar placed on HEADLESS-1", DELAY), "{:?}", d.log());
    assert!(d.wait_for("launcher socket ready", DELAY), "{:?}", d.log());
    (sway, hypr, d)
}

/// Open, then close, through the socket like Mod+Space: the n-th open and
/// the n-th close (of any reason).
fn cycle(sway: &Sway, d: &Daemon, n: usize) {
    assert_eq!(launcher(sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    assert_eq!(launcher(sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher closed (", n, DELAY), "{:?}", d.log());
}

/// Open through the socket, close with a real Escape typed by wtype: the
/// keyboard is bound (keymap, compose table), and the key that closes arms
/// sctk's repeat timer (task 12, note 2) — what toggle cycles never touch.
fn key_cycle(sway: &Sway, d: &Daemon, n: usize) {
    assert_eq!(launcher(sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", n, DELAY), "{:?}", d.log());
    assert!(wtype(sway, &["-s", "300", "-k", "Escape"]).success());
    assert!(d.wait_for_count("launcher closed (", n, DELAY), "{:?}", d.log());
    let log = d.log();
    let last = log.iter().rev().find(|l| l.contains("launcher closed ("));
    assert!(last.is_some_and(|l| l.contains("(escape)")), "closed by the key, not otherwise: {last:?}");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_open_latency_under_20ms() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("latency");
    let (sway, _hypr, d) = start_launcher("latency", &bus, None, &many_apps());
    std::thread::sleep(Duration::from_secs(2));
    let mut us = Vec::new();
    for n in 1..=20 {
        cycle(&sway, &d, n);
        us.push(d.last_open_latency_us().expect("the latency is logged"));
    }
    us.sort_unstable();
    let (median, max) = (us[us.len() / 2], us[us.len() - 1]);
    eprintln!("launcher open latency, 20 opens (request accepted → first frame committed): median {median} µs, max {max} µs; {us:?}");
    assert!(median < 20_000, "median {median} µs >= 20 ms: report to Mae with these figures");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_open_close_cycles_no_growth() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("cycles");
    let (sway, _hypr, d) = start_launcher("cycles", &bus, None, &many_apps());
    std::thread::sleep(Duration::from_secs(3)); // startup settled, as in pss_under_30mb_and_stable
    let closed = d.pss_kb();
    assert_eq!(launcher(&sway, &Request::Toggle), Reply::Done);
    assert!(d.wait_for_count("launcher shown on", 1, DELAY), "{:?}", d.log());
    // Typed: the keyboard is bound and its keymap and compose table are
    // compiled — part of what an open launcher costs, measured with it.
    // One continuous wtype invocation, not two: on headless sway
    // (WLR_LIBINPUT_NO_DEVICES=1) the seat's only keyboard capability is
    // wtype's own virtual keyboard, gone the moment its process exits — a
    // second, separate wtype call would find the launcher already closed
    // (the first process exiting looks exactly like the keyboard being
    // unplugged, `new_capability`/`remove_capability` in wayland.rs). One
    // process, two `-s` delays, keeps that one virtual keyboard alive from
    // the typing through to the Escape that closes the launcher.
    let typing = wtype_spawn(&sway, &["-s", "300", "synth", "-s", "3000", "-k", "Escape"]);
    std::thread::sleep(Duration::from_millis(1500)); // after "synth" is typed, well before Escape fires
    let open = d.pss_kb();
    // Still well inside the delay before Escape: the launcher must still be
    // open when this PSS was read, or it is not measuring what it claims to.
    assert_eq!(d.count("launcher closed ("), 0, "launcher closed before the 'open' PSS was read: {:?}", d.log());
    assert!(typing.wait().unwrap().success());
    assert!(d.wait_for_count("launcher closed (escape)", 1, DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    // Baseline after one warm-up cycle: the bar's bounded glyph cache now
    // holds the box's glyphs (same convention as pss_under_30mb_and_stable).
    let baseline = d.pss_kb();
    // 50 cycles, 17 of them closed by a typed Escape (a keyboard bound and
    // released each time), the others by Mod+Space's toggle.
    for n in 2..=51 {
        if n % 3 == 0 { key_cycle(&sway, &d, n) } else { cycle(&sway, &d, n) }
    }
    std::thread::sleep(Duration::from_secs(2));
    let end = d.pss_kb();
    eprintln!(
        "launcher PSS: closed {closed} KB, open {open} KB (+{} KB), after one cycle {baseline} KB, after 50 more {end} KB",
        open.saturating_sub(closed)
    );
    assert!(open <= closed + 6 * 1024, "open costs {} KB > 6 MB: report to Mae, never absorb", open - closed);
    assert!(end <= baseline + 1024, "50 open/close cycles grew PSS: {baseline} -> {end} KB");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups_after_a_launcher_cycle() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("launcher-rest");
    let (sway, _hypr, d) = start_launcher("launcher-rest", &bus, Some(AT_REST), &[]);
    std::thread::sleep(Duration::from_secs(2));
    let threads = d.threads();
    // Closed by a typed Escape: a key that closes arms sctk's repeat timer
    // after the keyboard is released (task 12, note 2); it must fire once
    // and go, never keep re-arming.
    key_cycle(&sway, &d, 1);
    // A script menu whose client goes away: its watch must go with it.
    let mut client = UnixStream::connect(socket_path(&sway.runtime)).unwrap();
    client.write_all(&encode_request(&Request::Dmenu { prompt: None, lines: vec!["a".into()] })).unwrap();
    assert!(d.wait_for_count("launcher shown on", 2, DELAY), "{:?}", d.log());
    drop(client);
    assert!(d.wait_for("launcher closed (close)", DELAY), "{:?}", d.log());
    // Past REQUEST_DEADLINE: a timer that keeps re-arming, a keyboard repeat
    // or a watch left behind shows up below as wake-ups.
    std::thread::sleep(Duration::from_secs(3));
    let before = d.wakeups();
    std::thread::sleep(Duration::from_secs(5));
    let delta = d.wakeups().saturating_sub(before);
    eprintln!("wake-ups at rest after a launcher cycle (5 s): {delta}; threads {} (before the cycle: {threads})", d.threads());
    assert!(delta <= 4, "{delta} wake-ups in 5 s at rest after the launcher was used");
    assert_eq!(d.threads(), threads, "no thread left by the launcher");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn launcher_threads_back_to_baseline() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("launcher-threads");
    let (sway, _hypr, d) = start_launcher("launcher-threads", &bus, Some(AT_REST), &many_apps());
    std::thread::sleep(Duration::from_secs(2));
    let before = d.threads();
    cycle(&sway, &d, 1);
    let t = Instant::now();
    while d.threads() != before {
        assert!(t.elapsed() < Duration::from_secs(1), "{} threads 1 s after close, {before} before open", d.threads());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Controller carry (task 1 review): a dmenu of 100,000 short lines, open
/// PSS and its value 1 s after cancel — printed, no assertion (the budgets
/// above already cover growth and the 6 MB open margin on a realistic
/// catalog; this is the worst-case dmenu size, reported for the README).
/// The request itself goes through `alpheus_launch::exchange` on its own
/// thread, exactly as the shipped client would send it; a typed Escape
/// cancels it, as a person would.
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn large_dmenu_pss_is_reported() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("large-dmenu");
    let (sway, _hypr, d) = start_launcher("large-dmenu", &bus, None, &[]);
    std::thread::sleep(Duration::from_secs(2));
    let closed = d.pss_kb();
    let socket = socket_path(&sway.runtime);
    let lines: Vec<String> = (0..100_000).map(|i| format!("entry-{i}")).collect();
    let menu = std::thread::spawn(move || alpheus_launch::exchange(&socket, &Request::Dmenu { prompt: None, lines }));
    assert!(d.wait_for("launcher shown on", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1)); // ranked and drawn
    let open = d.pss_kb();
    assert!(wtype(&sway, &["-s", "300", "-k", "Escape"]).success());
    assert_eq!(menu.join().unwrap(), Ok(Reply::Cancelled));
    assert!(d.wait_for("launcher closed (escape)", DELAY), "{:?}", d.log());
    std::thread::sleep(Duration::from_secs(1));
    let after = d.pss_kb();
    eprintln!(
        "large dmenu (100,000 lines) PSS: {closed} KB closed, {open} KB open (+{} KB), {after} KB 1 s after cancel (+{} KB vs closed)",
        open.saturating_sub(closed),
        after.saturating_sub(closed)
    );
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn pss_under_30mb_and_stable() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("memory");
    let (sway, hypr, d) = start("memory", 3, &bus, Some("[resources]\ninterval = 1\n"), PwDump::Busy);
    // 3 s: startup is over (fonts, pw-dump's first array, the D-Bus initial
    // state, their release_memory): `initial` is a settled figure, which the
    // return-margin check below needs.
    std::thread::sleep(Duration::from_secs(3));
    let initial = d.pss_kb();
    // Fonts: regular + bold ≈ 2 MB (the whole font database: ≈ 47 MB);
    // plan 1 measured ≈ 12 MB here; zbus (≈ 2.4 MB) and the sound source must
    // fit in what is left (task 16, budget decisions).
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
    let cases = [
        ("plan 1", "\"workspaces\", \"window\", \"clock\""),
        ("+ sound", "\"workspaces\", \"window\", \"volume\", \"clock\""),
        ("+ D-Bus", "\"workspaces\", \"window\", \"network\", \"bluetooth\", \"battery\", \"clock\""),
        ("+ resources", "\"workspaces\", \"window\", \"resources\", \"clock\""),
        ("+ launcher", "\"launcher\", \"workspaces\", \"window\", \"clock\""),
        (
            "everything",
            "\"launcher\", \"workspaces\", \"window\", \"volume\", \"network\", \"bluetooth\", \"battery\", \"resources\", \"clock\"",
        ),
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

/// DejaVu Sans Mono (the default `[font] fallback`): printed for the task
/// report — what loading it costs at startup while unused, and what
/// actually drawing from it costs once a title needs it (✳◐◑◒, the glyphs
/// `fallback_font_draws_a_symbol_the_main_family_lacks` and
/// `uncovered_character_is_masked_with_a_consistent_width`, in
/// `painter.rs`, cover unit-wise).
#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn fallback_font_pss_cost() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("fallback-pss");
    let (_sway, hypr, d) = start("fallback-pss", 1, &bus, Some("modules = [\"window\", \"clock\"]\n"), PwDump::Silent);
    std::thread::sleep(Duration::from_secs(3)); // startup settled: fonts loaded, DejaVu Sans Mono unused so far
    let startup = d.pss_kb();
    // A title with the bug's own symbols, covered only by the fallback.
    hypr.write("activewindow>>app,\u{2733} build \u{25D0}\u{25D1}\u{25D2}\u{25D3}\n");
    std::thread::sleep(Duration::from_millis(500));
    let rendered = d.pss_kb();
    eprintln!("PSS: {startup} KB at startup (DejaVu Sans Mono loaded, unused), {rendered} KB after rendering a title with ✳◐◑◒ from it");
    assert!(startup < STARTUP_MAX_KB, "startup: {startup} KB >= 15 MB");
    assert!(rendered < STARTUP_MAX_KB, "after rendering the fallback glyphs: {rendered} KB >= 15 MB");
}

#[test]
#[ignore = "headless sway and dbus-daemon (nix develop)"]
fn at_rest_no_wakeups() {
    require_release(cfg!(debug_assertions));
    let bus = PrivateBus::start("rest");
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
    // Loop, guard, compositor, one rayon, sound, D-Bus, zbus's executor and
    // reactor (2 measured, 3 allowed), plus transient command reapers.
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
/// restarted, a crash): each reconnection must leave no thread behind
/// (zbus's per-connection executor ends with its connection).
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
