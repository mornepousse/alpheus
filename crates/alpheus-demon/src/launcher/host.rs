//! The launcher as one feature: socket requests decided purely (`decide`),
//! the bar button and the toggle, the scan's catalog delivered by
//! generation, the compositor's windows pushed to an open launcher, a dmenu
//! client's hang-up — every entry point behind `guarded`: a launcher error
//! closes the launcher, never the bar (spec § 3).

use crate::compositor::Toplevel;
use crate::launcher::config::{LauncherConfig, config_path};
use crate::launcher::history::History;
use crate::launcher::model::{Launcher, Origin, Outcome};
use crate::launcher::scan::{self, Catalog, ScanEnv, SourcePlan};
use crate::launcher::socket::Conn;
use crate::launcher::sources::Entries;
use crate::launcher::surface::{CloseReason, history_path_for, reply_on_close};
use crate::log::write as log;
use crate::startup::Mail;
use crate::state::Feed;
use crate::wayland::App;
use alpheus_commun::launcher_ipc::{DmenuFlags, Reply, Request};
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode, PostAction};
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a scan `spawn_scan` refused (`WouldBlock`) must have been
/// running before a reopen shows the "could not scan" message instead of
/// waiting quietly for it: a normal scan finishes well
/// under this (bounded file counts and depth); one still going past
/// it is more likely stuck (a hung NFS/FUSE mount) than merely slow, and
/// the daemon's own `REQUEST_DEADLINE` already uses the same order of
/// magnitude for "too long" elsewhere on this socket.
const SCAN_STUCK_AFTER: Duration = Duration::from_secs(2);

/// How long after the launcher opens a Toggle is still ignored (Mae,
/// 2026-10-06): with a press bind, Hyprland fires the bind a second time on
/// release once the launcher has grabbed the keyboard, so a second Toggle
/// lands here a few ms to ~200 ms after the open and would otherwise close
/// the box right back. Ignoring it is what keeps the press bind usable — the
/// release bind nixos-config tried instead misses whenever Mae lets go of
/// Super before Space.
pub(crate) const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Whether a Toggle arriving `since_open` after the launcher opened must be
/// ignored rather than close it (see `TOGGLE_DEBOUNCE`). A real double click
/// on the bar button within the window is treated the same way — ignored,
/// not a bug.
pub(crate) fn toggle_is_debounced(since_open: Duration) -> bool {
    since_open < TOGGLE_DEBOUNCE
}

fn guarded_with<R>(what: &str, f: impl FnOnce() -> R, log: &mut dyn FnMut(&str)) -> Option<R> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            log(&format!("launcher error in {what}"));
            None
        }
    }
}

/// Runs `f`, catching a panic: logs "launcher error in <what>" — never the panic's message, which
/// can quote the query, a menu line or a password (the daemon's panic hook logs the place) — and
/// returns None.
pub fn guarded<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    guarded_with(what, f, &mut |m| log(m))
}

/// What the daemon does with a request, decided purely (tested without Wayland).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    OpenApps,
    /// `--calc`: the calculator, with no scan.
    OpenCalc,
    /// `--windows`: the window switcher, with no scan.
    OpenWindows,
    /// `--clipboard`: the history alone, replacing any other open launcher.
    OpenClipboard,
    /// `--windows` while the switcher is open: select the next window.
    StepWindows,
    Close(CloseReason),
    /// Replaces an open app launcher (closed with CloseReason::Replaced) or opens fresh;
    /// the connection is kept (`Conn::into_pending`) for the answer.
    OpenScript {
        prompt: Option<String>,
        lines: Vec<String>,
        flags: DmenuFlags,
    },
}

/// What is open, for `decide`: the app launcher (the calculator counts as one), a script menu, the
/// window switcher, or the clipboard history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    Apps,
    Script,
    Switcher,
    Clipboard,
}

/// The open model's kind.
pub fn opened(model: &Launcher) -> Opened {
    if model.is_script() {
        Opened::Script
    } else if model.is_switcher() {
        Opened::Switcher
    } else if model.mode == crate::launcher::matching::Mode::Clipboard {
        Opened::Clipboard
    } else {
        Opened::Apps
    }
}

/// open: None (closed), else what is open (`Opened`). `since_open` is the time elapsed
/// since the launcher opened (ignored when `open` is None). The Reply, when Some, is sent at once
/// on this connection: Toggle/Close/Calc → Done (Toggle and Calc close an open script launcher: its own client gets
/// Cancelled through launcher_close, unless debounced — see `TOGGLE_DEBOUNCE`); Dmenu while a script
/// is open → (Nothing, Some(Busy)); Dmenu otherwise → (OpenScript, None).
pub fn decide(request: Request, open: Option<Opened>, since_open: Duration) -> (Action, Option<Reply>) {
    match (request, open) {
        (Request::Toggle, None) => (Action::OpenApps, Some(Reply::Done)),
        // Small features, decision 1: a second `--calc` closes the calculator, as a Toggle does,
        // with the same debounce against a re-fired press bind.
        (Request::Calc, None) => (Action::OpenCalc, Some(Reply::Done)),
        (Request::Calc, Some(_)) if toggle_is_debounced(since_open) => (Action::Nothing, Some(Reply::Done)),
        (Request::Calc, Some(_)) => (Action::Close(CloseReason::Toggle), Some(Reply::Done)),
        // Launcher 2, decision 12 (amended at the final review): a repeated `--windows` (Alt+Tab
        // held, or pressed again) steps to the next window, except within the debounce of the
        // switcher's own open — Hyprland's press-bind refire, which would move the selection off
        // the previous window. Past it, no debounce: a repeat is the point.
        (Request::Windows, Some(Opened::Switcher)) if toggle_is_debounced(since_open) => (Action::Nothing, Some(Reply::Done)),
        (Request::Windows, Some(Opened::Switcher)) => (Action::StepWindows, Some(Reply::Done)),
        // From nothing, an app launcher or a script menu: the switcher replaces it, as a script does.
        (Request::Windows, _) => (Action::OpenWindows, Some(Reply::Done)),
        // Clipboard spec, decision 17: `--clipboard` toggles the history (with the same debounce
        // against a re-fired press bind) and replaces anything else open, as `--windows` does.
        (Request::Clipboard, None) => (Action::OpenClipboard, Some(Reply::Done)),
        (Request::Clipboard, Some(Opened::Clipboard)) if toggle_is_debounced(since_open) => (Action::Nothing, Some(Reply::Done)),
        (Request::Clipboard, Some(Opened::Clipboard)) => (Action::Close(CloseReason::Toggle), Some(Reply::Done)),
        (Request::Clipboard, Some(_)) => (Action::OpenClipboard, Some(Reply::Done)),
        // The socket still answers Done (review/brief: alpheus-launch must exit 0), but no Close
        // action: the Hyprland press-bind refire must not take the box right back down.
        (Request::Toggle, Some(_)) if toggle_is_debounced(since_open) => (Action::Nothing, Some(Reply::Done)),
        (Request::Toggle, Some(_)) => (Action::Close(CloseReason::Toggle), Some(Reply::Done)),
        (Request::Close, None) => (Action::Nothing, Some(Reply::Done)),
        (Request::Close, Some(_)) => (Action::Close(CloseReason::Close), Some(Reply::Done)),
        (Request::Dmenu { .. }, Some(Opened::Script)) => (Action::Nothing, Some(Reply::Busy)),
        (Request::Dmenu { prompt, lines, flags }, _) => (Action::OpenScript { prompt, lines, flags }, None),
        // Not the launcher's: `watch_connection` answers them
        // before they could reach here. Answered all the same, never a panic.
        (
            Request::Dnd(_)
            | Request::DndOn(_)
            | Request::ClearNotifications
            | Request::Osd(_)
            | Request::Quick(_)
            | Request::Timer(_)
            | Request::Notes
            | Request::ClipboardClear
            | Request::Power
            // The Tools menu's open comes with the glue (plan task 7): until then, not answered here.
            | Request::Tools
            | Request::Outputs
            | Request::OutputsTry(_)
            | Request::OutputsKeep
            | Request::OutputsRevert,
            _,
        ) => (Action::Nothing, Some(Reply::Error("not a launcher request".into()))),
    }
}

/// The launcher's state after `action`.
pub fn open_after(open: Option<Opened>, action: &Action) -> Option<Opened> {
    match action {
        Action::Nothing | Action::StepWindows => open,
        // The calculator is not a script: it stands for the apps' state (Dmenu replaces it).
        Action::OpenApps | Action::OpenCalc => Some(Opened::Apps),
        Action::Close(_) => None,
        Action::OpenScript { .. } => Some(Opened::Script),
        Action::OpenWindows => Some(Opened::Switcher),
        Action::OpenClipboard => Some(Opened::Clipboard),
    }
}

/// A finished scan is for the open launcher if its generation is the open
/// one's (the normal case), or — a quick reopen while the previous open's
/// scan is still running: `spawn_scan` allows only one scan
/// at a time, so a reopen whose own attempt was refused (`WouldBlock`,
/// `launcher_open_apps`) never starts a second one — if the open launcher
/// has not received a catalog of its own yet, whatever arrives next can
/// only be that same in-flight scan's answer, whichever generation it
/// carries.
pub fn catalog_is_current(open: Option<(u64, bool)>, generation: u64) -> bool {
    match open {
        Some((g, has_catalog)) => g == generation || !has_catalog,
        None => false,
    }
}

/// Whether a source delivery is for the open launcher: the catalog's rule (`catalog_is_current`),
/// on an app launcher only (a script menu has no use for entries).
///
/// Two narrow races remain, both harmless. An open with no entries yet adopts whatever delivery
/// comes first: if the previous open's run lands, then its own (a reopen after that run ended but
/// before it was delivered), the open takes both, the second replacing the first — the same
/// sources, read twice. And the scan thread's "the previous open's are still being read" log
/// assumes the refused thread is the later one: an earlier open's thread that lost the race to the
/// next open's (it delivered its apps, then stalled before claiming the run) logs it too, though
/// the run in flight is then the next open's — the one the open launcher adopts anyway. The log
/// can name the wrong open, never the entries shown.
pub fn entries_are_current(open: Option<(u64, bool)>, takes_entries: bool, generation: u64) -> bool {
    takes_entries && catalog_is_current(open, generation)
}

/// Whether a scan refusal (`spawn_scan`'s `Err`) is shown as an error in the
/// box: any refusal other than `WouldBlock`, right away — `WouldBlock` only
/// once the scan it is waiting on has been running at least `SCAN_STUCK_AFTER`
/// (`running_for`, `None` when no scan is known to be in flight at all).
/// A quick close-then-reopen while a normal scan is still finishing must
/// look like nothing happened, not like a failure; one that has run
/// suspiciously long deserves to say so.
pub(crate) fn scan_refusal_is_an_error(kind: io::ErrorKind, running_for: Option<Duration>) -> bool {
    kind != io::ErrorKind::WouldBlock || running_for.is_some_and(|d| d >= SCAN_STUCK_AFTER)
}

/// launcher.toml from `config_dir`: absent → defaults silently, invalid → defaults + one `log` call;
/// each bound a valid file went past (`LauncherConfig::notes`) is logged too, at each open.
pub(crate) fn read_config(config_dir: &Path, log: &mut dyn FnMut(&str)) -> LauncherConfig {
    let (config, warning) = LauncherConfig::load(&config_path(config_dir));
    if let Some(w) = warning {
        log(&w);
    }
    for note in &config.notes {
        log(note);
    }
    config
}

/// Whether a scan's deliveries (catalog, entries) are for this model: by how the open began, not
/// by its current mode, so a `/` jump into the Tools menu and back keeps them (tools plan,
/// decision 2); a `--tools` open, a script menu, the calculator or the switcher take none.
pub(crate) fn takes_scan_deliveries(model: &Launcher) -> bool {
    model.origin == Origin::Apps
}

/// Only an app launcher (and the switcher) is fed the compositor's windows: the other menus have
/// nothing to rank them against.
pub(crate) fn feeds_windows(open: Option<&Launcher>) -> bool {
    open.is_some_and(|m| m.origin == Origin::Apps || m.is_switcher())
}

/// Whether `Launcher::set_windows` is worth calling: it rebuilds the
/// ranker's per-window facts and refilters, so a feed that would deliver the
/// same list already fed (`current`) is skipped rather than paid for again.
pub(crate) fn windows_changed(current: &[Toplevel], new: &[Toplevel]) -> bool {
    current != new
}

/// The message shown in the box when `spawn_scan` is refused (a previous
/// scan still stuck, e.g. a hung mount): the box stays open with the
/// windows and history it already has, only commands/apps are missing —
/// one log line (the caller), never closed, never retried in a loop.
pub(crate) fn scan_failure_message(e: &io::Error) -> String {
    format!("could not scan applications: {e}")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl App {
    /// Runs a launcher entry point; a panic closes the launcher (Error reply to a waiting client), the bar goes on.
    pub(crate) fn launcher_guarded<R>(&mut self, what: &str, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let r = guarded(what, || f(self));
        if r.is_none() {
            self.launcher_fail();
        }
        r
    }

    /// After a panic: the model may be in any state, so it is never called
    /// again — only dropped, itself guarded (a second panic must not take
    /// the bar down either). Caveat `guarded` cannot cover: if
    /// `reply_on_close` panics here AND `OpenLauncher`'s `Drop` (run right
    /// after, dropping `open` at the end of this closure) ALSO panics while
    /// the first one is still unwinding, that is a panic during unwind —
    /// Rust aborts the process then, past any `catch_unwind`. Not expected
    /// (`Drop` only releases local resources: the client watch), so not
    /// specially guarded against.
    fn launcher_fail(&mut self) {
        // Take the launcher OUT first, then release the keyboard:
        // `keyboard_release` is a no-op while `keyboard_wanted()` is true, and that stays true as long
        // as `self.launcher` is `Some` — releasing before the take left the `wl_keyboard`, its keymap
        // and its repeat source resident after a panic closed the launcher. The release must also run
        // when a panic inside `launcher_close` already took the launcher (`None` here).
        // And after the box is dropped: while it lives, a key down on it (the one
        // that panicked) defers the release to a key-up or leave that its dead surface never sends.
        let open = self.launcher.take();
        let was_open = open.is_some();
        // Only one launcher exists: any open prompt is this one, and its model is not read after a panic.
        self.clipboard_password_closed_if_open();
        if let Some(open) = open {
            let _ = guarded("close", move || {
                let mut open = open;
                reply_on_close(open.reply.take(), CloseReason::Error);
            });
        }
        self.keyboard_release();
        if !was_open {
            return;
        }
        log("launcher closed (error)");
        crate::memory::release_memory();
    }

    fn launcher_toplevels(&self) -> Vec<Toplevel> {
        match &self.bar.compositor {
            Feed::Ready(c) => c.toplevels.clone(),
            _ => Vec::new(),
        }
    }

    /// From the socket: Toggle/Close answered Done; a second Dmenu while a script is open → Busy.
    pub fn launcher_request(&mut self, qh: &QueueHandle<App>, request: Request, conn: Conn) {
        let qh = qh.clone();
        self.launcher_guarded("request", move |app| app.request_now(&qh, request, conn));
    }

    fn request_now(&mut self, qh: &QueueHandle<App>, request: Request, conn: Conn) {
        // The connection's own accepted_at, not "now": the open latency the
        // daemon logs (spec § 3) covers request accepted → first frame, not
        // just the part after this request was fully read.
        let requested_at = conn.accepted_at();
        let open = self.launcher.as_ref().map(|o| opened(&o.model));
        let since_open = self.launcher.as_ref().map_or(Duration::ZERO, |o| o.requested_at.elapsed());
        // The history off or unbound: said before `decide`, so nothing else open is replaced.
        if request == Request::Clipboard {
            if let Err(why) = self.clipboard_availability() {
                conn.reply(&Reply::Error(why.into()));
                return;
            }
        }
        match decide(request, open, since_open) {
            (Action::OpenScript { prompt, lines, flags }, _) => {
                if self.launcher_is_open() {
                    self.launcher_close(CloseReason::Replaced);
                }
                let config = read_config(&paths::config_dir(), &mut |m| log(m));
                let model = Launcher::for_dmenu(config, prompt, lines, flags);
                self.launcher_open(qh, model, Some(conn.into_pending()), requested_at);
                self.launcher_watch_client();
            }
            (action, reply) => {
                // Answered first: alpheus-launch returns at once, whatever the
                // opening then costs. `decide` never pairs anything but
                // OpenScript (matched above) with None (this
                // used to match `reply` with an unreachable `None` arm).
                conn.reply(&reply.expect("decide: only OpenScript answers None, matched above"));
                match action {
                    Action::OpenApps => self.launcher_open_apps(qh, requested_at),
                    Action::OpenCalc => self.launcher_open_calc(qh, requested_at),
                    Action::OpenClipboard => {
                        if self.launcher_is_open() {
                            self.launcher_close(CloseReason::Replaced);
                        }
                        self.launcher_open_clipboard(qh, requested_at);
                    }
                    Action::OpenWindows => {
                        if self.launcher_is_open() {
                            self.launcher_close(CloseReason::Replaced);
                        }
                        self.launcher_open_windows(qh, requested_at);
                    }
                    Action::StepWindows => {
                        let stepped = self.launcher.as_mut().is_some_and(|o| o.model.step(1) == Outcome::Redraw);
                        if stepped {
                            self.launcher_redraw();
                        }
                    }
                    Action::Close(reason) => self.launcher_close(reason),
                    Action::Nothing | Action::OpenScript { .. } => {}
                }
            }
        }
    }

    /// Bar button and Toggle: open (reads launcher.toml and the history now, spawns the scan with a new
    /// generation, decides the spawner) or close — unless debounced (`TOGGLE_DEBOUNCE`): a real
    /// double click on the bar button right after opening is ignored the same way a Hyprland
    /// press-bind refire is.
    pub fn launcher_toggle(&mut self, qh: &QueueHandle<App>) {
        let qh = qh.clone();
        self.launcher_guarded("toggle", move |app| {
            if app.launcher_is_open() {
                let since_open = app.launcher.as_ref().map_or(Duration::ZERO, |o| o.requested_at.elapsed());
                if !toggle_is_debounced(since_open) {
                    app.launcher_close(CloseReason::Toggle);
                }
            } else {
                app.launcher_open_apps(&qh, Instant::now());
            }
        });
    }

    /// `--calc`: the calculator alone. No scan, no history; the `$PATH` is read for the copy
    /// command's lookup only.
    fn launcher_open_calc(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        let config = read_config(&paths::config_dir(), &mut |m| log(m));
        let mut model = Launcher::for_calc(config);
        model.path_dirs = ScanEnv::from_env(|k| std::env::var_os(k)).path_dirs;
        self.launcher_open(qh, model, None, requested_at);
    }

    /// `--clipboard`: the history alone. No scan, no `$PATH`; the rows are a snapshot of the history.
    pub(crate) fn launcher_open_clipboard(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        let config = read_config(&paths::config_dir(), &mut |m| log(m));
        let model = Launcher::for_clipboard(config, self.clipboard_rows());
        self.launcher_open(qh, model, None, requested_at);
    }

    /// `--windows`: the window switcher. No scan, no history, no `$PATH`.
    fn launcher_open_windows(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        let config = read_config(&paths::config_dir(), &mut |m| log(m));
        let model = Launcher::for_windows(config, self.launcher_toplevels());
        self.launcher_open(qh, model, None, requested_at);
    }

    fn launcher_open_apps(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        let config = read_config(&paths::config_dir(), &mut |m| log(m));
        let sources = config.sources.clone();
        let now = unix_now();
        let history = match history_path_for(&config, &paths::state_dir()) {
            Some(path) => {
                let (h, warning) = History::load(&path, config.history, now);
                if let Some(w) = warning {
                    log(&w);
                }
                h
            }
            None => History::empty(0),
        };
        // The scan's environment, read once: its $PATH is also the one
        // activation checks programs against (RunContext::path_dirs, via
        // `open.model.path_dirs` — the single copy), so the
        // launcher never says "found" for a list and "missing" for a
        // launch. Set on the model BEFORE `launcher_open`, which then keeps
        // it as is for the app launcher (this used to read
        // the environment a second time, inside `launcher_open`, only to
        // overwrite it with the same value right back here).
        let env = ScanEnv::from_env(|k| std::env::var_os(k));
        let mut model = Launcher::for_apps(config, self.launcher_toplevels(), history, now);
        model.path_dirs = env.path_dirs.clone();
        // The `"` prefix only with the history on and bound.
        if self.clipboard_availability().is_ok() {
            model.clipboard = true;
            model.set_clips(self.clipboard_rows());
        }
        self.launcher_open(qh, model, None, requested_at);
        let Some(open) = self.launcher.as_mut() else { return };
        let generation = open.generation;
        let Some(tx) = self.mail.clone() else { return };
        // Sources (sources spec § 2): read after the apps on the same thread; no plan, no cost.
        let plan = (!sources.is_empty()).then(|| SourcePlan { sources, exec: self.exec.clone() });
        let entries_tx = tx.clone();
        // How long the scan this reopen is about to wait on (if refused) has
        // already been running: computed BEFORE `spawn_scan`, which is the
        // one call that would start a brand new one.
        let running_for = self.scan_started.map(|t| t.elapsed());
        match scan::spawn_scan(
            env,
            plan,
            move |catalog| {
                let _ = tx.send(Mail::Catalog { generation, catalog });
            },
            move |entries| {
                let _ = entries_tx.send(Mail::Entries { generation, entries });
            },
        ) {
            Ok(()) => {
                self.scan_started = Some(Instant::now());
                log("launcher scan: started");
            }
            Err(e) if !scan_refusal_is_an_error(e.kind(), running_for) => {
                // A quick reopen while the previous open's scan is still
                // running (`spawn_scan` allows only one at a time): no
                // second scan started, no message shown — the box keeps the
                // windows and history it already has, and will adopt that
                // scan's catalog whenever it arrives (`catalog_is_current`),
                // whatever generation it carries. Nothing to redraw either:
                // nothing changed yet.
                log("launcher scan: a scan is already running, waiting for it");
            }
            Err(e) => {
                // Either a genuine refusal (not WouldBlock), or a scan that
                // has been stuck long enough to call it so (e.g. a hung
                // mount): the box keeps the windows and history it already
                // has, a message replaces the (empty) catalog, one log
                // line — never closed, never retried in a loop.
                log(&format!("launcher scan: {e}"));
                if let Some(open) = self.launcher.as_mut() {
                    open.model.set_catalog(Catalog::default());
                    open.model.show_message(scan_failure_message(&e));
                }
                self.launcher_redraw();
            }
        }
    }

    pub fn launcher_catalog(&mut self, generation: u64, catalog: Catalog) {
        // At most one scan runs at a time (`scan::spawn_scan`): whatever
        // arrives here, the one it was in flight for is now done.
        self.scan_started = None;
        self.launcher_guarded("catalog", move |app| {
            let open = app.launcher.as_ref().map(|o| (o.generation, o.model.catalog.is_some()));
            let takes = app.launcher.as_ref().is_some_and(|o| takes_scan_deliveries(&o.model));
            if !takes || !catalog_is_current(open, generation) {
                // Closed, or reopened and already fed its own: a stale scan
                // — give its pages back, nothing to hold onto it for.
                drop(catalog);
                crate::memory::release_memory();
                return;
            }
            if let Some(open) = app.launcher.as_mut() {
                open.model.set_catalog(catalog);
            }
            app.launcher_redraw();
        });
    }

    /// An open's source entries, its second delivery (sources spec § 6): adopted under
    /// `entries_are_current`, else dropped. One log line — how many, how long after the request —
    /// never a line.
    pub fn launcher_entries(&mut self, generation: u64, entries: Entries) {
        self.launcher_guarded("entries", move |app| {
            let open = app.launcher.as_ref().map(|o| (o.generation, o.model.entries.is_some()));
            let takes = app.launcher.as_ref().is_some_and(|o| takes_scan_deliveries(&o.model));
            if !entries_are_current(open, takes, generation) {
                drop(entries);
                crate::memory::release_memory();
                return;
            }
            let Some(open) = app.launcher.as_mut() else { return };
            log(&format!(
                "launcher sources: {} entries, {} ms after the request",
                entries.items.len(),
                open.requested_at.elapsed().as_millis()
            ));
            open.model.set_entries(entries);
            app.launcher_redraw();
        });
    }

    /// Pushes the compositor's current windows to an open launcher — called
    /// at most once per `dispatch()` pass (`startup::WindowsFeed`),
    /// never once per `Msg::Compositor`: `Launcher::set_windows` is not
    /// cheap, and a burst (e.g. niri) can carry several in one pass.
    pub fn launcher_windows_changed(&mut self) {
        if !feeds_windows(self.launcher.as_ref().map(|o| &o.model)) {
            return;
        }
        self.launcher_guarded("windows", |app| {
            let windows = app.launcher_toplevels();
            let Some(open) = app.launcher.as_mut() else { return };
            if !windows_changed(&open.model.windows, &windows) {
                return; // unchanged since the last feed
            }
            open.model.set_windows(windows);
            app.launcher_redraw();
        });
    }

    /// Called by the client watch `launcher_watch_client` inserts below, when a dmenu client's fd is
    /// readable or hung up.
    pub fn launcher_client_check(&mut self) {
        self.launcher_guarded("client", |app| {
            let gone = app.launcher.as_ref().and_then(|o| o.reply.as_ref()).is_some_and(|p| p.hung_up());
            if gone {
                log("launcher: the dmenu client hung up");
                if let Some(open) = app.launcher.as_mut() {
                    open.reply = None; // nobody to answer
                }
                app.launcher_close(CloseReason::Close);
            }
        });
    }

    /// Watches the open script menu's client: edge-triggered (a client that
    /// half-closed after sending stays readable forever: level-triggered
    /// would spin the loop). Removed when the launcher is dropped.
    fn launcher_watch_client(&mut self) {
        let Some(handle) = self.loop_handle.clone() else { return };
        let Some(open) = self.launcher.as_mut() else { return };
        let Some(pending) = open.reply.as_ref() else { return };
        let fd = match pending.fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(e) => {
                log(&format!("launcher: client watch: {e}"));
                return;
            }
        };
        let source = Generic::new(fd, Interest::READ, Mode::Edge);
        match handle.insert_source(source, |_, _, app: &mut App| {
            app.launcher_client_check();
            Ok(PostAction::Continue)
        }) {
            Ok(token) => open.set_client_watch(handle, token),
            Err(e) => log(&format!("launcher: client watch: {}", e.error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpheus_commun::launcher_ipc::{DndVerb, OsdVerb, QuickVerb, TimerVerb};

    fn dmenu() -> Request {
        Request::Dmenu { prompt: Some("Pick: ".into()), lines: vec!["a".into(), "b".into()], flags: DmenuFlags::default() }
    }

    /// A Duration safely past `TOGGLE_DEBOUNCE`, for rows that exercise the
    /// ordinary (non-debounced) behaviour.
    const PAST_DEBOUNCE: Duration = Duration::from_millis(300);

    #[test]
    fn every_request_in_every_state() {
        use CloseReason as R;
        use Opened::{Apps, Clipboard, Script, Switcher};
        let script =
            Action::OpenScript { prompt: Some("Pick: ".into()), lines: vec!["a".into(), "b".into()], flags: DmenuFlags::default() };
        let not_ours = || Action::Nothing;
        let refused = || Some(Reply::Error("not a launcher request".into()));
        let done = || Some(Reply::Done);
        let rows = [
            (Request::Toggle, None, Action::OpenApps, done()),
            (Request::Toggle, Some(Apps), Action::Close(R::Toggle), done()),
            (Request::Toggle, Some(Script), Action::Close(R::Toggle), done()),
            (Request::Toggle, Some(Switcher), Action::Close(R::Toggle), done()),
            (Request::Close, None, Action::Nothing, done()),
            (Request::Close, Some(Apps), Action::Close(R::Close), done()),
            (Request::Close, Some(Script), Action::Close(R::Close), done()),
            (Request::Close, Some(Switcher), Action::Close(R::Close), done()),
            (dmenu(), None, script.clone(), None),
            (dmenu(), Some(Apps), script.clone(), None),
            (dmenu(), Some(Script), Action::Nothing, Some(Reply::Busy)),
            (dmenu(), Some(Switcher), script.clone(), None),
            (Request::Dnd(DndVerb::Toggle), None, not_ours(), refused()),
            (Request::ClearNotifications, Some(Script), not_ours(), refused()),
            (Request::Osd(OsdVerb::Brightness), None, not_ours(), refused()),
            (Request::Notes, Some(Apps), not_ours(), refused()),
            (Request::Clipboard, None, Action::OpenClipboard, done()),
            (Request::Clipboard, Some(Clipboard), Action::Close(R::Toggle), done()),
            (Request::Clipboard, Some(Apps), Action::OpenClipboard, done()),
            (Request::Clipboard, Some(Script), Action::OpenClipboard, done()),
            (Request::Clipboard, Some(Switcher), Action::OpenClipboard, done()),
            (Request::ClipboardClear, Some(Apps), not_ours(), refused()),
            // The power menu is answered before the launcher (`Route::Power`); the Tools menu's
            // open is Task 7: until then `decide` does not know it.
            (Request::Power, Some(Apps), not_ours(), refused()),
            (Request::Tools, None, not_ours(), refused()),
            (Request::Toggle, Some(Clipboard), Action::Close(R::Toggle), done()),
            (Request::Close, Some(Clipboard), Action::Close(R::Close), done()),
            (Request::Calc, Some(Clipboard), Action::Close(R::Toggle), done()),
            (Request::Windows, Some(Clipboard), Action::OpenWindows, done()),
            (dmenu(), Some(Clipboard), script.clone(), None),
            (Request::Calc, None, Action::OpenCalc, done()),
            (Request::Calc, Some(Apps), Action::Close(R::Toggle), done()),
            (Request::Calc, Some(Script), Action::Close(R::Toggle), done()),
            (Request::Calc, Some(Switcher), Action::Close(R::Toggle), done()),
            (Request::Windows, None, Action::OpenWindows, done()),
            (Request::Windows, Some(Apps), Action::OpenWindows, done()),
            (Request::Windows, Some(Script), Action::OpenWindows, done()),
            (Request::Windows, Some(Switcher), Action::StepWindows, done()),
            (Request::Quick(QuickVerb::Sound), Some(Apps), not_ours(), refused()),
            (Request::Timer(TimerVerb::Popup), Some(Apps), not_ours(), refused()),
            (Request::Timer(TimerVerb::Start(60)), None, not_ours(), refused()),
            (Request::Outputs, None, not_ours(), refused()),
            (Request::OutputsTry(String::new()), Some(Script), not_ours(), refused()),
            (Request::OutputsKeep, None, not_ours(), refused()),
            (Request::OutputsRevert, Some(Switcher), not_ours(), refused()),
        ];
        for (request, open, action, reply) in rows {
            let label = format!("{request:?} while {open:?}");
            assert_eq!(decide(request, open, PAST_DEBOUNCE), (action, reply), "{label}");
        }
    }

    /// Decision 12, amended at the final review: a `--windows` within `TOGGLE_DEBOUNCE` of the
    /// switcher's own open is Hyprland's press-bind refire and is ignored (still answered Done),
    /// so the previous window stays selected; past it every repeat steps, however close to the
    /// last one (the time counts from the open, not from the last step).
    #[test]
    fn a_repeated_windows_steps_past_the_debounce_only() {
        let switcher = Some(Opened::Switcher);
        assert_eq!(decide(Request::Windows, switcher, Duration::from_millis(10)), (Action::Nothing, Some(Reply::Done)));
        assert_eq!(decide(Request::Windows, switcher, Duration::from_millis(249)), (Action::Nothing, Some(Reply::Done)));
        assert_eq!(decide(Request::Windows, switcher, TOGGLE_DEBOUNCE), (Action::StepWindows, Some(Reply::Done)));
        assert_eq!(decide(Request::Windows, switcher, Duration::from_millis(300)), (Action::StepWindows, Some(Reply::Done)));
        assert_eq!(
            decide(Request::Windows, switcher, Duration::from_millis(310)),
            (Action::StepWindows, Some(Reply::Done)),
            "10 ms after the last step: steps again"
        );
    }

    /// Clipboard spec, decision 17: `--clipboard` toggles the history, ignoring a press-bind refire
    /// inside the debounce, and replaces any other open launcher (the state becomes the history).
    #[test]
    fn clipboard_toggles_with_the_debounce_and_replaces_others() {
        let open = Some(Opened::Clipboard);
        assert_eq!(decide(Request::Clipboard, open, Duration::from_millis(10)), (Action::Nothing, Some(Reply::Done)));
        assert_eq!(decide(Request::Clipboard, open, Duration::from_millis(300)), (Action::Close(CloseReason::Toggle), Some(Reply::Done)));
        for other in [Opened::Apps, Opened::Script, Opened::Switcher] {
            for since in [Duration::from_millis(10), PAST_DEBOUNCE] {
                assert_eq!(decide(Request::Clipboard, Some(other), since), (Action::OpenClipboard, Some(Reply::Done)), "{other:?}");
            }
        }
        assert_eq!(open_after(Some(Opened::Apps), &Action::OpenClipboard), open);
        let model = Launcher::for_clipboard(LauncherConfig::default(), vec![]);
        assert_eq!(opened(&model), Opened::Clipboard);
        assert_eq!(opened(&apps_model()), Opened::Apps);
    }

    /// `--windows` replaces another launcher (as a script does); the state becomes the switcher.
    #[test]
    fn windows_replaces_another_launcher() {
        for open in [Opened::Apps, Opened::Script] {
            let (action, reply) = decide(Request::Windows, Some(open), Duration::from_millis(10));
            assert_eq!((action.clone(), reply), (Action::OpenWindows, Some(Reply::Done)), "{open:?}");
            assert_eq!(open_after(Some(open), &action), Some(Opened::Switcher));
        }
        assert_eq!(open_after(Some(Opened::Script), &Action::OpenWindows), Some(Opened::Switcher));
        assert_eq!(open_after(Some(Opened::Switcher), &Action::StepWindows), Some(Opened::Switcher), "a step keeps the state");
    }

    /// A Toggle closes a switcher as it closes an app launcher.
    #[test]
    fn toggle_closes_a_switcher_as_an_app_launcher() {
        let model = Launcher::for_windows(LauncherConfig::default(), vec![]);
        assert_eq!(opened(&model), Opened::Switcher);
        assert_eq!(decide(Request::Toggle, Some(opened(&model)), PAST_DEBOUNCE), (Action::Close(CloseReason::Toggle), Some(Reply::Done)));
        assert_eq!(opened(&Launcher::for_calc(LauncherConfig::default())), Opened::Apps, "the calculator counts as Apps");
    }

    /// Small features, decision 1: `--calc` opens the calculator; a second one closes it, unless it
    /// is the press-bind refire inside the debounce window (ignored, still answered Done). A plain
    /// Toggle closes an open calculator, which the app launcher's open state stands for.
    #[test]
    fn a_second_calc_closes_the_calculator_unless_debounced() {
        let (action, reply) = decide(Request::Calc, None, Duration::ZERO);
        assert_eq!((action.clone(), reply), (Action::OpenCalc, Some(Reply::Done)));
        assert_eq!(open_after(None, &action), Some(Opened::Apps), "no script: the apps' state");
        let open = open_after(None, &action);
        assert_eq!(decide(Request::Calc, open, Duration::from_millis(100)), (Action::Nothing, Some(Reply::Done)));
        assert_eq!(decide(Request::Calc, open, PAST_DEBOUNCE), (Action::Close(CloseReason::Toggle), Some(Reply::Done)));
        assert_eq!(decide(Request::Toggle, open, PAST_DEBOUNCE), (Action::Close(CloseReason::Toggle), Some(Reply::Done)));
        assert_eq!(decide(Request::Toggle, open, Duration::from_millis(100)), (Action::Nothing, Some(Reply::Done)));
    }

    /// A scan's catalog or a source's entries (the previous open's, still in flight) never go to a
    /// calculator or a script menu: neither has a list to feed.
    #[test]
    fn a_calculator_takes_no_scan_delivery() {
        assert!(takes_scan_deliveries(&apps_model()));
        assert!(!takes_scan_deliveries(&script_model()));
        assert!(!takes_scan_deliveries(&Launcher::for_calc(LauncherConfig::default())));
    }

    /// A Toggle arriving less than `TOGGLE_DEBOUNCE` after the open is the
    /// Hyprland press-bind refire (spec of this fix, Mae 2026-10-06): it must
    /// be ignored, not close the box right back — but the socket still
    /// replies Done, so `alpheus-launch` exits 0.
    #[test]
    fn a_toggle_within_the_debounce_window_is_ignored_but_still_acked() {
        let (action, reply) = decide(Request::Toggle, Some(Opened::Apps), Duration::from_millis(100));
        assert_eq!(action, Action::Nothing, "the Hyprland press-bind refire must not close the box");
        assert_eq!(reply, Some(Reply::Done), "alpheus-launch must still exit 0");
    }

    /// Past the debounce window, a Toggle is a real one again and closes.
    #[test]
    fn a_toggle_past_the_debounce_window_closes() {
        let (action, reply) = decide(Request::Toggle, Some(Opened::Apps), Duration::from_millis(300));
        assert_eq!(action, Action::Close(CloseReason::Toggle));
        assert_eq!(reply, Some(Reply::Done));
    }

    /// Was `toggle_twice_quickly_leaves_it_closed`: before this fix, two
    /// Toggles in a row (the exact shape of the Hyprland press-bind refire)
    /// opened then immediately closed the box again. That is the bug this
    /// fix removes — adapted here to assert the new, intended behaviour: a
    /// quick second Toggle is swallowed and the box stays open; only once
    /// `TOGGLE_DEBOUNCE` has elapsed does a Toggle close it.
    #[test]
    fn toggle_twice_quickly_is_debounced_and_leaves_it_open() {
        let (action, reply) = decide(Request::Toggle, None, Duration::ZERO);
        assert_eq!(reply, Some(Reply::Done));
        let open = open_after(None, &action);
        assert_eq!(open, Some(Opened::Apps));
        let (action, reply) = decide(Request::Toggle, open, Duration::from_millis(100));
        assert_eq!(reply, Some(Reply::Done));
        assert_eq!(open_after(open, &action), open, "the second, quick Toggle changes nothing");
        let (action, _) = decide(Request::Toggle, open, PAST_DEBOUNCE);
        assert_eq!(open_after(open, &action), None, "a Toggle once the window has passed closes it");
    }

    #[test]
    fn a_dmenu_replaces_an_app_launcher_and_a_second_dmenu_is_busy() {
        let (action, reply) = decide(dmenu(), Some(Opened::Apps), PAST_DEBOUNCE);
        assert!(matches!(action, Action::OpenScript { .. }) && reply.is_none(), "the connection is kept for the answer");
        let open = open_after(Some(Opened::Apps), &action);
        assert_eq!(open, Some(Opened::Script));
        let (action, reply) = decide(dmenu(), open, PAST_DEBOUNCE);
        assert_eq!((action.clone(), reply), (Action::Nothing, Some(Reply::Busy)));
        assert_eq!(open_after(open, &action), Some(Opened::Script), "the first script menu stays open");
    }

    #[test]
    fn a_dmenus_flags_reach_the_menu() {
        let flags = DmenuFlags { index: true, password: true, visible: Some(2) };
        let (action, _) = decide(Request::Dmenu { prompt: None, lines: vec![], flags }, None, PAST_DEBOUNCE);
        assert_eq!(action, Action::OpenScript { prompt: None, lines: vec![], flags });
    }

    #[test]
    fn a_stale_catalog_is_ignored_once_the_open_launcher_has_its_own() {
        assert!(catalog_is_current(Some((3, true)), 3), "its own, matching generation");
        assert!(!catalog_is_current(Some((3, true)), 2), "already fed its own: a scan from a previous open is stale");
        assert!(!catalog_is_current(None, 3), "closed: nobody to give it to");
    }

    /// Closed and reopened (generation 3) while the previous
    /// open's scan (generation 2) is still running — `spawn_scan` refused a
    /// second one (`WouldBlock`, `launcher_open_apps`), so generation 3 has
    /// no scan of its own in flight. Whatever catalog arrives next is
    /// adopted, whichever generation it carries: `spawn_scan` allows only
    /// one scan at a time, so it can only be that same in-flight one.
    #[test]
    fn a_quick_reopen_during_a_scan_adopts_the_in_flight_catalog() {
        assert!(catalog_is_current(Some((3, false)), 2), "no catalog of its own yet: adopts the in-flight scan's result");
        assert!(catalog_is_current(Some((3, false)), 99), "whatever generation it carries");
        assert!(catalog_is_current(Some((3, false)), 3), "its own generation too, obviously");
    }

    #[test]
    fn entries_follow_the_catalog_rule_on_an_app_launcher_only() {
        assert!(entries_are_current(Some((3, false)), true, 3), "its own open");
        assert!(entries_are_current(Some((3, false)), true, 2), "no entries yet: adopts the run in flight");
        assert!(!entries_are_current(Some((3, true)), true, 2), "already fed: a previous open's run is stale");
        assert!(!entries_are_current(None, false, 3), "closed");
        assert!(!entries_are_current(Some((3, false)), false, 3), "a script menu has no use for entries");
    }

    #[test]
    fn a_panic_inside_guarded_returns_none_and_is_logged() {
        let mut lines = Vec::new();
        // A panic message can quote the query or a menu line (a slice's boundary panic does): the
        // log names the place only.
        let r: Option<u8> =
            guarded_with("key", || panic!("byte index 2 is not a char boundary of `query qz`"), &mut |m| lines.push(m.to_string()));
        assert_eq!(r, None);
        assert_eq!(lines, ["launcher error in key"]);
        assert_eq!(guarded("value", || 5), Some(5), "no panic: the value goes through");
    }

    /// `guarded` relies on unwinding: `panic = "abort"` in the release
    /// profile would turn every launcher bug into a dead bar.
    #[test]
    fn the_release_profile_unwinds_so_guarded_can_catch() {
        let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml")).unwrap();
        let table: toml::Table = manifest.parse().unwrap();
        let panic = table.get("profile").and_then(|p| p.get("release")).and_then(|r| r.get("panic")).and_then(|v| v.as_str());
        assert_ne!(panic, Some("abort"));
    }

    #[test]
    fn launcher_toml_invalid_gives_defaults_and_one_log_line_per_open() {
        let dir = std::env::temp_dir().join(format!("alpheus-host-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = Vec::new();
        assert_eq!(read_config(&dir, &mut |m| lines.push(m.to_string())), LauncherConfig::default());
        assert!(lines.is_empty(), "absent: defaults, no message");
        std::fs::write(dir.join("launcher.toml"), "rows = \"many\"\n").unwrap();
        for _ in 0..2 {
            assert_eq!(read_config(&dir, &mut |m| lines.push(m.to_string())), LauncherConfig::default());
        }
        assert_eq!(lines.len(), 2, "one line per open, read again each time: {lines:?}");
        assert!(lines.iter().all(|l| l.contains("launcher.toml")), "{lines:?}");
        std::fs::write(dir.join("launcher.toml"), "rows = 5\n").unwrap();
        assert_eq!(read_config(&dir, &mut |_| {}).rows, 5, "changes apply at the next open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_config_logs_the_notes() {
        let dir = std::env::temp_dir().join(format!("alpheus-host-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text: String = (0..17).map(|i| format!("[[tools.category]]\nname = \"c{i}\"\n")).collect();
        std::fs::write(dir.join("launcher.toml"), text).unwrap();
        for _ in 0..2 {
            let mut lines = Vec::new();
            let config = read_config(&dir, &mut |m| lines.push(m.to_string()));
            assert_eq!(config.tools.categories.len(), 16);
            assert_eq!(lines, ["tools: 17 categories in launcher.toml, the first 16 kept"], "once per open, counts only");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn apps_model() -> Launcher {
        Launcher::for_apps(LauncherConfig::default(), Vec::new(), History::empty(200), 0)
    }

    fn script_model() -> Launcher {
        Launcher::for_script(LauncherConfig::default(), None, vec!["a".into()])
    }

    /// Tools plan, decision 2: deliveries follow how the open began, not its mode. A Mod+Space
    /// open that jumped into the Tools menu keeps the catalog, the source entries and the windows
    /// for the way back; a `--tools` open takes none.
    #[test]
    fn an_apps_open_keeps_its_deliveries_in_tools() {
        use crate::launcher::tools::catalog::{Catalog, Context};
        let config = LauncherConfig::default();
        let catalog = Catalog::build(&config.tools, &Context::default(), &mut |_| {});
        let mut m = Launcher::for_apps(config.clone(), Vec::new(), History::empty(200), 0);
        m.set_tools(Catalog::build(&config.tools, &Context::default(), &mut |_| {}));
        m.key(crate::keys::Key::Char('/'));
        assert_eq!(m.mode, crate::launcher::matching::Mode::Tools);
        assert!(takes_scan_deliveries(&m), "the jump keeps them");
        assert!(feeds_windows(Some(&m)));
        let only = Launcher::for_tools(config, catalog, History::empty(200), 0);
        assert!(!takes_scan_deliveries(&only), "a --tools open takes none");
        assert!(!feeds_windows(Some(&only)));
    }

    #[test]
    fn a_switcher_takes_no_scan_delivery_but_is_fed_windows() {
        let m = Launcher::for_windows(LauncherConfig::default(), vec![]);
        assert!(!takes_scan_deliveries(&m));
        assert!(feeds_windows(Some(&m)));
    }

    #[test]
    fn only_an_open_app_launcher_is_fed_windows() {
        assert!(!feeds_windows(None), "closed: nobody to feed");
        assert!(feeds_windows(Some(&apps_model())));
        assert!(!feeds_windows(Some(&script_model())), "a script menu has nothing to rank windows against");
    }

    fn toplevel(selector: &str) -> Toplevel {
        Toplevel { selector: selector.into(), class: "c".into(), title: "t".into(), workspace: "1".into(), focused: false }
    }

    #[test]
    fn set_windows_is_skipped_when_nothing_changed() {
        let a = vec![toplevel("id:1")];
        assert!(!windows_changed(&a, &a.clone()), "the same list fed again: no re-rank");
        let b = vec![toplevel("id:2")];
        assert!(windows_changed(&a, &b), "a real change: must re-rank");
        assert!(windows_changed(&[], &a), "windows appearing for the first time: a real change");
    }

    /// This used to only check `scan_failure_message`'s
    /// format string, which says nothing about WHEN a scan refusal is shown
    /// at all. Renamed to be honest about that, kept for the format string
    /// itself; the actual behaviour (box stays open, no retry, shown only
    /// when warranted) is `scan_refusal_is_an_error`, tested below.
    #[test]
    fn scan_failure_message_formats_the_io_error() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "the previous scan is still running");
        assert_eq!(scan_failure_message(&e), "could not scan applications: the previous scan is still running");
    }

    #[test]
    fn a_quick_reopen_during_a_scan_shows_no_error() {
        let busy = io::ErrorKind::WouldBlock;
        assert!(!scan_refusal_is_an_error(busy, None), "no scan known to be running yet: wait quietly");
        assert!(
            !scan_refusal_is_an_error(busy, Some(SCAN_STUCK_AFTER - Duration::from_millis(1))),
            "still well within a normal scan's time: not an error"
        );
    }

    #[test]
    fn a_scan_stuck_long_enough_is_shown_as_an_error() {
        let busy = io::ErrorKind::WouldBlock;
        assert!(scan_refusal_is_an_error(busy, Some(SCAN_STUCK_AFTER)), "run for SCAN_STUCK_AFTER: likely a hung mount");
        assert!(scan_refusal_is_an_error(busy, Some(SCAN_STUCK_AFTER + Duration::from_secs(60))));
    }

    #[test]
    fn any_other_scan_refusal_is_always_an_error() {
        assert!(scan_refusal_is_an_error(io::ErrorKind::Other, None), "not WouldBlock: shown right away, whatever `running_for`");
        assert!(scan_refusal_is_an_error(io::ErrorKind::PermissionDenied, Some(Duration::ZERO)));
    }
}
