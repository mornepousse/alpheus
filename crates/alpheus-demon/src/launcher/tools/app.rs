//! The Tools menu's glue (tools spec §§ 1, 5, 6): what the catalog's `Context` is made of, the
//! built-ins replayed after the box closed, the reader's one-job-at-a-time schedule and its mail,
//! and the confirmation question's one `DeadlineTimer` (armed only while a question is pending,
//! disarmed by `launcher_close` and `launcher_fail`). The pure parts (`context`, `tab_argv`,
//! `builtin_argv`, `Scheduler`) take plain values so they are tested with no `App`.

use crate::actions::substitute;
use crate::config::{BarConfig, ModuleId, QuickConfig};
use crate::deadline::DeadlineTimer;
use crate::launcher::config::LauncherConfig;
use crate::launcher::model::Launcher;
use crate::launcher::run::{self, RunContext};
use crate::launcher::scan::{ScanEnv, has_program};
use crate::launcher::tools;
use crate::launcher::tools::catalog::{BUILTINS, Builtin, BuiltinAction, Catalog, Context, EntryKind};
use crate::launcher::tools::nav::{Counts, Job, StateJob};
use crate::launcher::tools::read::{self, JobEvent};
use crate::log::write as log;
use crate::quick::Toggle;
use crate::startup::Mail;
use crate::wayland::App;
use alpheus_commun::launcher_ipc::{DndVerb, Reply, Request, TimerVerb};
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The three answers of `tools_join` (the clipboard, the shield, the diagnostics tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Join {
    pub shield: bool,
    pub clipboard: bool,
    pub diagnostics: bool,
}

/// What decides which built-ins show (spec § 3), from the configs and what the app knows.
pub fn context(bar: &BarConfig, launcher: &LauncherConfig, timer: bool, join: Join) -> Context {
    let has = |m| bar.modules.contains(&m);
    Context {
        quick: has(ModuleId::Quick),
        notifications: has(ModuleId::Notifications),
        timer,
        keep_awake: !Toggle::KeepAwake.config(&bar.quick).toggle.is_empty(),
        night_light: !Toggle::NightLight.config(&bar.quick).toggle.is_empty(),
        lock: !bar.quick.lock.is_empty(),
        session: crate::session::any_configured(&bar.quick),
        reload: !launcher.tools.reload.is_empty(),
        shield: join.shield,
        clipboard: join.clipboard,
        diagnostics: join.diagnostics,
    }
}

/// The argv that opens the control center tab `slug` (plan decision 9): `[actions] open_tab` with
/// `{tab}` substituted when its program is on the open's `$PATH`, else the launcher's terminal
/// running `alpheus --tab <slug>`; None when neither program is there.
pub fn tab_argv(open_tab: &[String], slug: &str, terminal: &[String], path_dirs: &[PathBuf]) -> Option<Vec<String>> {
    if open_tab.first().is_some_and(|p| has_program(path_dirs, p)) {
        return Some(open_tab.iter().map(|a| substitute(a, &[("{tab}", slug)])).collect());
    }
    if terminal.first().is_some_and(|p| has_program(path_dirs, p)) {
        return Some(terminal.iter().cloned().chain(["alpheus".into(), "--tab".into(), slug.into()]).collect());
    }
    None
}

/// A replayed verb's answer that says it did nothing.
pub fn is_refusal(reply: &Reply) -> bool {
    matches!(reply, Reply::Error(_) | Reply::Busy)
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
    /// The open's `$PATH`.
    pub path_dirs: &'a [PathBuf],
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
                None => Err(run::failure(c.open_tab.first().or(c.terminal.first()).map_or("terminal", String::as_str), not_found())),
            };
        }
        BuiltinAction::Quick(_)
        | BuiltinAction::Dnd
        | BuiltinAction::Notes
        | BuiltinAction::Timer
        | BuiltinAction::Clipboard
        | BuiltinAction::Emoji
        | BuiltinAction::Power
        | BuiltinAction::Shield => return Ok(None),
    };
    let program = argv.first().ok_or_else(|| "nothing to run".to_string())?;
    if !has_program(c.path_dirs, program) {
        return Err(run::failure(program, not_found()));
    }
    Ok(Some(argv))
}

fn not_found() -> alpheus_commun::exec::ExecError {
    alpheus_commun::exec::ExecError::NotFound
}

/// Starts `argv` for a built-in in its own scope when `runtime_dir` has systemd's private socket
/// (decision 10), bare otherwise; the program is looked up on `path_dirs` first.
pub fn spawn_builtin(
    exec: &dyn alpheus_commun::exec::Executor,
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
        let mine = |j: Option<(u64, Job)>| j.filter(|(g, _)| Some(*g) == current);
        let (choices, states) = (mine(self.choices.take()), mine(self.states.take()));
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
        let join = Join {
            shield: tools::join::shield_available(self),
            clipboard: tools::join::clipboard_available(self),
            diagnostics: tools::join::DIAGNOSTICS_TAB.is_some(),
        };
        context(&self.bar.config, launcher, self.timer.enabled, join)
    }

    /// The catalog of this open (rebuilt at each open, dropped at close); its notes are logged
    /// only when `notes` (a Tools open: a Mod+Space open says nothing about it).
    pub(crate) fn tools_catalog(&self, config: &LauncherConfig, notes: bool) -> Catalog {
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
        model
            .tools()
            .map(|t| {
                t.catalog
                    .entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| {
                        let EntryKind::Builtin(b) = e.kind else { return None };
                        match BUILTINS.get(b)?.action {
                            BuiltinAction::Dnd => Some((i, self.dnd)),
                            BuiltinAction::Toggle(t) => self.quick_toggle_state(t).map(|on| (i, on)),
                            _ => None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
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
        self.launcher.as_ref().map(|o| o.generation)
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
            let failed = match (self.mail.clone(), generation) {
                (Some(tx), generation) => {
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
                (None, _) => Some((None, 0)),
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
        let at = self.launcher.as_ref().and_then(|o| o.model.tools_deadline());
        self.tools.timer.sync_opt(self.loop_handle.as_ref(), at, App::tools_tick);
    }

    /// The timer fired (its source is dropped): the question's 3 s are up, or no launcher is left.
    pub(crate) fn tools_tick(app: &mut App, now: Instant) {
        app.tools.timer.fired();
        app.launcher_guarded("tools timer", move |app| {
            let expired = app.launcher.as_mut().is_some_and(|o| o.model.tools_expire(now));
            if expired {
                app.launcher_redraw();
            }
        });
        app.tools_timer_sync();
    }

    /// What a built-in that spawns will run, checked while the box is still open: Err stays in the
    /// box. None for a built-in that replays a verb, or when the box is gone.
    pub(crate) fn tools_prepare(&self, i: usize) -> Result<Option<Vec<String>>, String> {
        let (Some(b), Some(open)) = (BUILTINS.get(i), self.launcher.as_ref()) else { return Ok(None) };
        let commands = Commands {
            quick: &self.bar.config.quick,
            reload: &open.model.config.tools.reload,
            open_tab: &self.bar.config.actions.open_tab,
            terminal: &open.model.config.terminal,
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
            BuiltinAction::Toggle(_) | BuiltinAction::Lock | BuiltinAction::Reload | BuiltinAction::Tab => {
                if let Some(argv) = argv {
                    self.tools_spawn(id, argv);
                }
                None
            }
            BuiltinAction::Notes => Some(self.notes_verb(qh, &Request::Notes)),
            BuiltinAction::Timer => Some(self.timer_verb(&Request::Timer(TimerVerb::Popup))),
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
        };
        if reply.as_ref().is_some_and(is_refusal) {
            log(&format!("tools: built-in {id} refused"));
        }
    }

    /// `argv` in its own scope (decision 10), looked up on the daemon's own `$PATH`.
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
