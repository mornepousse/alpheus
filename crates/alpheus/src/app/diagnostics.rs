//! The Diag tab's state: what `systemctl`, `coredumpctl` and the health checks last said, the view
//! it shows (the overview, or one of the three views opened from it) and the pure rules that turn
//! those reads into rows. Nothing here runs a tool: the worker does, the tab only queues the reads.

use super::worker::{DiagRead, DiagResult, Domain, Op, Request};
use super::{App, Item, Loadable, Overlay, TOAST_TTL, Toast};
use crate::backends::{BResult, BackendError, CheckResult, PokeAnswer, UnitAction};
use crate::config::Check;
use crate::diagnostics::crashes::Crash;
use crate::diagnostics::names::valid_unit;
use crate::diagnostics::query::{JournalQuery, Preset, Scope};
use crate::diagnostics::show::Restarts;
use crate::diagnostics::units::FailedUnit;
use alpheus_common::compositor::{self, Compositor};
use alpheus_common::text::clean_line;
use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// A list read again and again: the last good one is kept when a read fails, with its error, so a
/// broken tool never shows a stale list dressed as fresh (the view draws the error above it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section<T> {
    /// None until a read has succeeded.
    pub items: Option<Vec<T>>,
    pub error: Option<BackendError>,
}

impl<T> Default for Section<T> {
    fn default() -> Self {
        Section { items: None, error: None }
    }
}

impl<T> Section<T> {
    pub fn set(&mut self, r: BResult<Vec<T>>) {
        match r {
            Ok(v) => {
                self.items = Some(v);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The last good list (empty before the first one).
    pub fn list(&self) -> &[T] {
        self.items.as_deref().unwrap_or(&[])
    }

    /// The tool is not installed: the section says so and has no selectable row.
    pub fn missing(&self) -> bool {
        matches!(self.error, Some(BackendError::Missing { .. }))
    }

    /// Nothing has been read yet, and nothing has failed.
    pub fn loading(&self) -> bool {
        self.items.is_none() && self.error.is_none()
    }
}

pub use super::report_views::{BootView, ReportView};

pub use super::journal::JournalView;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Overview,
    Journal(Box<JournalView>),
    Boot(BootView),
    Report(ReportView),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiagnosticsTab {
    pub failed_system: Section<FailedUnit>,
    pub failed_user: Section<FailedUnit>,
    pub loops_system: Section<Restarts>,
    pub loops_user: Section<Restarts>,
    pub crashes: Section<Crash>,
    /// The first line of `systemd-analyze time`.
    pub boot: Loadable<String>,
    /// The declared checks' last outcomes, in the config's order (empty before the first read).
    pub checks: Vec<CheckResult>,
    pub health: Option<HealthRead>,
    /// What the daemon answered to the last `failed-refresh`.
    pub daemon: Option<PokeAnswer>,
    pub view: View,
    /// The overview's selection, kept while a view covers it.
    pub saved_focus: usize,
    /// A key or a click has moved the selection since the view was shown: until then the row that
    /// is focused by default is not "already selected", so a first click only selects it.
    pub(super) touched: bool,
}

/// One focusable row of the overview, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagItem {
    /// A failing Session health row (index into `App::health_rows`): focusable to read its fix.
    HealthRow(usize),
    Failed(Scope, String),
    Loop(Scope, String),
    /// Index into the crashes.
    Crash(usize),
    Boot,
    Preset(Preset),
    Report,
}

/// `s` cut to `max` characters in the middle (`head…tail`): a unit's end (`-a.service`) is what tells
/// two long names apart.
pub fn middle_cut(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let tail = max.saturating_sub(1).div_ceil(2);
    let head = max.saturating_sub(1) - tail;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
    out
}

/// "Restart mnt-data.mount (system)?": what the unit confirmation asks.
pub fn unit_question(scope: Scope, unit: &str, action: UnitAction) -> String {
    unit_question_cut(scope, unit, action, 80)
}

/// The same with the unit name cut in the middle to `max_unit` characters: the dialog passes its
/// own width, so the wrapping never has to cut the name's end off.
pub fn unit_question_cut(scope: Scope, unit: &str, action: UnitAction, max_unit: usize) -> String {
    let verb = match action {
        UnitAction::Restart => "Restart",
        UnitAction::ResetFailed => "Clear the failed state of",
    };
    let unit = middle_cut(&clean_line(unit, 256), max_unit);
    match scope {
        Scope::User => format!("{verb} {unit} (user)?"),
        Scope::System => format!("{verb} {unit} (system)? systemd will ask for your password in this window."),
    }
}

impl DiagnosticsTab {
    /// The failed-units domain is failing for both managers (its re-read is slowed).
    pub fn failed_failing(&self) -> bool {
        self.failed_system.error.is_some() && self.failed_user.error.is_some()
    }

    pub fn apply(&mut self, result: DiagResult) {
        match result {
            DiagResult::Restarts { system, user } => {
                self.loops_system.set(system);
                self.loops_user.set(user);
            }
            DiagResult::Crashes(r) => self.crashes.set(r),
            DiagResult::BootTime(r) => self.boot = r.into(),
            DiagResult::Checks(results) => self.checks = results,
            // The journal, its picker and the boot report are kept by their views.
            DiagResult::ServiceUnits(_)
            | DiagResult::Journal(_)
            | DiagResult::JournalReadable(_)
            | DiagResult::BootReport { .. }
            | DiagResult::Report(_) => {}
        }
    }
}

/// How the session's Wayland socket looks from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaylandHealth {
    Unset,
    /// Set to this name, but no socket there.
    Missing(String),
    Reachable(String),
}

/// The compositor of this session and whether its IPC socket accepts a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositorHealth {
    pub name: &'static str,
    /// The environment variable that names it (`SWAYSOCK`…).
    pub var: &'static str,
    pub answers: bool,
}

/// What the worker found for the cheap health checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthRead {
    /// None: no compositor variable is set.
    pub compositor: Option<CompositorHealth>,
    pub wayland: WaylandHealth,
    /// The systemd user environment's `WAYLAND_DISPLAY` and compositor variable, nothing else.
    pub systemd_env: BResult<Vec<(String, String)>>,
    /// The same two as the TUI's own environment has them.
    pub own_env: Vec<(String, String)>,
}

fn describe(c: &Compositor) -> (&'static str, &'static str) {
    match c {
        Compositor::Sway { .. } => ("Sway", "SWAYSOCK"),
        Compositor::Hyprland { .. } => ("Hyprland", "HYPRLAND_INSTANCE_SIGNATURE"),
        Compositor::Mango { .. } => ("Mango", "MANGO_INSTANCE_SIGNATURE"),
        Compositor::Niri { .. } => ("Niri", "NIRI_SOCKET"),
    }
}

/// Connects to `path` and closes at once: nothing is ever written to a compositor.
fn socket_answers(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

fn wayland_health(var: &dyn Fn(&str) -> Option<OsString>) -> WaylandHealth {
    let Some(name) = var("WAYLAND_DISPLAY").filter(|v| !v.is_empty()) else { return WaylandHealth::Unset };
    let shown = clean_line(&name.to_string_lossy(), 64);
    let path = if Path::new(&name).is_absolute() {
        Some(PathBuf::from(&name))
    } else {
        var("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(|r| PathBuf::from(r).join(&name))
    };
    match path.and_then(|p| std::fs::metadata(p).ok()) {
        Some(m) if m.file_type().is_socket() => WaylandHealth::Reachable(shown),
        _ => WaylandHealth::Missing(shown),
    }
}

/// The cheap checks, read from the environment `var` gives (the process's own, in the worker).
pub fn health(b: &crate::backends::Backends, var: &dyn Fn(&str) -> Option<OsString>) -> HealthRead {
    let detected = compositor::detect(var);
    let comp = detected.as_ref().map(|c| {
        let (name, v) = describe(c);
        CompositorHealth { name, var: v, answers: socket_answers(&c.ipc_socket()) }
    });
    let mut names = vec!["WAYLAND_DISPLAY"];
    if let Some(c) = &comp {
        names.push(c.var);
    }
    let own_env = names
        .iter()
        .filter_map(|n| var(n).filter(|v| !v.is_empty()).map(|v| (n.to_string(), clean_line(&v.to_string_lossy(), 256))))
        .collect();
    HealthRead { compositor: comp, wayland: wayland_health(var), systemd_env: b.diagnostics.session_env(&names), own_env }
}

/// One line of the Session health section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthRow {
    pub ok: bool,
    pub text: String,
    /// What would fix it, shown and never run.
    pub fix: Option<String>,
}

pub struct HealthInputs<'a> {
    pub read: Option<&'a HealthRead>,
    pub failed_user: &'a Section<FailedUnit>,
    pub daemon: Option<&'a PokeAnswer>,
    /// The declared checks and their last outcomes (none until read).
    pub checks: &'a [(Check, Option<CheckResult>)],
}

fn row(ok: bool, text: impl Into<String>, fix: Option<String>) -> HealthRow {
    HealthRow { ok, text: text.into(), fix }
}

/// The Session health rows, in order: the compositor, `WAYLAND_DISPLAY`, the systemd user
/// environment, the failed user units, the daemon, then the declared checks. A check whose answer
/// has not come yet has no row.
pub fn health_rows(i: &HealthInputs) -> Vec<HealthRow> {
    let mut rows = Vec::new();
    if let Some(h) = i.read {
        rows.push(match &h.compositor {
            None => row(false, "no compositor detected: outside a session?", None),
            Some(c) if c.answers => row(true, format!("{} answers", c.name), None),
            Some(c) => row(false, format!("{} does not answer", c.name), None),
        });
        rows.push(match &h.wayland {
            WaylandHealth::Unset => row(false, "WAYLAND_DISPLAY is not set", None),
            WaylandHealth::Missing(n) => row(false, format!("WAYLAND_DISPLAY {n}: no such socket"), None),
            WaylandHealth::Reachable(n) => row(true, format!("WAYLAND_DISPLAY {n} reachable"), None),
        });
        match &h.systemd_env {
            Err(e) => rows.push(row(false, format!("systemd user environment unreadable: {e}"), None)),
            Ok(theirs) => {
                let before = rows.len();
                for (name, ours) in &h.own_env {
                    let fix = Some(format!("systemctl --user import-environment {name}"));
                    // The compositor variable's value is a path or a signature: only its name is shown.
                    let plain = name == "WAYLAND_DISPLAY";
                    match theirs.iter().find(|(n, _)| n == name) {
                        Some((_, v)) if v == ours => {}
                        Some((_, v)) if plain => {
                            rows.push(row(false, format!("systemd --user has {name}={v} (this session: {ours})"), fix))
                        }
                        None if plain => rows.push(row(false, format!("systemd --user has no {name} (this session: {ours})"), fix)),
                        Some(_) => rows.push(row(false, format!("systemd --user has a different {name}"), fix)),
                        None => rows.push(row(false, format!("systemd --user has no {name}"), fix)),
                    }
                }
                if rows.len() == before {
                    rows.push(row(true, "systemd --user environment matches this session", None));
                }
            }
        }
    }
    match (&i.failed_user.items, &i.failed_user.error) {
        (Some(v), _) if v.is_empty() => rows.push(row(true, "no failed user unit", None)),
        (Some(v), _) => rows.push(row(false, format!("{} failed user unit{}", v.len(), if v.len() == 1 { "" } else { "s" }), None)),
        (None, Some(e)) => rows.push(row(false, format!("failed user units unreadable: {e}"), None)),
        (None, None) => {}
    }
    match i.daemon {
        None => {}
        Some(PokeAnswer::Done) => rows.push(row(true, "the alpheus daemon answers", None)),
        Some(PokeAnswer::Older) => rows.push(row(true, "the alpheus daemon answers (it is older than this control center)", None)),
        Some(PokeAnswer::Unreachable(_)) => rows.push(row(false, "the alpheus daemon does not answer", None)),
    }
    for (check, result) in i.checks {
        let Some(r) = result else { continue };
        let name = clean_line(&check.name, 40);
        let fix = check.fix.as_ref().map(|f| clean_line(f, 200));
        rows.push(if r.ok {
            row(true, name, None)
        } else if r.detail.is_empty() {
            row(false, name, fix)
        } else {
            row(false, format!("{name}: {}", r.detail), fix)
        });
    }
    rows
}

/// "✓ all good" or "✗ N problems".
pub fn health_summary(rows: &[HealthRow]) -> String {
    match rows.iter().filter(|r| !r.ok).count() {
        0 => "✓ all good".into(),
        1 => "✗ 1 problem".into(),
        n => format!("✗ {n} problems"),
    }
}

impl App {
    /// The Session health rows as they stand.
    pub fn health_rows(&self) -> Vec<HealthRow> {
        let declared: Vec<(Check, Option<CheckResult>)> =
            self.config.diagnostics.checks.iter().enumerate().map(|(n, c)| (c.clone(), self.diag.checks.get(n).cloned())).collect();
        health_rows(&HealthInputs {
            read: self.diag.health.as_ref(),
            failed_user: &self.diag.failed_user,
            daemon: self.diag.daemon.as_ref(),
            checks: &declared,
        })
    }

    /// The reads made once per showing and on F5, not on the 5 s refresh: the loops, the crashes,
    /// the boot line, the declared checks, and the daemon check (a `failed-refresh`, which also
    /// makes the bar re-read its count).
    pub(crate) fn diagnostics_reads(&mut self) {
        let checks = self.config.diagnostics.checks.clone();
        self.requests.push(Request::Diag(DiagRead::Restarts(self.config.diagnostics.restart_loop)));
        self.requests.push(Request::Diag(DiagRead::Crashes));
        self.requests.push(Request::Diag(DiagRead::BootTime));
        self.requests.push(Request::Diag(DiagRead::Checks(checks)));
        self.requests.push(Request::Do(Op::FailedRefresh));
    }

    /// An answer of a Diag read: the journal view's own first, else the tab's lists.
    pub(crate) fn diag_update(&mut self, read: DiagRead, result: DiagResult, now: Instant) {
        if let Some(result) = self.journal_try(&read, result, now)
            && let Some(result) = self.report_views_try(result, now)
        {
            self.diag.apply(result);
        }
    }

    /// F5: everything again, now (not in the journal view, which reads for itself).
    pub fn diagnostics_refresh(&mut self, now: Instant) {
        if self.tab == super::Tab::Diagnostics && !self.diagnostics_refresh_held() {
            self.refresh_tab(now);
            self.diagnostics_reads();
        }
    }

    /// Whether the 5 s refresh is held back: the journal view makes its own reads, and the single
    /// worker thread is never queued behind more than one of them.
    pub(crate) fn diagnostics_refresh_held(&self) -> bool {
        self.tab == super::Tab::Diagnostics && matches!(self.diag.view, View::Journal(_))
    }

    /// The overview's focusable rows, in the order the view draws them. A section whose tool is
    /// missing has none.
    pub(crate) fn diag_items(&self) -> Vec<Item> {
        if self.diag.view != View::Overview {
            return Vec::new();
        }
        let d = &self.diag;
        let mut v: Vec<DiagItem> = Vec::new();
        v.extend(self.health_rows().iter().enumerate().filter(|(_, r)| !r.ok).map(|(i, _)| DiagItem::HealthRow(i)));
        for (scope, section) in [(Scope::System, &d.failed_system), (Scope::User, &d.failed_user)] {
            if !section.missing() {
                v.extend(section.list().iter().map(|u| DiagItem::Failed(scope, u.unit.clone())));
            }
        }
        for (scope, section) in [(Scope::System, &d.loops_system), (Scope::User, &d.loops_user)] {
            if !section.missing() {
                v.extend(section.list().iter().map(|l| DiagItem::Loop(scope, l.unit.clone())));
            }
        }
        if !d.crashes.missing() {
            v.extend((0..d.crashes.list().len()).map(DiagItem::Crash));
        }
        if !d.boot.is_failed() {
            v.push(DiagItem::Boot);
        }
        v.extend(Preset::ALL.map(DiagItem::Preset));
        v.push(DiagItem::Report);
        v.into_iter().map(Item::Diag).collect()
    }

    pub(super) fn open_diag_view(&mut self, view: View) {
        self.diag.saved_focus = self.focus;
        self.focus = 0;
        self.diag.view = view;
        self.diag.touched = false;
    }

    /// Esc in a view: back to the overview, its selection where it was.
    pub(super) fn close_diag_view(&mut self) {
        self.diag.view = View::Overview;
        self.diag.touched = true; // the selection is where the person left it
        self.focus = self.diag.saved_focus;
        self.clamp_focus();
    }

    /// Enter on an overview row.
    /// A key was pressed on the tab.
    pub(crate) fn diag_touch(&mut self) {
        self.diag.touched = true;
    }

    pub(crate) fn diag_activate(&mut self, item: DiagItem) {
        let lines = self.config.diagnostics.journal_lines;
        match item {
            DiagItem::HealthRow(_) => {}
            DiagItem::Failed(scope, unit) | DiagItem::Loop(scope, unit) => {
                self.journal_open(JournalQuery { unit: Some((scope, unit)), ..JournalQuery::new(Preset::ServiceLogs, lines) });
            }
            DiagItem::Crash(i) => {
                let Some(c) = self.diag.crashes.list().get(i) else { return };
                let secs = c.time_us / 1_000_000;
                self.journal_open(JournalQuery {
                    since_until: Some((secs.saturating_sub(60), secs.saturating_add(5))),
                    ..JournalQuery::new(Preset::ServiceLogs, lines)
                });
            }
            DiagItem::Boot => self.boot_open(),
            DiagItem::Preset(p) => self.journal_open(JournalQuery::new(p, lines)),
            DiagItem::Report => self.report_open(),
        }
    }

    /// A click on an overview row. A unit is selected first (its keys then apply), and a second
    /// click opens its journal; the other rows act at once. True when it was handled here.
    pub(crate) fn diag_click(&mut self, i: usize) -> bool {
        let was = self.focus == i && self.diag.touched;
        self.diag.touched = true;
        self.focus = i;
        match self.focused() {
            Some(Item::Diag(DiagItem::HealthRow(_))) => {}
            Some(Item::Diag(item @ (DiagItem::Failed(..) | DiagItem::Loop(..)))) => {
                if was {
                    self.diag_activate(item);
                }
            }
            Some(Item::Diag(item)) => self.diag_activate(item),
            _ => return false,
        }
        true
    }

    /// The unit under the cursor, when it is a failed or looping one.
    fn diag_unit(&self) -> Option<(Scope, String)> {
        match self.focused()? {
            Item::Diag(DiagItem::Failed(s, u) | DiagItem::Loop(s, u)) => Some((s, u)),
            _ => None,
        }
    }

    /// Whether `r` and `c` do something on the row under the cursor (the key hints follow).
    pub fn unit_keys_apply(&self) -> bool {
        self.diag_unit().is_some_and(|(scope, _)| scope == Scope::User || self.config.diagnostics.system_actions)
    }

    /// `r` and `c` on a unit. Restarts ask first, a user unit's reset does not; a system unit's
    /// actions run in the TUI's own window (systemd asks for the password there).
    fn unit_action(&mut self, action: UnitAction) {
        let Some((scope, unit)) = self.diag_unit() else { return };
        if !self.unit_keys_apply() {
            return;
        }
        match (scope, action) {
            (Scope::User, UnitAction::ResetFailed) => self.send_op(Op::UnitUser(action, unit)),
            _ => self.overlay = Overlay::UnitConfirm { scope, unit, action, yes_focused: false },
        }
    }

    /// The unit confirmation answered yes: a user unit goes to the worker, a system one waits for
    /// the loop to run it in the TUI's window.
    pub(crate) fn run_unit(&mut self, scope: Scope, unit: String, action: UnitAction) {
        match scope {
            Scope::User => self.send_op(Op::UnitUser(action, unit)),
            Scope::System if !valid_unit(&unit) => {
                // A name from a list is checked again before it can become an argument.
                self.status.error = Some("not a unit name".into());
            }
            Scope::System => self.system_unit = Some((action, unit)),
        }
    }

    /// The system-unit action to run in the TUI's own window (empties the slot).
    pub fn take_system_unit(&mut self) -> Option<(UnitAction, String)> {
        self.system_unit.take()
    }

    /// The command line of a system-unit action. It never goes through the executor: systemd asks
    /// for authorisation on the terminal it is given.
    pub fn system_unit_argv(job: &(UnitAction, String)) -> Vec<String> {
        let verb = match job.0 {
            UnitAction::Restart => "restart",
            UnitAction::ResetFailed => "reset-failed",
        };
        ["systemctl", verb, "--", job.1.as_str()].map(String::from).to_vec()
    }

    /// What the window says before it runs: the exact command, and how to stop waiting.
    pub fn system_unit_intro(job: &(UnitAction, String)) -> String {
        format!("Running {} (Ctrl+C stops waiting)", Self::system_unit_argv(job).join(" "))
    }

    /// The window's command ended.
    pub fn system_unit_done(&mut self, job: &(UnitAction, String), r: Result<(), String>, now: Instant) {
        let unit = clean_line(&job.1, 80);
        let (text, error) = match r {
            Ok(()) => (
                match job.0 {
                    UnitAction::Restart => format!("Restarted: {unit}"),
                    UnitAction::ResetFailed => format!("Cleared: {unit}"),
                },
                false,
            ),
            Err(e) => (format!("{unit}: {}", clean_line(&e, 120)), true),
        };
        if error {
            self.status.error = Some(text.clone());
        } else {
            self.status.error = None;
            self.status.last = Some(text.clone());
        }
        self.toasts.push(Toast { text, error, until: now + TOAST_TTL });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        self.after_unit_action(now);
    }

    /// Whatever the outcome, the bar's count and the list are read again.
    pub(crate) fn after_unit_action(&mut self, now: Instant) {
        self.requests.push(Request::Do(Op::FailedRefresh));
        self.refresh_tab(now);
    }

    /// The Diag tab's own keys; true when it took the action.
    pub(crate) fn diagnostics_action(&mut self, a: super::action::Action, now: Instant) -> bool {
        use super::action::Action;
        if self.tab != super::Tab::Diagnostics {
            return false;
        }
        match a {
            Action::Back if self.diag.view != View::Overview => self.close_diag_view(),
            Action::UnitRestart => self.unit_action(UnitAction::Restart),
            Action::UnitReset => self.unit_action(UnitAction::ResetFailed),
            Action::Refresh => self.diagnostics_refresh(now),
            _ => return false,
        }
        true
    }

    pub(crate) fn domain_diag_failing(&self, d: Domain) -> bool {
        d == Domain::Failed && self.diag.failed_failing()
    }
}

#[cfg(test)]
mod tests;
