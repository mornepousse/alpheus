//! The Tools menu's wiring on the daemon's state: what the catalog's `Context` is made of, the
//! built-ins replayed after the box closed, the reader's one-job-at-a-time schedule and its mail, and
//! the confirmation question's timer.
//!
//! The catalog, the navigation and the configuration are pure and live in the files beside this one.
//! This file is the part that touches the rest of the daemon. When the menu opens, it builds the
//! catalog from the configuration it is given and the app's own state; it starts states and choices
//! reads on the reader's short-lived thread (`read.rs`), one at a time, and adopts their mail into the
//! open launcher; after the box has closed it runs the built-in that was chosen, either by replaying
//! the daemon's own verb or by spawning the configured command; and it keeps one `DeadlineTimer`,
//! armed only while a confirmation question is pending and disarmed by `launcher_close` and
//! `launcher_fail`. The functions that decide something (`context`, `tab_argv`, `builtin_argv`,
//! `Scheduler`) take plain values, so they are tested without an `App`.
//!
//! The path of a built-in: Enter on its row first reaches `tools_prepare`, which checks the program
//! while the box is still open (a missing one is said in the box, which stays open). Otherwise the
//! launcher closes, then `run_builtin` replays the daemon's verb, or `tools_spawn` hands the argv to
//! the launcher's own start function, in its own scope.
//!
//! Never:
//! - start a process itself, through a shell or otherwise: a built-in that spawns goes through the
//!   launcher's start function, exactly once (a guardrail, `the_tools_glue_spawns_only_through_launch`);
//! - write a name, a command, a choice or a line into a log line: only fixed words, a built-in's id,
//!   its position and counts (a guardrail, `the_tools_logs_are_fixed_words`);
//! - call the clipboard's, the shield's or the diagnostics tab's own functions or types: `join.rs` is
//!   the one file that does (a guardrail, `the_parallel_branches_are_named_only_in_tools_join`, looks
//!   in this folder's other files for the names of those functions and types);
//! - keep a thread, a timer or a catalog while the menu is closed.
//!
//! See: docs/features/tools-menu.md, and docs/decisions.md ("A built-in replays the daemon's own verb
//! after the box has closed", "A tab opens through `[actions] open_tab`, else in the terminal",
//! "Everything a built-in spawns runs in its own scope", "The Tools menu reads states and choices one
//! job at a time on a short-lived thread").

use crate::actions::substitute;
use crate::config::{BarConfig, ModuleId, QuickConfig};
use crate::deadline::DeadlineTimer;
use crate::launcher::config::LauncherConfig;
use crate::launcher::model::Launcher;
use crate::launcher::run::{self, RunContext};
use crate::launcher::scan::{ScanEnv, has_program};
use crate::launcher::tools;
use crate::launcher::tools::catalog::{BUILTINS, Builtin, BuiltinAction, Catalog, Context, Entry, EntryKind};
use crate::launcher::tools::diag::{self, Scan, Subnet};
use crate::launcher::tools::nav::{Counts, Job, StateJob};
use crate::launcher::tools::read::{self, JobEvent};
use crate::log::write as log;
use crate::quick::Toggle;
use crate::startup::Mail;
use crate::timer::shutdown::ShutdownChoice;
use crate::wayland::App;
use alpheus_common::launcher_ipc::{DndVerb, Reply, Request, TimerVerb};
use alpheus_common::paths;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The three answers of `join.rs` (the clipboard, the shield, the diagnostics tab), and whether
/// `nmap` is on the `$PATH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Join {
    pub shield: bool,
    pub clipboard: bool,
    pub diagnostics: bool,
    pub nmap: bool,
}

/// What decides which built-ins show, from the configs and what the app knows.
pub fn context(bar: &BarConfig, launcher: &LauncherConfig, timer: bool, join: Join) -> Context {
    Context {
        quick: bar.modules.contains(&ModuleId::Quick),
        notifications: bar.modules.contains(&ModuleId::Notifications),
        timer,
        keep_awake: !Toggle::KeepAwake.config(&bar.quick).toggle.is_empty(),
        night_light: !Toggle::NightLight.config(&bar.quick).toggle.is_empty(),
        lock: !bar.quick.lock.is_empty(),
        session: crate::session::any_configured(&bar.quick),
        reload: !launcher.tools.reload.is_empty(),
        shield: join.shield,
        clipboard: join.clipboard,
        diagnostics: join.diagnostics,
        nmap: join.nmap,
        usb_reset: !launcher.tools.builtin.usb_reset.is_empty(),
        shutdown: timer && !bar.quick.poweroff.is_empty(),
        shutdown_left: None,
        // The captures' own answers are the daemon's, added by `tools_context`.
        captures: false,
        recording: false,
        annotate: false,
        ocr: false,
        record: false,
    }
}

/// Whether the program of `argv` (its first element) is on `path_dirs`; false for an empty argv.
fn program_is_present(argv: &[String], path_dirs: &[PathBuf]) -> bool {
    match argv.first() {
        Some(program) => has_program(path_dirs, program),
        None => false,
    }
}

/// The argv that opens the control center tab `slug`: `[actions] open_tab` with `{tab}` substituted
/// when its program is on the open's `$PATH`, else the launcher's terminal running
/// `alpheus --tab <slug>`; None when neither program is there. See docs/decisions.md, "A tab opens
/// through `[actions] open_tab`, else in the terminal".
pub fn tab_argv(open_tab: &[String], slug: &str, terminal: &[String], path_dirs: &[PathBuf]) -> Option<Vec<String>> {
    if program_is_present(open_tab, path_dirs) {
        return Some(open_tab.iter().map(|a| substitute(a, &[("{tab}", slug)])).collect());
    }
    if program_is_present(terminal, path_dirs) {
        let mut argv = terminal.to_vec();
        argv.extend(["alpheus".to_string(), "--tab".to_string(), slug.to_string()]);
        return Some(argv);
    }
    None
}

/// A replayed verb's answer that says it did nothing.
pub fn is_refusal(reply: &Reply) -> bool {
    matches!(reply, Reply::Error(_) | Reply::Busy)
}

/// The state of the built-in entries among `entries` whose state the daemon knows itself, as
/// (position in `entries`, on): Do not disturb is `dnd`, and a `[quick]` toggle is what `toggle_state`
/// says (None: unknown, so no entry for it). The positions come from the catalog, which builds each
/// `Builtin` index from `BUILTINS`, so the lookup finds its entry.
pub fn known_builtin_states(entries: &[Entry], dnd: bool, toggle_state: &dyn Fn(Toggle) -> Option<bool>) -> Vec<(usize, bool)> {
    let mut known = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let EntryKind::Builtin(builtin_index) = entry.kind else { continue };
        let Some(builtin) = BUILTINS.get(builtin_index) else { continue };
        match builtin.action {
            BuiltinAction::Dnd => known.push((index, dnd)),
            BuiltinAction::Toggle(toggle) => {
                if let Some(on) = toggle_state(toggle) {
                    known.push((index, on));
                }
            }
            _ => {}
        }
    }
    known
}

/// What a built-in that spawns runs.
pub struct Commands<'a> {
    pub quick: &'a QuickConfig,
    /// `[tools] reload`.
    pub reload: &'a [String],
    /// `[actions] open_tab`.
    pub open_tab: &'a [String],
    /// launcher.toml's `terminal`.
    pub terminal: &'a [String],
    /// launcher.toml's `terminal_hold`: where the diagnostics run, so `sudo` can ask there.
    pub terminal_hold: &'a [String],
    /// `[tools.builtin] usb_reset`.
    pub usb_reset: &'a [String],
    /// Where the host discovery's subnet is read.
    pub subnet: &'a dyn Subnet,
    /// The built-in's fixed choice, when it has one (the scan).
    pub choice: Option<&'a str>,
    /// What was typed for it. Never logged.
    pub text: Option<&'a str>,
    /// The open's `$PATH`.
    pub path_dirs: &'a [PathBuf],
}

/// `rest` behind `terminal` (its first element's program must be on the `$PATH`); `name` is what
/// the box says when the setting is empty.
fn in_terminal(terminal: &[String], name: &str, rest: Vec<String>, path_dirs: &[PathBuf]) -> Result<Option<Vec<String>>, String> {
    let Some(program) = terminal.first() else { return Err(run::failure(name, not_found())) };
    if !has_program(path_dirs, program) {
        return Err(run::failure(program, not_found()));
    }
    Ok(Some(terminal.iter().cloned().chain(rest).collect()))
}

/// The scan the choice names, with the box's answers: the argv in the held terminal. Err says, in
/// the box, what is missing: nothing is started.
fn scan_in_terminal(c: &Commands) -> Result<Option<Vec<String>>, String> {
    let Some(scan) = c.choice.and_then(Scan::from_label) else { return Err("the list changed: try again".to_string()) };
    let (target, ports) = match (scan, c.text) {
        (Scan::Ports, Some(text)) => match diag::split_ports(text) {
            Some((_, ports)) if !diag::valid_ports(ports) => return Err("ports: digits, commas, dashes".to_string()),
            Some((target, ports)) => (Some(target), Some(ports)),
            None => return Err("type the target, then the ports".to_string()),
        },
        (_, text) => (text, None),
    };
    let subnet = if scan == Scan::HostDiscovery { c.subnet.default_v4() } else { None };
    match diag::scan_argv(scan, target, ports, subnet.as_deref()) {
        Some(argv) => in_terminal(c.terminal_hold, "terminal_hold", argv, c.path_dirs),
        None if scan == Scan::HostDiscovery => Err("no network".to_string()),
        None => Err("no target".to_string()),
    }
}

/// The command a built-in spawns (never a shell, always the configured argv or the `alpheus --tab`
/// fallback): None for one that replays a verb instead; Err (said in the box, which stays open)
/// when its program is not on the `$PATH`.
pub fn builtin_argv(b: &Builtin, c: &Commands) -> Result<Option<Vec<String>>, String> {
    let argv: Vec<String> = match b.action {
        BuiltinAction::Toggle(t) => t.config(c.quick).toggle.clone(),
        BuiltinAction::Lock => c.quick.lock.clone(),
        BuiltinAction::Reload => c.reload.to_vec(),
        BuiltinAction::Tab => {
            let slug = b.tab_slug().unwrap_or_default();
            return match tab_argv(c.open_tab, slug, c.terminal, c.path_dirs) {
                Some(argv) => Ok(Some(argv)),
                None => {
                    // The program named in the message is the one the user configured: `open_tab`'s,
                    // else the terminal's, else the word "terminal".
                    let program = match c.open_tab.first().or(c.terminal.first()) {
                        Some(program) => program.as_str(),
                        None => "terminal",
                    };
                    Err(run::failure(program, not_found()))
                }
            };
        }
        BuiltinAction::Scan => return scan_in_terminal(c),
        BuiltinAction::UsbEvents => return in_terminal(c.terminal_hold, "terminal_hold", diag::usb_events_argv(), c.path_dirs),
        BuiltinAction::UsbReset => {
            if c.usb_reset.is_empty() {
                return Err("nothing to run".to_string());
            }
            return in_terminal(c.terminal_hold, "terminal_hold", c.usb_reset.to_vec(), c.path_dirs);
        }
        // `open_tab`'s script takes a tab and nothing else, so a preset always runs the TUI itself.
        BuiltinAction::Preset(preset) => {
            let Some(tab) = tools::join::DIAGNOSTICS_TAB else { return Err("nothing to run".to_string()) };
            if !has_program(c.path_dirs, "alpheus") {
                return Err(run::failure("alpheus", not_found()));
            }
            let rest = ["alpheus", "--tab", tab, "--preset", preset.slug()].map(String::from).to_vec();
            return in_terminal(c.terminal, "terminal", rest, c.path_dirs);
        }
        BuiltinAction::Quick(_)
        | BuiltinAction::Dnd
        | BuiltinAction::Notes
        | BuiltinAction::Timer
        | BuiltinAction::Shutdown
        | BuiltinAction::Clipboard
        | BuiltinAction::Emoji
        | BuiltinAction::Power
        | BuiltinAction::Shield
        | BuiltinAction::Kill
        | BuiltinAction::Capture(_) => return Ok(None),
    };
    let Some(program) = argv.first() else { return Err("nothing to run".to_string()) };
    if !has_program(c.path_dirs, program) {
        return Err(run::failure(program, not_found()));
    }
    Ok(Some(argv))
}

fn not_found() -> alpheus_common::exec::ExecError {
    alpheus_common::exec::ExecError::NotFound
}

/// Starts `argv` for a built-in in its own scope when `runtime_dir` has systemd's private socket,
/// bare otherwise; the program is looked up on `path_dirs` first. See docs/decisions.md, "Everything
/// a built-in spawns runs in its own scope".
pub fn spawn_builtin(
    exec: &dyn alpheus_common::exec::Executor,
    runtime_dir: &std::path::Path,
    path_dirs: &[PathBuf],
    argv: Vec<String>,
) -> Result<(), String> {
    let ctx = RunContext { exec, spawner: run::spawner(runtime_dir), history_path: None, path_dirs };
    run::launch(&ctx, argv)
}

/// The reader's schedule: one job at a time, a waiting choices read first (it is what the person
/// is looking at), the unread states behind it. A job belongs to an open (`generation`); one whose
/// open is gone is never started (a state command is the user's), and any last delivery, stale
/// ones too, frees the slot.
pub struct Scheduler {
    running: bool,
    choices: Option<(u64, Job)>,
    states: Option<(u64, Job)>,
    /// Raised while a choices read waits behind a running job: the states read stops after its
    /// current entry. Lowered each time a job starts.
    pub preempt: Arc<AtomicBool>,
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler { running: false, choices: None, states: None, preempt: Arc::new(AtomicBool::new(false)) }
    }

    /// Queues `job` for open `generation`; the job to start now, if the reader is free.
    pub fn offer(&mut self, generation: u64, job: Job, current: Option<u64>) -> Option<(u64, Job)> {
        match job {
            Job::Choices { .. } => {
                if self.running {
                    self.preempt.store(true, Ordering::Release);
                }
                self.choices = Some((generation, job));
            }
            Job::States(..) => self.states = Some((generation, job)),
        }
        self.next(current)
    }

    /// The open is gone (closed or failed): a running states read stops after its current entry,
    /// so a dead menu's user `state` commands do not go on.
    pub fn stop(&self) {
        self.preempt.store(true, Ordering::Release);
    }

    /// The states a preempted read left, behind whatever waits (nothing when the open is gone).
    pub fn resume(&mut self, generation: u64, unread: Vec<StateJob>, counts: Counts, current: Option<u64>) {
        if !unread.is_empty() && current == Some(generation) {
            self.states = Some((generation, Job::States(unread, counts)));
        }
    }

    /// A last delivery arrived (or a start failed): the slot is free; the job to start next.
    pub fn finished(&mut self, current: Option<u64>) -> Option<(u64, Job)> {
        self.running = false;
        self.next(current)
    }

    fn next(&mut self, current: Option<u64>) -> Option<(u64, Job)> {
        if self.running {
            return None;
        }
        // What belongs to another open is dropped here, not kept for a later one.
        let (choices, states) = (of_open(self.choices.take(), current), of_open(self.states.take(), current));
        let (job, rest) = match choices {
            Some(c) => (Some(c), states),
            None => (states, None),
        };
        self.states = rest;
        if job.is_some() {
            self.preempt.store(false, Ordering::Release);
            self.running = true;
        }
        job
    }
}

/// `job` if it belongs to the open `current`, else None.
fn of_open(job: Option<(u64, Job)>, current: Option<u64>) -> Option<(u64, Job)> {
    match job {
        Some((generation, job)) if Some(generation) == current => Some((generation, job)),
        _ => None,
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler::new()
    }
}

/// The glue's state on `App`: the question's timer and the reader's schedule.
pub struct ToolsApp {
    pub(crate) timer: DeadlineTimer,
    pub(crate) scheduler: Scheduler,
}

impl ToolsApp {
    pub fn new() -> ToolsApp {
        ToolsApp { timer: DeadlineTimer::new(), scheduler: Scheduler::new() }
    }
}

impl Default for ToolsApp {
    fn default() -> Self {
        ToolsApp::new()
    }
}

impl App {
    /// The catalog's `Context` from this app's configs.
    fn tools_context(&self, launcher: &LauncherConfig) -> Context {
        let path_dirs = ScanEnv::from_env(|k| std::env::var_os(k)).path_dirs;
        let join = Join {
            shield: tools::join::shield_available(self),
            clipboard: tools::join::clipboard_available(self),
            diagnostics: tools::join::DIAGNOSTICS_TAB.is_some(),
            nmap: has_program(&path_dirs, "nmap"),
        };
        let tools = &self.bar.config.captures;
        let mut c = Context {
            captures: self.screencopy.is_some(),
            recording: self.capture.is_recording(),
            annotate: program_is_present(&tools.annotate, &path_dirs),
            ocr: program_is_present(&tools.ocr, &path_dirs),
            record: program_is_present(&tools.record, &path_dirs),
            ..context(&self.bar.config, launcher, self.timer.enabled, join)
        };
        // A shutdown pending: the entry's first choice cancels it, with the time left.
        c.shutdown_left = self.timer.shutdown.map(|s| s.left(crate::timer::app::unix_ms()));
        c
    }

    /// The catalog of this open (rebuilt at each open, dropped at close); its notes are logged
    /// only when `notes` (a Tools open: a Mod+Space open says nothing about it).
    pub(crate) fn tools_catalog(&self, config: &LauncherConfig, notes: bool) -> Catalog {
        // A boxed closure, because the two branches are different closures of one type: a note
        // sink that writes to the log, or one that drops it.
        let mut note: Box<dyn FnMut(&str)> = if notes { Box::new(log) } else { Box::new(|_| {}) };
        Catalog::build(&config.tools, &self.tools_context(config), &mut *note)
    }

    /// The menu is gone: a running states read stops after its entry.
    pub(crate) fn tools_reads_stop(&self) {
        self.tools.scheduler.stop();
    }

    /// The states the daemon knows itself (do-not-disturb, the `[quick]` toggles), of the entries
    /// of `model`'s catalog (may scan `/proc`: read only when the Tools mode is entered).
    pub(crate) fn tools_known_states(&self, model: &Launcher) -> Vec<(usize, bool)> {
        let Some(tools) = model.tools() else { return Vec::new() };
        known_builtin_states(&tools.catalog.entries, self.dnd, &|toggle| self.quick_toggle_state(toggle))
    }

    /// Those states, set on a model that has a Tools state (a `--tools` open).
    pub(crate) fn tools_builtin_states(&self, model: &mut Launcher) {
        for (entry, on) in self.tools_known_states(model) {
            model.adopt_state(entry, Some(on));
        }
    }

    /// Those states, set on the open launcher: its Tools mode was just entered by the `/` jump.
    pub(crate) fn tools_adopt_builtin_states(&mut self) {
        let known = self.launcher.as_ref().map(|o| self.tools_known_states(&o.model)).unwrap_or_default();
        if let Some(open) = self.launcher.as_mut() {
            for (entry, on) in known {
                open.model.adopt_state(entry, Some(on));
            }
        }
    }

    /// `alpheus-launch --tools`: the Tools menu alone.
    pub(crate) fn launcher_open_tools(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        let config = crate::launcher::host::read_config(&paths::config_dir(), &mut |m| log(m));
        let now = crate::launcher::host::unix_now();
        let history = crate::launcher::host::load_history(&config, now);
        let catalog = self.tools_catalog(&config, true);
        let mut model = Launcher::for_tools(config, catalog, history, now);
        model.path_dirs = ScanEnv::from_env(|k| std::env::var_os(k)).path_dirs;
        self.tools_builtin_states(&mut model);
        let job = model.tools().and_then(|t| t.state_job(&model.config.tools));
        self.launcher_open(qh, model, None, requested_at);
        if let Some(job) = job {
            self.tools_offer(job);
        }
    }

    /// The generation of the open launcher, when one is open.
    fn tools_current(&self) -> Option<u64> {
        // `map` because clippy rejects the plain `match` here: None when no launcher is open.
        self.launcher.as_ref().map(|open| open.generation)
    }

    /// Hands `job` (for the open launcher) to the reader: started now, or waiting its turn.
    pub(crate) fn tools_offer(&mut self, job: Job) {
        let Some(generation) = self.tools_current() else { return };
        let current = Some(generation);
        let next = self.tools.scheduler.offer(generation, job, current);
        self.tools_start(next);
    }

    /// Starts `next` on the reader thread; a start that fails frees the slot and tries the next.
    fn tools_start(&mut self, mut next: Option<(u64, Job)>) {
        while let Some((generation, job)) = next.take() {
            let failed = match self.mail.clone() {
                Some(tx) => {
                    let exec = self.exec.clone();
                    let preempt = self.tools.scheduler.preempt.clone();
                    let (entry, token) = match &job {
                        Job::Choices { entry, token, .. } => (Some(*entry), *token),
                        Job::States(..) => (None, 0),
                    };
                    match read::spawn_job(job, exec, preempt, move |e| {
                        let _ = tx.send(match e {
                            JobEvent::State { entry, state } => Mail::ToolState { generation, entry, state },
                            JobEvent::StatesDone { unread, counts } => Mail::ToolStatesDone { generation, unread, counts },
                            JobEvent::Choices { entry, token, lines, failed } => {
                                Mail::ToolChoices { generation, token, entry, lines, failed }
                            }
                        });
                    }) {
                        Ok(()) => None,
                        Err(_) => Some((entry, token)),
                    }
                }
                None => Some((None, 0)),
            };
            let Some((entry, token)) = failed else { return };
            log("tools read failed");
            if let Some(entry) = entry {
                self.tools_adopt_choices(generation, token, entry, Vec::new(), true);
            }
            next = self.tools.scheduler.finished(self.tools_current());
        }
    }

    /// One entry's state, from the reader: the open launcher with that generation shows its switch.
    pub(crate) fn tools_state(&mut self, generation: u64, entry: usize, state: Option<bool>) {
        self.launcher_guarded("tools state", move |app| {
            let Some(open) = app.launcher.as_mut().filter(|o| o.generation == generation) else { return };
            open.model.adopt_state(entry, state);
            app.launcher_redraw();
        });
    }

    /// The states read is over: what it left unread waits behind a choices read, and the slot is free.
    pub(crate) fn tools_states_done(&mut self, generation: u64, unread: Vec<StateJob>, counts: Counts) {
        let current = self.tools_current();
        self.tools.scheduler.resume(generation, unread, counts, current);
        let next = self.tools.scheduler.finished(current);
        self.tools_start(next);
    }

    /// A `choices_cmd`'s lines, adopted only by the open launcher whose generation and token match;
    /// the slot is free either way.
    pub(crate) fn tools_choices(&mut self, generation: u64, token: u64, entry: usize, lines: Vec<String>, failed: bool) {
        self.tools_adopt_choices(generation, token, entry, lines, failed);
        let next = self.tools.scheduler.finished(self.tools_current());
        self.tools_start(next);
    }

    fn tools_adopt_choices(&mut self, generation: u64, token: u64, entry: usize, lines: Vec<String>, failed: bool) {
        self.launcher_guarded("tools choices", move |app| {
            let Some(open) = app.launcher.as_mut().filter(|o| o.generation == generation) else { return };
            if open.model.adopt_choices(entry, token, lines, failed) {
                app.launcher_redraw();
            }
        });
    }

    /// The timer follows the pending question's end; none without one.
    pub(crate) fn tools_timer_sync(&mut self) {
        // None when no launcher is open, or the open one has no question pending.
        let at = self.launcher.as_ref().and_then(|o| o.model.tools_deadline());
        self.tools.timer.sync_opt(self.loop_handle.as_ref(), at, App::tools_tick);
    }

    /// The timer fired (its source is dropped): the question's 3 s are up, or no launcher is left.
    pub(crate) fn tools_tick(app: &mut App, now: Instant) {
        app.tools.timer.fired();
        app.launcher_guarded("tools timer", move |app| {
            let expired = match app.launcher.as_mut() {
                Some(open) => open.model.tools_expire(now),
                None => false,
            };
            if expired {
                app.launcher_redraw();
            }
        });
        app.tools_timer_sync();
    }

    /// What a built-in that spawns will run, checked while the box is still open: Err stays in the
    /// box. None for a built-in that replays a verb, or when the box is gone.
    pub(crate) fn tools_prepare(&self, i: usize, choice: Option<&str>, text: Option<&str>) -> Result<Option<Vec<String>>, String> {
        let (Some(b), Some(open)) = (BUILTINS.get(i), self.launcher.as_ref()) else { return Ok(None) };
        let commands = Commands {
            quick: &self.bar.config.quick,
            reload: &open.model.config.tools.reload,
            open_tab: &self.bar.config.actions.open_tab,
            terminal: &open.model.config.terminal,
            terminal_hold: &open.model.config.terminal_hold,
            usb_reset: &open.model.config.tools.builtin.usb_reset,
            subnet: &diag::ProcSubnet::new(),
            choice,
            text,
            path_dirs: &open.model.path_dirs,
        };
        builtin_argv(b, &commands)
    }

    /// Replays the built-in `i` after the box closed, as the socket verb would; `argv` is what
    /// `tools_prepare` returned. One log line, by id.
    pub(crate) fn run_builtin(&mut self, qh: &QueueHandle<App>, i: usize, argv: Option<Vec<String>>) {
        let Some(b) = BUILTINS.get(i) else { return };
        let id = b.id;
        log(&format!("tools: built-in {id}"));
        let reply = match b.action {
            BuiltinAction::Quick(verb) => Some(self.quick_verb(&Request::Quick(verb))),
            BuiltinAction::Dnd => Some(self.notifications_verb(&Request::Dnd(DndVerb::Toggle))),
            BuiltinAction::Toggle(_)
            | BuiltinAction::Lock
            | BuiltinAction::Reload
            | BuiltinAction::Tab
            | BuiltinAction::Scan
            | BuiltinAction::UsbEvents
            | BuiltinAction::UsbReset
            | BuiltinAction::Preset(_) => {
                if let Some(argv) = argv {
                    self.tools_spawn(id, argv);
                }
                None
            }
            BuiltinAction::Notes => Some(self.notes_verb(qh, &Request::Notes)),
            BuiltinAction::Timer => Some(self.timer_verb(&Request::Timer(TimerVerb::Popup))),
            // Chosen without a duration (never from the menu: it has choices): nothing to do.
            BuiltinAction::Shutdown => None,
            BuiltinAction::Clipboard => {
                tools::join::open_clipboard(self, qh);
                None
            }
            BuiltinAction::Emoji => {
                self.tools_open_emoji(qh);
                None
            }
            BuiltinAction::Power => Some(self.power_verb(qh, &Request::Power)),
            BuiltinAction::Shield => {
                tools::join::open_shield(self);
                None
            }
            // The kill list replaces nothing: the box that held this entry is already closed.
            BuiltinAction::Kill => {
                self.launcher_open_processes(qh, Instant::now());
                None
            }
            BuiltinAction::Capture(verb) => Some(self.capture_verb(&Request::Capture(verb))),
        };
        if reply.as_ref().is_some_and(is_refusal) {
            log(&format!("tools: built-in {id} refused"));
        }
    }

    /// Replays the shutdown built-in's chosen label after the box closed, through the timer's own
    /// verbs (`--timer shutdown <secs>` or `shutdown-cancel`): one path for every way in. One log
    /// line, by id.
    pub(crate) fn run_builtin_choice(&mut self, i: usize, choice: &str) {
        let Some(b) = BUILTINS.get(i) else { return };
        let id = b.id;
        log(&format!("tools: built-in {id}"));
        let verb = match (b.action, crate::timer::shutdown::choice_of(choice)) {
            (BuiltinAction::Shutdown, Some(ShutdownChoice::Cancel)) => TimerVerb::ShutdownCancel,
            (BuiltinAction::Shutdown, Some(ShutdownChoice::After(ms))) => {
                // At most 3 h: far inside a u32 of seconds.
                TimerVerb::Shutdown(u32::try_from(ms / 1000).unwrap_or(u32::MAX))
            }
            _ => {
                log(&format!("tools: built-in {id} refused"));
                return;
            }
        };
        if is_refusal(&self.timer_verb(&Request::Timer(verb))) {
            log(&format!("tools: built-in {id} refused"));
        }
    }

    /// `argv` in its own scope, looked up on the daemon's own `$PATH`.
    fn tools_spawn(&mut self, id: &str, argv: Vec<String>) {
        let path_dirs = ScanEnv::from_env(|k| std::env::var_os(k)).path_dirs;
        if spawn_builtin(self.exec.as_ref(), &paths::runtime_dir(), &path_dirs, argv).is_err() {
            log(&format!("tools: built-in {id} could not start"));
        }
    }

    /// The emoji picker: the apps launcher with the `:` prefix typed.
    fn tools_open_emoji(&mut self, qh: &QueueHandle<App>) {
        self.launcher_open_apps(qh, Instant::now());
        if let Some(open) = self.launcher.as_mut() {
            open.model.set_query(":");
        }
        self.launcher_redraw();
    }
}

#[cfg(test)]
mod tests;
