//! Tests of the Tools menu's wiring (`app.rs`): the catalog's context, the argv of a tab and of a
//! built-in, the spawn scope, and the reader's schedule. Every program is a fake; no `App`.

use super::*;
use crate::config::{BarConfig, ModuleId};
use crate::launcher::config::LauncherConfig;
use crate::launcher::run::{self, RunContext, Spawner, launch};
use crate::launcher::tools::catalog::{BUILTINS, Builtin};
use crate::launcher::tools::nav::{Counts, StateJob};
use crate::launcher::tools::read::{JobEvent, run_job};
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::exec::{CaptureEnd, Captured};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

/// The built-in with this id.
fn builtin_by_id(id: &str) -> &'static Builtin {
    BUILTINS.iter().find(|b| b.id == id).unwrap()
}

/// The strings of `s`, owned.
fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| x.to_string()).collect()
}

/// A directory of empty executables, as a `$PATH`.
struct Bin(PathBuf);

impl Bin {
    fn new(name: &str, programs: &[&str]) -> Bin {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("alpheus-tools-app-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for p in programs {
            std::fs::write(dir.join(p), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(dir.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Bin(dir)
    }
}

impl Drop for Bin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn join_answers(shield: bool, clipboard: bool, diagnostics: bool) -> Join {
    Join { shield, clipboard, diagnostics }
}

#[test]
fn the_context_reads_the_configs() {
    let mut bar = BarConfig::default();
    let mut launcher = LauncherConfig::default();
    let c = context(&bar, &launcher, true, join_answers(false, false, false));
    assert_eq!(c.quick, bar.modules.contains(&ModuleId::Quick));
    assert!(c.timer && c.lock && c.session, "the defaults: a lock command, a session");
    assert!(!c.keep_awake && !c.night_light && !c.reload && !c.shield && !c.clipboard && !c.diagnostics);
    bar.quick.keep_awake.toggle = strings(&["ka"]);
    bar.quick.night_light.toggle = strings(&["nl"]);
    bar.quick.lock = Vec::new();
    launcher.tools.reload = strings(&["shell-ctl", "reload-bar"]);
    let c = context(&bar, &launcher, false, join_answers(true, true, true));
    assert!(c.keep_awake && c.night_light && c.reload && c.shield && c.clipboard && c.diagnostics);
    assert!(!c.lock && c.session && !c.timer, "the lock alone is gone, reboot and power off stay");
    bar.quick.reboot = Vec::new();
    bar.quick.poweroff = Vec::new();
    assert!(!context(&bar, &launcher, true, join_answers(false, false, false)).session);
    bar.modules = vec![ModuleId::Clock];
    let c = context(&bar, &launcher, true, join_answers(false, false, false));
    assert!(!c.quick && !c.notifications);
    bar.modules = vec![ModuleId::Quick, ModuleId::Notifications];
    let c = context(&bar, &launcher, true, join_answers(false, false, false));
    assert!(c.quick && c.notifications);
}

#[test]
fn a_tab_opens_through_open_tab_or_the_terminal() {
    let bin = Bin::new("tab", &["alpheus-toggle", "foot"]);
    let dirs = [bin.0.clone()];
    let open_tab = strings(&["alpheus-toggle", "{tab}"]);
    let terminal = strings(&["foot"]);
    assert_eq!(tab_argv(&open_tab, "network", &terminal, &dirs), Some(strings(&["alpheus-toggle", "network"])));
    // open_tab's program missing: the terminal runs `alpheus --tab <slug>`.
    let missing = strings(&["no-such-opener", "{tab}"]);
    assert_eq!(tab_argv(&missing, "sound", &terminal, &dirs), Some(strings(&["foot", "alpheus", "--tab", "sound"])));
    // Both missing.
    assert_eq!(tab_argv(&missing, "sound", &strings(&["no-such-terminal"]), &dirs), None);
    assert_eq!(tab_argv(&[], "sound", &[], &dirs), None);
    // The slug is one whole element, whatever it holds.
    let o = strings(&["alpheus-toggle", "--tab={tab}"]);
    assert_eq!(tab_argv(&o, "x y;z", &terminal, &dirs), Some(strings(&["alpheus-toggle", "--tab=x y;z"])));
}

/// The commands a built-in that spawns may run, from the five things that name them.
fn commands_with<'a>(
    quick: &'a QuickConfig,
    reload: &'a [String],
    open_tab: &'a [String],
    terminal: &'a [String],
    dirs: &'a [PathBuf],
) -> Commands<'a> {
    Commands { quick, reload, open_tab, terminal, path_dirs: dirs }
}

#[test]
fn builtin_argv_is_only_the_configured_command() {
    let bin = Bin::new("argv", &["loginctl", "ka", "shell-ctl", "alpheus-toggle", "foot"]);
    let dirs = [bin.0.clone()];
    let mut quick = QuickConfig::default();
    quick.keep_awake.toggle = strings(&["ka", "--flip"]);
    quick.night_light.toggle = strings(&["no-such-toggle"]);
    let reload = strings(&["shell-ctl", "reload-bar"]);
    let terminal = strings(&["foot"]);
    let open_tab = strings(&["alpheus-toggle", "{tab}"]);
    let c = commands_with(&quick, &reload, &open_tab, &terminal, &dirs);
    assert_eq!(builtin_argv(builtin_by_id("lock"), &c), Ok(Some(strings(&["loginctl", "lock-session"]))));
    assert_eq!(builtin_argv(builtin_by_id("keep_awake"), &c), Ok(Some(strings(&["ka", "--flip"]))));
    assert_eq!(builtin_argv(builtin_by_id("reload"), &c), Ok(Some(strings(&["shell-ctl", "reload-bar"]))));
    assert_eq!(builtin_argv(builtin_by_id("tab.sound"), &c), Ok(Some(strings(&["alpheus-toggle", "sound"]))));
    // A program that is not there: said in the box, never spawned.
    assert_eq!(builtin_argv(builtin_by_id("night_light"), &c), Err("no-such-toggle: program not found".into()));
    // The others spawn nothing of their own.
    for id in ["quick.sound", "dnd", "notes", "timer", "clipboard", "emoji", "power", "quick.shield"] {
        assert_eq!(builtin_argv(builtin_by_id(id), &c), Ok(None), "{id}");
    }
    let empty_reload: Vec<String> = Vec::new();
    let c = commands_with(&quick, &empty_reload, &open_tab, &terminal, &dirs);
    assert!(builtin_argv(builtin_by_id("reload"), &c).is_err(), "no command, nothing to run");
}

#[test]
fn a_tab_with_no_program_to_open_it_names_the_open_tab_program_else_the_terminal() {
    let bin = Bin::new("tab-missing", &[]);
    let dirs = [bin.0.clone()];
    let quick = QuickConfig::default();
    let reload: Vec<String> = Vec::new();
    let open_tab = strings(&["no-such-opener", "{tab}"]);
    let terminal = strings(&["no-such-terminal"]);
    let named = |open_tab: &[String], terminal: &[String]| {
        builtin_argv(builtin_by_id("tab.sound"), &commands_with(&quick, &reload, open_tab, terminal, &dirs))
    };
    assert_eq!(named(&open_tab, &terminal), Err("no-such-opener: program not found".into()), "open_tab's program first");
    assert_eq!(named(&[], &terminal), Err("no-such-terminal: program not found".into()), "else the terminal's");
    assert_eq!(named(&[], &[]), Err("terminal: program not found".into()), "else the word terminal");
}

#[test]
fn the_context_tells_the_quick_and_notifications_modules_apart() {
    let mut bar = BarConfig::default();
    let launcher = LauncherConfig::default();
    let none = join_answers(false, false, false);
    bar.modules = vec![ModuleId::Quick];
    let c = context(&bar, &launcher, false, none);
    assert!(c.quick && !c.notifications, "quick alone");
    bar.modules = vec![ModuleId::Notifications];
    let c = context(&bar, &launcher, false, none);
    assert!(!c.quick && c.notifications, "notifications alone");
}

#[test]
fn the_states_the_daemon_knows_are_dnd_and_the_toggles_that_answer() {
    use crate::config::{BarConfig, ModuleId};
    use crate::launcher::tools::catalog::Catalog;
    let mut bar = BarConfig { modules: vec![ModuleId::Quick, ModuleId::Notifications], ..Default::default() };
    bar.quick.keep_awake.toggle = strings(&["ka"]);
    bar.quick.night_light.toggle = strings(&["nl"]);
    let mut launcher = LauncherConfig::default();
    launcher.tools.entries.push(tools::config::EntryConfig {
        name: "Mine".into(),
        run: strings(&["mine"]),
        position: 1,
        ..Default::default()
    });
    let ctx = context(&bar, &launcher, true, join_answers(false, false, false));
    let catalog = Catalog::build(&launcher.tools, &ctx, &mut |_| {});
    // Reversed, so that the user's entry comes before the built-ins: it must be skipped, not end the walk.
    let mut entries = catalog.entries.clone();
    entries.reverse();
    assert!(matches!(entries[0].kind, EntryKind::User(_)), "the user's entry is first");
    let position = |id: &str| {
        entries
            .iter()
            .position(|e| matches!(e.kind, EntryKind::Builtin(b) if BUILTINS[b].id == id))
            .unwrap_or_else(|| panic!("{id} is not in the catalog"))
    };
    // Do not disturb is off, keep awake is on and night light is unknown.
    let toggles = |t: Toggle| match t {
        Toggle::KeepAwake => Some(true),
        Toggle::NightLight => None,
    };
    let mut expected = vec![(position("dnd"), false), (position("keep_awake"), true)];
    expected.sort();
    assert_eq!(known_builtin_states(&entries, false, &toggles), expected);
    // Do not disturb is on, keep awake is off and night light is on.
    let toggles = |t: Toggle| match t {
        Toggle::KeepAwake => Some(false),
        Toggle::NightLight => Some(true),
    };
    let mut expected = vec![(position("dnd"), true), (position("keep_awake"), false), (position("night_light"), true)];
    expected.sort();
    assert_eq!(known_builtin_states(&entries, true, &toggles), expected);
}

/// Decision 10: everything a built-in spawns is in its own scope, so a reload outlives the daemon's
/// cgroup; detached only when there is no systemd.
#[test]
fn a_built_in_that_spawns_runs_in_its_own_scope() {
    let bin = Bin::new("wrap", &["shell-ctl", "loginctl", "alpheus-toggle"]);
    let dirs = [bin.0.clone()];
    let quick = QuickConfig::default();
    let reload = strings(&["shell-ctl", "reload-bar"]);
    let terminal = strings(&["foot"]);
    let open_tab = strings(&["alpheus-toggle", "{tab}"]);
    let c = commands_with(&quick, &reload, &open_tab, &terminal, &dirs);
    for id in ["reload", "lock", "tab.home"] {
        let argv = builtin_argv(builtin_by_id(id), &c).unwrap().unwrap();
        let fake = FakeExecutor::new();
        fake.ok(&run::wrap(Spawner::SystemdScope, argv.clone()).join(" "), "");
        let ctx = RunContext { exec: &fake, spawner: Spawner::SystemdScope, history_path: None, path_dirs: &dirs };
        launch(&ctx, argv.clone()).unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{id}");
        assert!(calls[0].starts_with("systemd-run --user --scope"), "{id}: {calls:?}");
        assert!(calls[0].ends_with(&argv.join(" ")), "{id}: {calls:?}");
    }
}

/// A state read of entry `entry` running the program `argv`.
fn a_state_job(entry: usize, argv: &str) -> StateJob {
    StateJob { entry, position: entry + 1, argv: strings(&[argv]) }
}

/// A states read of `n` entries.
fn a_states_job(n: usize) -> Job {
    Job::States((0..n).map(|i| a_state_job(i, "s")).collect(), Counts::default())
}

/// A choices read of `entry` with this token.
fn a_choices_job(entry: usize, token: u64) -> Job {
    Job::Choices { entry, position: 1, argv: strings(&["c"]), token }
}

/// Whether the job is a choices read.
fn is_a_choices_job(j: &(u64, Job)) -> bool {
    matches!(j.1, Job::Choices { .. })
}

#[test]
fn an_idle_reader_starts_the_first_job_at_once() {
    let mut s = Scheduler::new();
    assert!(matches!(s.offer(1, a_states_job(2), Some(1)), Some((1, Job::States(..)))));
    // Busy: the next ones wait, one slot per kind.
    assert!(s.offer(1, a_choices_job(0, 1), Some(1)).is_none());
    assert!(s.preempt.load(Ordering::Acquire), "a waiting choices read stops the states read after its entry");
}

/// The flag a waiting choices read raised is lowered when a job starts, so
/// the resumed states read goes through all its remaining entries in one thread.
#[test]
fn a_resumed_states_job_reads_all_the_remaining_entries_in_one_thread() {
    let fake = FakeExecutor::new();
    fake.capture_ok("s", Captured { stdout: Vec::new(), end: CaptureEnd::Exited(Some(0)) });
    let mut s = Scheduler::new();
    let first = s.offer(1, a_states_job(4), Some(1)).unwrap();
    assert!(s.offer(1, a_choices_job(9, 7), Some(1)).is_none());
    // The states job reads one entry, sees the flag, hands the rest back.
    let JobEvent::StatesDone { unread, counts } = run_job(first.1, &fake, &s.preempt, &mut |_| {}, &mut |_| {}) else { panic!() };
    assert_eq!((unread.len(), counts.on), (3, 1));
    s.resume(1, unread, counts, Some(1));
    // The slot is freed: the choices read goes first, and starting it lowers the flag.
    let next = s.finished(Some(1)).unwrap();
    assert!(is_a_choices_job(&next), "choices before the rest of the states");
    assert!(!s.preempt.load(Ordering::Acquire));
    let resumed = s.finished(Some(1)).unwrap();
    let mut seen = Vec::new();
    let JobEvent::StatesDone { unread, counts } = run_job(resumed.1, &fake, &s.preempt, &mut |e| seen.push(e), &mut |_| {}) else {
        panic!()
    };
    assert!(unread.is_empty(), "all the remaining entries in one go");
    assert_eq!((seen.len(), counts.on), (3, 4));
    assert!(s.finished(Some(1)).is_none());
}

#[test]
fn a_stale_delivery_frees_the_slot_and_starts_the_waiting_job() {
    let mut s = Scheduler::new();
    s.offer(1, a_states_job(1), Some(1)).unwrap();
    assert!(s.offer(2, a_choices_job(0, 3), Some(2)).is_none(), "a reopen while the first open's read runs");
    // The first open's last delivery arrives (stale for the open now showing): it still frees the slot.
    let next = s.finished(Some(2)).unwrap();
    assert_eq!((next.0, is_a_choices_job(&next)), (2, true));
}

#[test]
fn a_job_of_a_closed_or_replaced_open_is_never_started() {
    let mut s = Scheduler::new();
    s.offer(1, a_states_job(1), Some(1)).unwrap();
    s.offer(1, a_choices_job(0, 1), Some(1));
    s.offer(1, a_states_job(2), Some(1));
    // The launcher closed meanwhile: nothing waiting starts (a state command is the user's).
    assert!(s.finished(None).is_none());
    s.offer(1, a_states_job(1), Some(1)).unwrap();
    s.offer(1, a_choices_job(0, 1), Some(1));
    assert!(s.finished(Some(2)).is_none(), "another open");
}

#[test]
fn a_new_states_job_replaces_the_waiting_one() {
    let mut s = Scheduler::new();
    s.offer(1, a_states_job(1), Some(1)).unwrap();
    s.offer(1, a_states_job(2), Some(1));
    s.offer(1, a_states_job(3), Some(1));
    let Some((_, Job::States(jobs, _))) = s.finished(Some(1)) else { panic!() };
    assert_eq!(jobs.len(), 3);
    assert!(s.finished(Some(1)).is_none());
}

#[test]
fn an_unread_remainder_of_a_closed_open_is_dropped() {
    let mut s = Scheduler::new();
    s.offer(1, a_states_job(3), Some(1)).unwrap();
    s.resume(1, vec![a_state_job(1, "s")], Counts::default(), None);
    assert!(s.finished(None).is_none());
    s.resume(1, Vec::new(), Counts::default(), Some(1));
    assert!(s.finished(Some(1)).is_none(), "nothing unread, nothing queued");
}

/// The menu closing (or failing) raises the flag, so a running states read stops after
/// its current entry instead of running the user's `state` commands for a dead menu.
#[test]
fn a_closed_menu_stops_the_running_states_read_after_its_entry() {
    let fake = FakeExecutor::new();
    fake.capture_ok("s", Captured { stdout: Vec::new(), end: CaptureEnd::Exited(Some(0)) });
    let mut s = Scheduler::new();
    let first = s.offer(1, a_states_job(4), Some(1)).unwrap();
    assert!(!s.preempt.load(Ordering::Acquire));
    s.stop();
    let JobEvent::StatesDone { unread, counts } = run_job(first.1, &fake, &s.preempt, &mut |_| {}, &mut |_| {}) else { panic!() };
    assert_eq!((unread.len(), counts.on), (3, 1), "one entry read, the rest left");
    s.resume(1, unread, counts, None);
    assert!(s.finished(None).is_none(), "and never started again");
}

/// The spawn the glue really makes is wrapped when the runtime dir has systemd's
/// private socket, bare when it has none.
#[test]
fn the_glues_spawn_follows_the_runtime_dirs_systemd() {
    let bin = Bin::new("spawn", &["shell-ctl"]);
    let dirs = [bin.0.clone()];
    let argv = strings(&["shell-ctl", "reload-bar"]);
    let with_systemd = bin.0.join("rt-systemd");
    std::fs::create_dir_all(with_systemd.join("systemd")).unwrap();
    std::fs::write(with_systemd.join("systemd/private"), "").unwrap();
    let fake = FakeExecutor::new();
    fake.ok(&run::wrap(Spawner::SystemdScope, argv.clone()).join(" "), "");
    spawn_builtin(&fake, &with_systemd, &dirs, argv.clone()).unwrap();
    assert!(fake.calls()[0].starts_with("systemd-run --user --scope"), "{:?}", fake.calls());
    let bare = bin.0.join("rt-bare");
    std::fs::create_dir_all(&bare).unwrap();
    let fake = FakeExecutor::new();
    fake.ok("shell-ctl reload-bar", "");
    spawn_builtin(&fake, &bare, &dirs, argv).unwrap();
    assert_eq!(fake.calls(), ["shell-ctl reload-bar"]);
}

/// A replayed verb that was refused is logged (by id); one that was done is not.
#[test]
fn a_refused_verb_reply_is_a_refusal() {
    use alpheus_common::launcher_ipc::Reply;
    assert!(is_refusal(&Reply::Error("no".into())) && is_refusal(&Reply::Busy));
    assert!(!is_refusal(&Reply::Done) && !is_refusal(&Reply::Cancelled));
}

/// The clipboard built-in shows only when the history is on AND a protocol is bound.
#[test]
fn the_clipboard_builtin_needs_the_history_on_and_bound() {
    use crate::clipboard::app::availability;
    use crate::launcher::tools::catalog::{Catalog, EntryKind};
    let shows = |enabled, bound| {
        let ctx = context(
            &BarConfig::default(),
            &LauncherConfig::default(),
            false,
            join_answers(false, availability(enabled, bound).is_ok(), false),
        );
        let cat = Catalog::build(&LauncherConfig::default().tools, &ctx, &mut |_| {});
        cat.entries.iter().any(|e| matches!(e.kind, EntryKind::Builtin(i) if BUILTINS[i].id == "clipboard"))
    };
    assert!(shows(true, true));
    assert!(!shows(true, false), "enabled with no protocol bound");
    assert!(!shows(false, true) && !shows(false, false));
}
