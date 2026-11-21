use super::*;
use crate::app::action::Tab;
use crate::app::keymap::Keymap;
use crate::app::testing::ready_app;
use crate::app::worker::{self, Update};
use crate::backends::{Backends, DaemonPoke};
use crate::config::Config;
use crate::diagnostics::query::{Preset, Scope};
use crate::exec::fake::FakeExecutor;
use crate::theme::Theme;
use alpheus_commun::temp_path::TempPath;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::time::Duration;

fn at(secs: u64, t0: Instant) -> Instant {
    t0 + Duration::from_secs(secs)
}

fn on_diag(t0: Instant) -> App {
    let mut a = App::with_tab(Config::default(), Theme::default(), Keymap::default(), Tab::Diagnostics, t0);
    a.take_requests();
    a
}

fn the_reads() -> Vec<Request> {
    vec![
        Request::Diag(DiagRead::Restarts(3)),
        Request::Diag(DiagRead::Crashes),
        Request::Diag(DiagRead::BootTime),
        Request::Diag(DiagRead::Checks(vec![])),
        Request::Do(Op::FailedRefresh),
    ]
}

#[test]
fn opening_on_the_diag_tab_reads_only_its_domains() {
    let t0 = Instant::now();
    let mut a = App::with_tab(Config::default(), Theme::default(), Keymap::default(), Tab::Diagnostics, t0);
    let mut expected = vec![Request::Refresh(vec![Domain::Failed, Domain::Health])];
    expected.extend(the_reads());
    assert_eq!(a.take_requests(), expected);

    // Reached from another tab: the same reads, once.
    let mut a = ready_app();
    a.go_tab(Tab::Diagnostics, t0);
    let r = a.take_requests();
    assert_eq!(
        r.iter().filter(|q| matches!(q, Request::Refresh(_))).collect::<Vec<_>>(),
        [&Request::Refresh(vec![Domain::Failed, Domain::Health])]
    );
    for read in the_reads() {
        assert_eq!(r.iter().filter(|q| **q == read).count(), 1, "{read:?} once in {r:?}");
    }
}

#[test]
fn another_tab_reads_nothing_of_diagnostics() {
    let t0 = Instant::now();
    let mut a = ready_app();
    let mut all = Vec::new();
    for tenth in 0..120 {
        a.on_tick(t0 + Duration::from_millis(100 * tenth));
        all.extend(a.take_requests());
    }
    assert!(!all.is_empty(), "Home does refresh");
    for r in &all {
        match r {
            Request::Refresh(ds) => assert!(!ds.contains(&Domain::Failed) && !ds.contains(&Domain::Health), "{ds:?}"),
            other => panic!("{other:?}"),
        }
    }
    // Leaving the tab stops its reads.
    let mut a = on_diag(t0);
    a.go_tab(Tab::Power, at(1, t0));
    a.take_requests();
    a.on_tick(at(30, t0));
    assert!(a.take_requests().is_empty(), "Power has nothing to read");
}

#[test]
fn the_five_second_refresh_skips_the_once_reads() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    a.on_tick(at(4, t0));
    assert!(a.take_requests().is_empty(), "not read at 4 s");
    a.on_tick(at(5, t0));
    assert_eq!(
        a.take_requests(),
        vec![Request::Refresh(vec![Domain::Failed, Domain::Health])],
        "the loops, crashes and boot stay as they were"
    );
    a.on_tick(at(6, t0));
    assert!(a.take_requests().is_empty());
}

#[test]
fn f5_reads_everything_again() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    a.diagnostics_refresh(at(2, t0));
    let r = a.take_requests();
    assert!(r.contains(&Request::Refresh(vec![Domain::Failed, Domain::Health])));
    for read in the_reads() {
        assert!(r.contains(&read), "{read:?}");
    }
    let mut home = ready_app();
    home.diagnostics_refresh(t0);
    assert!(home.take_requests().is_empty(), "only on the Diag tab");
}

#[test]
fn no_refresh_while_the_journal_view_is_open() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    a.diag.view = View::Journal(JournalView { query: JournalQuery::new(Preset::Live, 500) });
    for s in 1..=10 {
        a.on_tick(at(s, t0));
    }
    assert!(a.take_requests().is_empty(), "the journal view owns the worker");
    a.diag.view = View::Overview;
    a.on_tick(at(11, t0));
    assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Failed, Domain::Health])], "back on the overview it reads again");
}

#[test]
fn a_failing_failed_units_domain_is_slowed_to_fifteen_seconds() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    let gone = || Err(BackendError::Missing { tool: "systemctl".into() });
    a.on_update(Update::Failed(Scope::System, gone()), t0);
    a.on_update(Update::Failed(Scope::User, gone()), t0);
    a.on_tick(at(5, t0));
    assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Health])], "the broken tool is not relaunched every 5 s");
    a.on_tick(at(15, t0));
    assert!(a.take_requests().iter().any(|r| matches!(r, Request::Refresh(ds) if ds.contains(&Domain::Failed))));
}

fn unit(name: &str) -> FailedUnit {
    FailedUnit { unit: name.into(), load: "loaded".into(), active: "failed".into(), sub: "failed".into(), description: "d".into() }
}

#[test]
fn a_failed_read_keeps_the_last_list_with_its_error() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    assert!(a.diag.failed_system.loading());
    a.on_update(Update::Failed(Scope::System, Ok(vec![unit("mnt-data.mount")])), t0);
    assert_eq!(a.diag.failed_system.list().len(), 1);
    assert_eq!(a.diag.failed_system.error, None);
    a.on_update(Update::Failed(Scope::System, Err(BackendError::Timeout { tool: "systemctl".into() })), t0);
    assert_eq!(a.diag.failed_system.list().len(), 1, "the last list stays");
    assert_eq!(a.diag.failed_system.error, Some(BackendError::Timeout { tool: "systemctl".into() }));
    assert!(!a.diag.failed_system.missing());
    a.on_update(Update::Failed(Scope::System, Ok(vec![])), t0);
    assert_eq!(a.diag.failed_system.error, None, "a good read clears the error");
    assert!(a.diag.failed_system.list().is_empty());
    // The user manager is its own list.
    assert!(a.diag.failed_user.loading());
}

fn read(
    compositor: Option<(&'static str, &'static str, bool)>,
    wayland: WaylandHealth,
    theirs: &[(&str, &str)],
    own: &[(&str, &str)],
) -> HealthRead {
    let pairs = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
    HealthRead {
        compositor: compositor.map(|(name, var, answers)| CompositorHealth { name, var, answers }),
        wayland,
        systemd_env: Ok(pairs(theirs)),
        own_env: pairs(own),
    }
}

fn rows_of(
    h: Option<&HealthRead>,
    user: &Section<FailedUnit>,
    daemon: Option<&PokeAnswer>,
    checks: &[(Check, Option<CheckResult>)],
) -> Vec<HealthRow> {
    health_rows(&HealthInputs { read: h, failed_user: user, daemon, checks })
}

fn ok_user() -> Section<FailedUnit> {
    Section { items: Some(vec![]), error: None }
}

#[test]
fn health_rows_cases() {
    const MANGO: &str = "MANGO_INSTANCE_SIGNATURE";
    // Nothing read yet: no rows.
    assert!(rows_of(None, &Section::default(), None, &[]).is_empty());

    // Everything fine.
    let good = read(
        Some(("Mango", MANGO, true)),
        WaylandHealth::Reachable("wayland-1".into()),
        &[("WAYLAND_DISPLAY", "wayland-1"), (MANGO, "/run/user/1000/mango-1.sock")],
        &[("WAYLAND_DISPLAY", "wayland-1"), (MANGO, "/run/user/1000/mango-1.sock")],
    );
    let rows = rows_of(Some(&good), &ok_user(), Some(&PokeAnswer::Done), &[]);
    let texts: Vec<(&bool, &str)> = rows.iter().map(|r| (&r.ok, r.text.as_str())).collect();
    assert_eq!(
        texts,
        [
            (&true, "Mango answers"),
            (&true, "WAYLAND_DISPLAY wayland-1 reachable"),
            (&true, "systemd --user environment matches this session"),
            (&true, "no failed user unit"),
            (&true, "the alpheus daemon answers"),
        ]
    );
    assert_eq!(health_summary(&rows), "✓ all good");

    // The user manager points at another display.
    let stale = read(
        Some(("Mango", MANGO, true)),
        WaylandHealth::Reachable("wayland-1".into()),
        &[("WAYLAND_DISPLAY", "wayland-0"), (MANGO, "/run/user/1000/mango-1.sock")],
        &[("WAYLAND_DISPLAY", "wayland-1"), (MANGO, "/run/user/1000/mango-1.sock")],
    );
    let rows = rows_of(Some(&stale), &ok_user(), None, &[]);
    assert_eq!(rows[2].text, "systemd --user has WAYLAND_DISPLAY=wayland-0 (this session: wayland-1)");
    assert!(!rows[2].ok);
    assert_eq!(rows[2].fix.as_deref(), Some("systemctl --user import-environment WAYLAND_DISPLAY"));
    assert_eq!(rows.len(), 4, "no daemon row until it has answered");
    assert_eq!(health_summary(&rows), "✗ 1 problem");

    // A missing variable, and a different compositor socket (only its name is shown).
    let missing = read(
        Some(("Sway", "SWAYSOCK", true)),
        WaylandHealth::Reachable("wayland-1".into()),
        &[("SWAYSOCK", "/run/user/1000/sway-other.sock")],
        &[("WAYLAND_DISPLAY", "wayland-1"), ("SWAYSOCK", "/run/user/1000/sway-me.sock")],
    );
    let rows = rows_of(Some(&missing), &ok_user(), None, &[]);
    assert_eq!(rows[2].text, "systemd --user has no WAYLAND_DISPLAY (this session: wayland-1)");
    assert_eq!(rows[3].text, "systemd --user has a different SWAYSOCK");
    assert!(!format!("{rows:?}").contains("sway-me"), "a socket path never reaches a row");

    // No compositor, no display, an unreadable environment, a failed user unit, a silent daemon.
    let bare = HealthRead {
        compositor: None,
        wayland: WaylandHealth::Unset,
        systemd_env: Err(BackendError::Missing { tool: "systemctl".into() }),
        own_env: vec![],
    };
    let user = Section { items: Some(vec![unit("a.service"), unit("b.service")]), error: None };
    let rows = rows_of(Some(&bare), &user, Some(&PokeAnswer::Unreachable("no socket".into())), &[]);
    let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "no compositor detected: outside a session?",
            "WAYLAND_DISPLAY is not set",
            "systemd user environment unreadable: systemctl not found",
            "2 failed user units",
            "the alpheus daemon does not answer",
        ]
    );
    assert!(rows.iter().all(|r| !r.ok && r.fix.is_none()));
    assert_eq!(health_summary(&rows), "✗ 5 problems");

    // The compositor that does not answer, a socket that is gone, an older daemon.
    let down = read(Some(("Hyprland", "HYPRLAND_INSTANCE_SIGNATURE", false)), WaylandHealth::Missing("wayland-9".into()), &[], &[]);
    let rows = rows_of(Some(&down), &ok_user(), Some(&PokeAnswer::Older), &[]);
    assert_eq!(rows[0].text, "Hyprland does not answer");
    assert_eq!(rows[1].text, "WAYLAND_DISPLAY wayland-9: no such socket");
    assert!(rows[4].ok && rows[4].text.contains("older"), "{:?}", rows[4]);

    // The user units unreadable, with no list to fall back on.
    let broken = Section::<FailedUnit> { items: None, error: Some(BackendError::Timeout { tool: "systemctl".into() }) };
    let rows = rows_of(None, &broken, None, &[]);
    assert_eq!(rows[0].text, "failed user units unreadable: systemctl is not responding");

    // Declared checks: none until answered, then ✓, or ✗ with the detail and the fix.
    let checks = vec![
        (
            Check { name: "One hypridle".into(), run: vec!["true".into()], fix: Some("pkill -x hypridle".into()) },
            Some(CheckResult { ok: false, detail: "2 running".into() }),
        ),
        (Check { name: "Quiet".into(), run: vec!["true".into()], fix: None }, Some(CheckResult { ok: true, detail: String::new() })),
        (Check { name: "Later".into(), run: vec!["true".into()], fix: None }, None),
    ];
    let rows = rows_of(None, &ok_user(), None, &checks);
    assert_eq!(rows.len(), 3, "the unanswered check has no row");
    assert_eq!((rows[1].ok, rows[1].text.as_str(), rows[1].fix.as_deref()), (false, "One hypridle: 2 running", Some("pkill -x hypridle")));
    assert_eq!((rows[2].ok, rows[2].text.as_str()), (true, "Quiet"));
}

#[test]
fn the_compositor_socket_is_never_written() {
    let dir = TempPath::dir("diag-health-sockets");
    let sock = dir.join("compositor.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut got = Vec::new();
        peer.read_to_end(&mut got).unwrap();
        got
    });
    let exec = Arc::new(FakeExecutor::new());
    let backends = Backends::system(exec, dir.join("none"));
    let sock_var = sock.clone();
    let var = move |k: &str| -> Option<OsString> { (k == "SWAYSOCK").then(|| sock_var.clone().into_os_string()) };
    let h = health(&backends, &var);
    assert_eq!(h.compositor, Some(CompositorHealth { name: "Sway", var: "SWAYSOCK", answers: true }));
    assert_eq!(server.join().unwrap(), Vec::<u8>::new(), "connected and closed, not a byte sent");

    // A compositor that is not listening does not answer.
    let gone = dir.join("gone.sock");
    let var = move |k: &str| -> Option<OsString> { (k == "SWAYSOCK").then(|| gone.clone().into_os_string()) };
    assert_eq!(health(&backends, &var).compositor.map(|c| c.answers), Some(false));
    // None set: no compositor.
    assert_eq!(health(&backends, &|_| None).compositor, None);
}

#[test]
fn the_wayland_socket_is_looked_up_where_it_should_be() {
    let dir = TempPath::dir("diag-wayland");
    let _listener = UnixListener::bind(dir.join("wayland-1")).unwrap();
    std::fs::write(dir.join("wayland-2"), "not a socket").unwrap();
    let runtime = dir.as_path().to_path_buf();
    let with = |name: Option<&'static str>, runtime: Option<PathBuf>| {
        move |k: &str| -> Option<OsString> {
            match k {
                "WAYLAND_DISPLAY" => name.map(OsString::from),
                "XDG_RUNTIME_DIR" => runtime.clone().map(PathBuf::into_os_string),
                _ => None,
            }
        }
    };
    assert_eq!(wayland_health(&with(Some("wayland-1"), Some(runtime.clone()))), WaylandHealth::Reachable("wayland-1".into()));
    assert_eq!(
        wayland_health(&with(Some("wayland-2"), Some(runtime.clone()))),
        WaylandHealth::Missing("wayland-2".into()),
        "a plain file is no socket"
    );
    assert_eq!(wayland_health(&with(Some("wayland-3"), Some(runtime.clone()))), WaylandHealth::Missing("wayland-3".into()));
    assert_eq!(wayland_health(&with(Some("wayland-1"), None)), WaylandHealth::Missing("wayland-1".into()), "no runtime dir to look in");
    assert_eq!(wayland_health(&with(None, Some(runtime))), WaylandHealth::Unset);
    let absolute = dir.join("wayland-1");
    let var = move |k: &str| -> Option<OsString> { (k == "WAYLAND_DISPLAY").then(|| absolute.clone().into_os_string()) };
    assert!(matches!(wayland_health(&var), WaylandHealth::Reachable(_)), "an absolute name is the path itself");
}

struct FakePoke(PokeAnswer);

impl DaemonPoke for FakePoke {
    fn failed_refresh(&self) -> PokeAnswer {
        self.0.clone()
    }
}

#[test]
fn the_poke_answer_sets_the_daemon_check() {
    let t0 = Instant::now();
    let exec = Arc::new(FakeExecutor::new());
    for (answer, ok, older) in
        [(PokeAnswer::Done, true, false), (PokeAnswer::Older, true, true), (PokeAnswer::Unreachable("no socket".into()), false, false)]
    {
        let mut b = Backends::system(exec.clone(), std::env::temp_dir().join("alpheus-poke-none"));
        b.poke = Arc::new(FakePoke(answer.clone()));
        let mut a = on_diag(t0);
        a.on_update(worker::respond(&b, Op::FailedRefresh), t0);
        assert_eq!(a.diag.daemon, Some(answer));
        let rows = a.health_rows();
        let daemon = rows.last().expect("the daemon row");
        assert_eq!(daemon.ok, ok);
        assert_eq!(daemon.text.contains("older"), older);
    }
}

#[test]
fn diag_results_fill_the_sections() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    a.on_update(Update::Diag(DiagRead::Crashes, DiagResult::Crashes(Ok(vec![]))), t0);
    assert_eq!(a.diag.crashes.items, Some(vec![]));
    a.on_update(Update::Diag(DiagRead::BootTime, DiagResult::BootTime(Ok("Startup finished in 42.9s".into()))), t0);
    assert_eq!(a.diag.boot, Loadable::Ready("Startup finished in 42.9s".into()));
    a.on_update(
        Update::Diag(
            DiagRead::Restarts(3),
            DiagResult::Restarts { system: Err(BackendError::Missing { tool: "systemctl".into() }), user: Ok(vec![]) },
        ),
        t0,
    );
    assert!(a.diag.loops_system.missing());
    assert_eq!(a.diag.loops_user.items, Some(vec![]));
}
