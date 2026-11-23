//! The worker thread: the one place that runs the backends (`wpctl`, `nmcli`, the daemon's socket,
//! `systemctl` and the rest) while the program is up. Closing is the exception: `run.rs`'s
//! `revert_on_close` asks the daemon to revert an open display trial itself, once the loop is over.
//!
//! The `App` sends `Request`s (read these domains, do this action, make this Diag read) down a
//! channel and gets `Update`s back on another. The thread takes what has piled up, merges it
//! (`coalesce`), runs each request in turn and answers each one as soon as it is done (a refresh
//! streams one `Update` per domain, each as soon as that domain is read). A slow tool
//! (killed after up to 2 s, `run.rs`'s `TOOL_TIMEOUT`) therefore never freezes the screen. The
//! functions that do the reading and the acting (`fetch_with`, `diag_fetch`, `perform`) are plain
//! functions over `Backends`, so the tests call them with fake backends and no thread.
//!
//! Never:
//! - touch the `App`: it only sends `Update`s, so the `App` stays a plain value;
//! - read the process's environment on its own: the health checks read the `Env` they are given
//!   (`run.rs` gives the process's own; everywhere else it is empty, so a test never finds the
//!   session's compositor);
//! - leave a counted action without an answer: each action the `App` counts in flight (`Op::domains`
//!   not empty) gets exactly one `Update::Done` or `Update::Superseded`, so that count always
//!   returns to zero. `Op::FailedRefresh` is not counted (its `domains` are empty): the daemon is
//!   poked once per burst and answered by `Update::Poke`, never `Done`.
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3), docs/features/control-center.md.

use crate::app::diagnostics::{HealthRead, health};
use crate::backends::{
    AudioStatus, BResult, BackendError, Backends, BtStatus, CheckResult, JournalRead, NetStatus, NodeRef, PokeAnswer, PowerAction,
    Profiles, UnitAction, Volume,
};
use crate::config::Check;
use crate::diagnostics::analyze::Blame;
use crate::diagnostics::crashes::Crash;
use crate::diagnostics::query::{JournalQuery, Scope};
use crate::diagnostics::show::Restarts;
use crate::diagnostics::units::FailedUnit;
use crate::displays::Plan;
use crate::displays::wire::Snapshot;
use std::ffi::OsString;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

/// A group of values the worker reads together. A tab lists the domains it shows (`TabSpec::domains`)
/// and only those are read while it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    Volume,
    Brightness,
    Dnd,
    KeepAwake,
    Profiles,
    Network,
    Bluetooth,
    Audio,
    Displays,
    /// The failed units of both managers.
    Failed,
    /// The session health checks that are cheap enough to repeat.
    Health,
}

impl Domain {
    /// Every domain.
    pub const ALL: [Domain; 11] = [
        Domain::Volume,
        Domain::Brightness,
        Domain::Dnd,
        Domain::KeepAwake,
        Domain::Profiles,
        Domain::Network,
        Domain::Bluetooth,
        Domain::Audio,
        Domain::Displays,
        Domain::Failed,
        Domain::Health,
    ];
}

/// An action that changes something outside alpheus. The worker runs it; `Op::domains` says which
/// reads it makes stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    SetVolume(NodeRef, u8),
    SetDefault(u32),
    SetBrightness(u8),
    SetWifi(bool),
    SetBluetooth(bool),
    SetDnd(bool),
    /// Toggles "Keep awake"; `now_on` is the state aimed for, known at the
    /// time of the action (so the success message doesn't depend on a
    /// stale read arriving in the meantime).
    ToggleKeepAwake {
        now_on: bool,
    },
    SetProfile(String),
    Power(PowerAction),
    /// Display setting, tried by the daemon: answered once the compositor
    /// has, its 15 s trial then open there.
    TryDisplays(Plan),
    /// Keeps the daemon's trial.
    KeepDisplays,
    /// Reverts the daemon's trial.
    RevertDisplays,
    /// Restarts or clears a user unit (never a system one: those run in the TUI's own window).
    UnitUser(UnitAction, String),
    /// Asks the alpheus daemon to re-read its failed-units count; answered by `Update::Poke`.
    FailedRefresh,
    /// Puts the bug report on the clipboard: the text on the copy program's standard input.
    Copy {
        argv: Vec<String>,
        text: String,
    },
}

/// A read of the Diag tab that is not one of the repeated domains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagRead {
    /// Services restarted at least this many times, of both managers.
    Restarts(u32),
    Crashes,
    BootTime,
    /// The declared health checks.
    Checks(Vec<Check>),
    /// The loaded service units of both managers (the journal's picker).
    ServiceUnits,
    Journal(JournalQuery),
    /// Whether this user can open the system journal's files (once per journal view).
    JournalReadable,
    /// `systemd-analyze blame` and `critical-chain`.
    BootReport,
    /// The bug report, built and redacted (the compositor's name is the Diag tab's health read).
    Report {
        compositor: String,
        restart_loop: u32,
    },
}

/// The answer to a `DiagRead`, of the same shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagResult {
    Restarts { system: BResult<Vec<Restarts>>, user: BResult<Vec<Restarts>> },
    Crashes(BResult<Vec<Crash>>),
    BootTime(BResult<String>),
    Checks(Vec<CheckResult>),
    ServiceUnits(BResult<Vec<(Scope, String)>>),
    Journal(BResult<JournalRead>),
    JournalReadable(bool),
    BootReport { blame: BResult<Vec<Blame>>, chain: BResult<Vec<String>> },
    Report(String),
}

/// What the `App` asks of the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Refresh(Vec<Domain>),
    Do(Op),
    Diag(DiagRead),
}

/// What the worker answers: a read, or the end of an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Volume(BResult<Volume>),
    Brightness(BResult<u8>),
    Network(BResult<NetStatus>),
    Bluetooth(BResult<BtStatus>),
    Dnd(BResult<bool>),
    KeepAwake(BResult<bool>),
    Profiles(BResult<Profiles>),
    Audio(BResult<AudioStatus>),
    Displays(BResult<Snapshot>),
    Failed(Scope, BResult<Vec<FailedUnit>>),
    Health(HealthRead),
    Diag(DiagRead, DiagResult),
    /// The daemon's answer to `Op::FailedRefresh` (which has no `Done`: it can answer "older").
    Poke(PokeAnswer),
    Done(Op, BResult<()>),
    /// A setting dropped by coalescing a burst (a more recent change to the
    /// same slider replaced it): each `Request::Do` thus gets exactly one
    /// response, `Done` or `Superseded`.
    Superseded(Op),
}

impl Op {
    /// Domains whose display this action changes (optimistic display):
    /// their reads are ignored while it's in flight.
    pub fn domains(&self) -> &'static [Domain] {
        match self {
            Op::SetVolume(NodeRef::DefaultSink, _) => &[Domain::Volume, Domain::Audio],
            Op::SetVolume(NodeRef::Id(_), _) | Op::SetDefault(_) => &[Domain::Audio, Domain::Volume],
            Op::SetBrightness(_) => &[Domain::Brightness],
            Op::SetWifi(_) => &[Domain::Network],
            Op::SetBluetooth(_) => &[Domain::Bluetooth],
            Op::SetDnd(_) => &[Domain::Dnd],
            Op::ToggleKeepAwake { .. } => &[Domain::KeepAwake],
            Op::SetProfile(_) => &[Domain::Profiles],
            Op::TryDisplays(_) | Op::KeepDisplays | Op::RevertDisplays => &[Domain::Displays],
            Op::UnitUser(..) => &[Domain::Failed],
            Op::Power(_) | Op::FailedRefresh | Op::Copy { .. } => &[],
        }
    }
}

/// The environment the health checks read: the process's own in the real worker (`run.rs`), an
/// empty one everywhere else, so a test never finds (nor connects to) the session's compositor.
pub type Env = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// No variable set at all.
pub fn empty_env() -> Env {
    Arc::new(|_| None)
}

/// What reading `d` gives: one update, two for the failed units (one per manager).
pub fn fetch_all(b: &Backends, d: Domain, var: &dyn Fn(&str) -> Option<OsString>) -> Vec<Update> {
    match d {
        Domain::Failed => [Scope::System, Scope::User].into_iter().map(|s| Update::Failed(s, b.diagnostics.failed(s))).collect(),
        d => vec![fetch_with(b, d, var)],
    }
}

/// `fetch_with` over an empty environment.
pub fn fetch(b: &Backends, d: Domain) -> Update {
    fetch_with(b, d, &|_| None)
}

/// One update for a domain that gives one (the failed units' first manager for `Failed`; the
/// worker uses `fetch_all`); `var` is the environment of the health checks.
pub fn fetch_with(b: &Backends, d: Domain, var: &dyn Fn(&str) -> Option<OsString>) -> Update {
    match d {
        Domain::Volume => Update::Volume(b.sound.volume()),
        Domain::Brightness => Update::Brightness(b.brightness.get()),
        Domain::Dnd => Update::Dnd(b.notifications.dnd()),
        Domain::KeepAwake => Update::KeepAwake(b.energy.keep_awake()),
        Domain::Profiles => Update::Profiles(b.energy.profiles()),
        Domain::Network => Update::Network(b.network.status()),
        Domain::Bluetooth => Update::Bluetooth(b.bluetooth.status()),
        Domain::Audio => Update::Audio(b.sound.status()),
        Domain::Displays => Update::Displays(b.displays.snapshot().map_err(BackendError::Daemon)),
        Domain::Failed => Update::Failed(Scope::System, b.diagnostics.failed(Scope::System)),
        Domain::Health => Update::Health(health(b, var)),
    }
}

/// A manager's unit names, each tagged with that manager.
fn scoped(scope: Scope, names: BResult<Vec<String>>) -> BResult<Vec<(Scope, String)>> {
    match names {
        Ok(names) => Ok(names.into_iter().map(|name| (scope, name)).collect()),
        Err(e) => Err(e),
    }
}

/// A Diag read, done.
pub fn diag_fetch(b: &Backends, read: &DiagRead) -> DiagResult {
    let d = &*b.diagnostics;
    match read {
        DiagRead::Restarts(t) => DiagResult::Restarts { system: d.restarts(Scope::System, *t), user: d.restarts(Scope::User, *t) },
        DiagRead::Crashes => DiagResult::Crashes(d.crashes()),
        DiagRead::BootTime => DiagResult::BootTime(d.boot_time()),
        DiagRead::Checks(checks) => DiagResult::Checks(checks.iter().map(|c| d.check(c)).collect()),
        DiagRead::ServiceUnits => {
            let system = scoped(Scope::System, d.service_units(Scope::System));
            let user = scoped(Scope::User, d.service_units(Scope::User));
            // One manager failing is not an error while the other answers; both failing gives the
            // system manager's error.
            DiagResult::ServiceUnits(match (system, user) {
                (Ok(mut a), Ok(b)) => {
                    a.extend(b);
                    Ok(a)
                }
                (Ok(a), Err(_)) | (Err(_), Ok(a)) => Ok(a),
                (Err(e), Err(_)) => Err(e),
            })
        }
        DiagRead::Journal(q) => DiagResult::Journal(d.journal(q)),
        DiagRead::JournalReadable => DiagResult::JournalReadable(d.journal_readable()),
        DiagRead::BootReport => DiagResult::BootReport { blame: d.blame(15), chain: d.critical_chain(60) },
        DiagRead::Report { compositor, restart_loop } => DiagResult::Report(d.report(compositor, *restart_loop)),
    }
}

/// The worker's answer to an action: `Done`, except the daemon poke, which says more.
pub fn respond(b: &Backends, op: Op) -> Update {
    match op {
        Op::FailedRefresh => Update::Poke(b.poke.failed_refresh()),
        op => {
            let r = perform(b, &op);
            Update::Done(op, r)
        }
    }
}

/// Runs an action through the backends. The answer is what the tool said, as a `BResult`.
pub fn perform(b: &Backends, op: &Op) -> BResult<()> {
    match op {
        Op::SetVolume(n, v) => b.sound.set_volume(*n, *v),
        Op::SetDefault(id) => b.sound.set_default(*id),
        Op::SetBrightness(v) => b.brightness.set(*v),
        Op::SetWifi(on) => b.network.set_wifi(*on),
        Op::SetBluetooth(on) => b.bluetooth.set_power(*on),
        Op::SetDnd(on) => b.notifications.set_dnd(*on),
        Op::ToggleKeepAwake { .. } => b.energy.toggle_keep_awake(),
        Op::SetProfile(p) => b.energy.set_profile(p),
        Op::Power(a) => b.power.perform(*a),
        Op::TryDisplays(plan) => b.displays.try_(&plan.steps).map_err(BackendError::Daemon),
        Op::KeepDisplays => b.displays.keep().map_err(BackendError::Daemon),
        Op::RevertDisplays => b.displays.revert().map_err(BackendError::Daemon),
        Op::UnitUser(a, unit) => b.diagnostics.user_action(*a, unit),
        Op::Copy { argv, text } => b.diagnostics.copy(argv, text),
        Op::FailedRefresh => match b.poke.failed_refresh() {
            PokeAnswer::Done | PokeAnswer::Older => Ok(()),
            PokeAnswer::Unreachable(why) => Err(BackendError::Daemon(why)),
        },
    }
}

/// Merges the requests that piled up while the worker was busy: the last setting per slider (a
/// volume per node, one brightness), a single refresh (the union of the domains) placed after the
/// actions, then the Diag reads, each made once. Also returns the dropped settings, to be
/// announced as `Update::Superseded`. Every other action is kept, in order, and none is merged
/// with another: a toggle pressed twice runs twice. The one exception is `Op::FailedRefresh`,
/// kept once per burst (its answer is an `Update::Poke`, and it is not a counted action).
pub fn coalesce(reqs: Vec<Request>) -> (Vec<Request>, Vec<Op>) {
    let mut superseded: Vec<Op> = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut refresh: Vec<Domain> = Vec::new();
    let mut diag: Vec<Request> = Vec::new();
    for r in reqs {
        match r {
            // The same read asked twice (F5 pressed twice) is made once.
            Request::Diag(read) => {
                // Only the newest journal read matters: the view has moved on from the older one.
                if matches!(read, DiagRead::Journal(_)) {
                    diag.retain(|d| !matches!(d, Request::Diag(DiagRead::Journal(_))));
                }
                let r = Request::Diag(read);
                if !diag.contains(&r) {
                    diag.push(r);
                }
            }
            Request::Refresh(ds) => {
                for d in ds {
                    if !refresh.contains(&d) {
                        refresh.push(d);
                    }
                }
            }
            // The daemon is poked once per burst, whatever number of actions asked for it.
            Request::Do(Op::FailedRefresh) => {
                if !ops.contains(&Op::FailedRefresh) {
                    ops.push(Op::FailedRefresh);
                }
            }
            Request::Do(op) => {
                // An earlier setting of the same slider is replaced by this one.
                let mut replaced_at = None;
                for (i, earlier) in ops.iter().enumerate() {
                    let same_slider = match (&op, earlier) {
                        (Op::SetVolume(n, _), Op::SetVolume(m, _)) => m == n,
                        (Op::SetBrightness(_), Op::SetBrightness(_)) => true,
                        _ => false,
                    };
                    if same_slider {
                        replaced_at = Some(i);
                        break;
                    }
                }
                if let Some(i) = replaced_at {
                    superseded.push(ops.remove(i)); // at most one: the previous one was already removed
                }
                ops.push(op);
            }
        }
    }
    let mut out: Vec<Request> = ops.into_iter().map(Request::Do).collect();
    if !refresh.is_empty() {
        out.push(Request::Refresh(refresh));
    }
    out.extend(diag);
    (out, superseded)
}

/// Starts the thread over an empty environment (tests); it stops once the request sender is dropped.
pub fn spawn(b: Backends) -> (Sender<Request>, Receiver<Update>) {
    spawn_with(b, empty_env())
}

/// Starts the thread; `env` is what its health checks read. The thread answers each request in
/// the order `coalesce` gives, and ends when the request sender is dropped or the `App` stops
/// listening (a failed send).
pub fn spawn_with(b: Backends, env: Env) -> (Sender<Request>, Receiver<Update>) {
    let (tx_req, rx_req) = mpsc::channel::<Request>();
    let (tx_up, rx_up) = mpsc::channel::<Update>();
    std::thread::spawn(move || {
        while let Ok(first) = rx_req.recv() {
            let mut batch = vec![first];
            while let Ok(r) = rx_req.try_recv() {
                batch.push(r);
            }
            let (reqs, superseded) = coalesce(batch);
            // One response per setting, even a dropped one: the app counts
            // its pending actions and must never stay stuck.
            for op in superseded {
                if tx_up.send(Update::Superseded(op)).is_err() {
                    return;
                }
            }
            for req in reqs {
                let sent = match req {
                    Request::Refresh(ds) => ds.into_iter().flat_map(|d| fetch_all(&b, d, &*env)).all(|u| tx_up.send(u).is_ok()),
                    Request::Do(op) => tx_up.send(respond(&b, op)).is_ok(),
                    Request::Diag(read) => {
                        let result = diag_fetch(&b, &read);
                        tx_up.send(Update::Diag(read, result)).is_ok()
                    }
                };
                if !sent {
                    return; // the app has gone away
                }
            }
        }
    });
    (tx_req, rx_up)
}

#[cfg(test)]
mod tests {
    //! Tests of the worker (`worker.rs`): what each domain reads, what each action runs, how a burst
    //! is merged, and that the spawned thread answers every request. Fake backends, no real tool.

    use super::*;
    use crate::app::action::Tab;
    use crate::exec::fake::FakeExecutor;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn backends(f: &Arc<FakeExecutor>) -> Backends {
        Backends::system_isolated(f.clone(), std::env::temp_dir().join("alpheus-worker-no-flag"))
    }

    #[test]
    fn domains_per_tab() {
        assert_eq!(Tab::Home.domains().len(), 7);
        assert_eq!(Tab::Sound.domains(), &[Domain::Audio]);
        assert_eq!(Tab::Displays.domains(), &[Domain::Displays]);
        assert_eq!(Tab::Diagnostics.domains(), &[Domain::Failed, Domain::Health]);
        assert!(Tab::Power.domains().is_empty());
        assert!(Tab::Appearance.domains().is_empty());
    }

    #[test]
    fn the_health_read_sees_only_the_environment_it_is_given() {
        let f = Arc::new(FakeExecutor::new());
        let b = backends(&f);
        // Plain `fetch` is an empty environment: no compositor, no Wayland display, whatever the
        // session running the tests has.
        let Update::Health(h) = fetch(&b, Domain::Health) else { panic!("not a health read") };
        assert!(h.compositor.is_none(), "{h:?}");
        assert_eq!(h.wayland, crate::app::diagnostics::WaylandHealth::Unset);
        // A given one is used, and a socket that is not there answers no.
        let env = |k: &str| (k == "SWAYSOCK").then(|| OsString::from("/nonexistent/alpheus-test.sock"));
        let Update::Health(h) = fetch_with(&b, Domain::Health, &env) else { panic!("not a health read") };
        let c = h.compositor.expect("the injected variable is read");
        assert_eq!((c.name, c.answers), ("Sway", false));
    }

    #[test]
    fn the_spawned_worker_reads_an_empty_environment() {
        let f = Arc::new(FakeExecutor::new());
        let (tx, rx) = spawn(backends(&f));
        tx.send(Request::Refresh(vec![Domain::Health])).unwrap();
        let mut got = None;
        for _ in 0..50 {
            if let Ok(Update::Health(h)) = rx.recv_timeout(Duration::from_millis(100)) {
                got = Some(h);
                break;
            }
        }
        assert!(got.expect("a health update").compositor.is_none());
    }

    #[test]
    fn fetch_and_perform() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("shell-ctl dnd-status", "on\n");
        f.ok("nmcli radio wifi off", "");
        let b = backends(&f);
        assert_eq!(fetch(&b, Domain::Dnd), Update::Dnd(Ok(true)));
        assert_eq!(perform(&b, &Op::SetWifi(false)), Ok(()));
        assert_eq!(f.calls(), vec!["shell-ctl dnd-status", "nmcli radio wifi off"]);
    }

    #[test]
    fn burst_coalescing() {
        let (r, dropped) = coalesce(vec![
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 10)),
            Request::Refresh(vec![Domain::Volume]),
            Request::Do(Op::SetVolume(NodeRef::DefaultSink, 20)),
            Request::Do(Op::SetVolume(NodeRef::Id(64), 30)),
            Request::Do(Op::SetBrightness(5)),
            Request::Do(Op::SetBrightness(9)),
            Request::Refresh(vec![Domain::Volume, Domain::Brightness]),
        ]);
        assert_eq!(
            r,
            vec![
                Request::Do(Op::SetVolume(NodeRef::DefaultSink, 20)),
                Request::Do(Op::SetVolume(NodeRef::Id(64), 30)),
                Request::Do(Op::SetBrightness(9)),
                Request::Refresh(vec![Domain::Volume, Domain::Brightness]),
            ]
        );
        assert_eq!(dropped, vec![Op::SetVolume(NodeRef::DefaultSink, 10), Op::SetBrightness(5)]);
    }

    #[test]
    fn displays_are_read_and_changed_through_the_daemon() {
        use crate::app::testing::FakeDisplays;
        use crate::displays::Step;
        use crate::displays::wire::{Status, TrialState};
        let f = Arc::new(FakeExecutor::new());
        let fake = Arc::new(FakeDisplays::default());
        let mut b = backends(&f);
        b.displays = fake.clone();
        let snapshot = Snapshot { status: Status::Ready, outputs: crate::displays::testing::machine(), trial: TrialState::None };
        *fake.snapshot.lock().unwrap() = Ok(snapshot.clone());
        assert_eq!(fetch(&b, Domain::Displays), Update::Displays(Ok(snapshot)));
        let plan = Plan { steps: vec![Step { output: "eDP-1".into(), active: None }] };
        assert_eq!(perform(&b, &Op::TryDisplays(plan)), Ok(()));
        assert_eq!(perform(&b, &Op::KeepDisplays), Ok(()));
        assert_eq!(perform(&b, &Op::RevertDisplays), Ok(()));
        assert_eq!(fake.calls(), vec!["snapshot", "try eDP-1", "keep", "revert"]);
        assert!(f.calls().is_empty(), "no command: {:?}", f.calls());
        // The daemon's words become the error, as they are.
        *fake.answer.lock().unwrap() = Err("another change is in progress".into());
        let e = perform(&b, &Op::KeepDisplays).unwrap_err();
        assert_eq!(e.to_string(), "another change is in progress");
    }

    #[test]
    fn display_settings_are_never_coalesced() {
        // A trial then its rollback: both pass, in order.
        let trial = Op::TryDisplays(Plan::default());
        let rollback = Op::RevertDisplays;
        let (r, dropped) = coalesce(vec![Request::Do(trial.clone()), Request::Do(rollback.clone())]);
        assert_eq!(r, vec![Request::Do(trial), Request::Do(rollback)]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn the_failed_units_of_both_managers_and_the_diag_reads_go_through_the_backend() {
        use crate::exec::{CaptureEnd, Captured};
        let f = Arc::new(FakeExecutor::new());
        let done = |s: &str| Captured { stdout: s.as_bytes().to_vec(), end: CaptureEnd::Exited(Some(0)) };
        f.capture_ok("systemctl list-units --failed --output=json --no-pager", done("[]"));
        f.capture_ok("systemctl --user list-units --failed --output=json --no-pager", done("[]"));
        f.capture_ok("systemd-analyze time --no-pager", done("Startup finished in 42.9s\n"));
        let b = backends(&f);
        assert_eq!(
            fetch_all(&b, Domain::Failed, &|_| None),
            vec![Update::Failed(Scope::System, Ok(vec![])), Update::Failed(Scope::User, Ok(vec![]))],
            "one update per manager"
        );
        assert_eq!(diag_fetch(&b, &DiagRead::BootTime), DiagResult::BootTime(Ok("Startup finished in 42.9s".into())));
        // A read nobody programmed is an error shown in its section, never a panic.
        assert!(matches!(diag_fetch(&b, &DiagRead::Crashes), DiagResult::Crashes(Err(_))));
        assert_eq!(f.calls().len(), 4);
    }

    /// The journal's unit list tags each unit with its manager, keeps one manager's list when the
    /// other fails, and fails only when both do.
    #[test]
    fn the_unit_list_tags_each_unit_with_its_manager_and_survives_one_failing() {
        use crate::exec::{CaptureEnd, Captured, ExecError};
        let system_cmd = "systemctl list-units --type=service --all --output=json --no-pager";
        let user_cmd = "systemctl --user list-units --type=service --all --output=json --no-pager";
        let one = |name: &str| Captured {
            stdout: format!(r#"[{{"unit":"{name}","load":"loaded","active":"active","sub":"running","description":"x"}}]"#).into_bytes(),
            end: CaptureEnd::Exited(Some(0)),
        };
        let f = Arc::new(FakeExecutor::new());
        f.capture_ok(system_cmd, one("system-one.service"));
        f.capture_ok(user_cmd, one("user-one.service"));
        let b = backends(&f);
        assert_eq!(
            diag_fetch(&b, &DiagRead::ServiceUnits),
            DiagResult::ServiceUnits(Ok(vec![(Scope::System, "system-one.service".into()), (Scope::User, "user-one.service".into())]))
        );
        let f = Arc::new(FakeExecutor::new());
        f.capture_ok(system_cmd, one("system-one.service"));
        f.capture_err(user_cmd, ExecError::Io("boom".into()));
        assert_eq!(
            diag_fetch(&backends(&f), &DiagRead::ServiceUnits),
            DiagResult::ServiceUnits(Ok(vec![(Scope::System, "system-one.service".into())])),
            "the user manager failing leaves the system list"
        );
        let f = Arc::new(FakeExecutor::new());
        f.capture_err(system_cmd, ExecError::Io("boom".into()));
        f.capture_ok(user_cmd, one("user-one.service"));
        assert_eq!(
            diag_fetch(&backends(&f), &DiagRead::ServiceUnits),
            DiagResult::ServiceUnits(Ok(vec![(Scope::User, "user-one.service".into())])),
            "the system manager failing leaves the user list"
        );
        let f = Arc::new(FakeExecutor::new());
        f.capture_err(system_cmd, ExecError::Io("boom".into()));
        f.capture_err(user_cmd, ExecError::Io("boom".into()));
        assert!(matches!(diag_fetch(&backends(&f), &DiagRead::ServiceUnits), DiagResult::ServiceUnits(Err(_))));
    }

    #[test]
    fn a_user_unit_action_goes_through_the_backend_and_a_poke_answers_by_itself() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("systemctl --user --no-block restart -- example-sync.service", "");
        let b = backends(&f);
        let op = Op::UnitUser(UnitAction::Restart, "example-sync.service".into());
        assert_eq!(respond(&b, op.clone()), Update::Done(op, Ok(())));
        assert_eq!(f.calls(), vec!["systemctl --user --no-block restart -- example-sync.service"]);
        assert!(matches!(respond(&b, Op::FailedRefresh), Update::Poke(PokeAnswer::Unreachable(_))), "no daemon wired: unreachable");
    }

    #[test]
    fn a_repeated_diag_read_is_made_once_and_the_order_is_kept() {
        let (r, dropped) = coalesce(vec![
            Request::Diag(DiagRead::Crashes),
            Request::Refresh(vec![Domain::Failed]),
            Request::Diag(DiagRead::BootTime),
            Request::Diag(DiagRead::Crashes),
            Request::Do(Op::FailedRefresh),
            Request::Do(Op::FailedRefresh),
        ]);
        // The actions first, then the tab's main content (the failed units and the health), then
        // the once-reads: what the person looks at arrives before what is below it.
        assert_eq!(
            r,
            vec![
                Request::Do(Op::FailedRefresh),
                Request::Refresh(vec![Domain::Failed]),
                Request::Diag(DiagRead::Crashes),
                Request::Diag(DiagRead::BootTime),
            ],
            "the poke is asked once"
        );
        assert!(dropped.is_empty());
    }

    #[test]
    fn a_newer_journal_read_replaces_a_queued_one() {
        use crate::diagnostics::query::{JournalQuery, Preset};
        let q = |preset, n| Request::Diag(DiagRead::Journal(JournalQuery::new(preset, n)));
        let (r, dropped) =
            coalesce(vec![q(Preset::Live, 100), Request::Diag(DiagRead::Crashes), q(Preset::ErrorsThisBoot, 200), q(Preset::Live, 300)]);
        assert_eq!(r, vec![Request::Diag(DiagRead::Crashes), q(Preset::Live, 300)], "only the newest journal read is made");
        assert!(dropped.is_empty());
        // Other reads are not touched.
        let (r, _) = coalesce(vec![Request::Diag(DiagRead::Crashes), Request::Diag(DiagRead::BootTime)]);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn toggles_are_never_coalesced() {
        let (r, dropped) = coalesce(vec![Request::Do(Op::SetWifi(false)), Request::Do(Op::SetWifi(true))]);
        assert_eq!(r.len(), 2);
        assert!(dropped.is_empty());
    }

    #[test]
    fn the_thread_responds() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("shell-ctl dnd-status", "off\n");
        f.ok("shell-ctl dnd-on", "");
        let (tx, rx) = spawn(backends(&f));
        tx.send(Request::Refresh(vec![Domain::Dnd, Domain::KeepAwake])).unwrap();
        let t = Duration::from_secs(1);
        assert_eq!(rx.recv_timeout(t).unwrap(), Update::Dnd(Ok(false)));
        assert_eq!(rx.recv_timeout(t).unwrap(), Update::KeepAwake(Ok(false)));
        tx.send(Request::Do(Op::SetDnd(true))).unwrap();
        assert_eq!(rx.recv_timeout(t).unwrap(), Update::Done(Op::SetDnd(true), Ok(())));
    }

    #[test]
    fn each_setting_gets_a_response_even_when_coalesced() {
        // The thread is busy for 200 ms on a slow read: the three settings
        // pile up behind it and form a single burst.
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(200)));
        let (tx, rx) = spawn(backends(&f));
        tx.send(Request::Refresh(vec![Domain::Dnd])).unwrap();
        // The call is recorded before the wait: as soon as it shows up, the
        // thread is busy and its current burst already formed.
        let t0 = Instant::now();
        while f.calls().is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(2), "the thread did not start");
            std::thread::yield_now();
        }
        tx.send(Request::Do(Op::SetBrightness(30))).unwrap();
        tx.send(Request::Do(Op::SetBrightness(40))).unwrap();
        tx.send(Request::Do(Op::SetBrightness(50))).unwrap();
        let t = Duration::from_secs(2);
        assert!(matches!(rx.recv_timeout(t).unwrap(), Update::Dnd(_)));
        let mut received = Vec::new();
        while received.len() < 3 {
            received.push(rx.recv_timeout(t).expect("one response per setting"));
        }
        let last = received.pop().unwrap();
        assert!(matches!(last, Update::Done(Op::SetBrightness(50), _)), "{last:?}");
        assert_eq!(received, vec![Update::Superseded(Op::SetBrightness(30)), Update::Superseded(Op::SetBrightness(40))]);
    }

    #[test]
    fn domains_touched_by_an_action() {
        assert_eq!(Op::SetWifi(true).domains(), &[Domain::Network]);
        assert_eq!(Op::ToggleKeepAwake { now_on: true }.domains(), &[Domain::KeepAwake]);
        assert!(Op::SetVolume(NodeRef::DefaultSink, 5).domains().contains(&Domain::Audio));
        assert!(Op::SetDefault(64).domains().contains(&Domain::Volume));
        assert!(Op::Power(PowerAction::Lock).domains().is_empty());
        for op in [Op::TryDisplays(Plan::default()), Op::KeepDisplays, Op::RevertDisplays] {
            assert_eq!(op.domains(), &[Domain::Displays]);
        }
    }

    #[test]
    fn spawn_does_not_block_the_caller() {
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(300)));
        let (tx, rx) = spawn(backends(&f));
        let t = Instant::now();
        tx.send(Request::Refresh(Tab::Home.domains().to_vec())).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50));
        // The first domain arrives without waiting for the other six.
        assert!(matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Update::Volume(Err(_))));
    }
}
