//! The launcher as one feature: socket requests decided purely (`decide`),
//! the bar button and the toggle, the scan's catalog delivered by
//! generation, the compositor's windows pushed to an open launcher, a dmenu
//! client's hang-up — every entry point behind `guarded`: a launcher error
//! closes the launcher, never the bar (spec § 3).

use crate::compositor::Toplevel;
use crate::launch::Mail;
use crate::launcher::config::{LauncherConfig, config_path};
use crate::launcher::history::History;
use crate::launcher::model::Launcher;
use crate::launcher::scan::{self, Catalog, ScanEnv};
use crate::launcher::socket::Conn;
use crate::launcher::surface::{CloseReason, history_path_for, reply_on_close};
use crate::log::write as log;
use crate::sources::panic_text;
use crate::state::Feed;
use crate::wayland::App;
use alpheus_commun::launcher_ipc::{Reply, Request};
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode, PostAction};
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a scan `spawn_scan` refused (`WouldBlock`) must have been
/// running before a reopen shows the "could not scan" message instead of
/// waiting quietly for it (review item 1): a normal scan finishes well
/// under this (bounded file counts and depth, task 5); one still going past
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
const TOGGLE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Whether a Toggle arriving `since_open` after the launcher opened must be
/// ignored rather than close it (see `TOGGLE_DEBOUNCE`). A real double click
/// on the bar button within the window is treated the same way — ignored,
/// not a bug.
fn toggle_is_debounced(since_open: Duration) -> bool {
    since_open < TOGGLE_DEBOUNCE
}

fn guarded_with<R>(what: &str, f: impl FnOnce() -> R, log: &mut dyn FnMut(&str)) -> Option<R> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(p) => {
            log(&format!("launcher error in {what}: {}", panic_text(p.as_ref())));
            None
        }
    }
}

/// Runs `f`, catching a panic: logs "launcher error in <what>: <message>" and returns None.
pub fn guarded<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    guarded_with(what, f, &mut |m| log(m))
}

/// What the daemon does with a request, decided purely (tested without Wayland).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    OpenApps,
    Close(CloseReason),
    /// Replaces an open app launcher (closed with CloseReason::Replaced) or opens fresh;
    /// the connection is kept (`Conn::into_pending`) for the answer.
    OpenScript {
        prompt: Option<String>,
        lines: Vec<String>,
    },
}

/// open: None (closed), Some(false) (apps), Some(true) (script). `since_open` is the time elapsed
/// since the launcher opened (ignored when `open` is None). The Reply, when Some, is sent at once
/// on this connection: Toggle/Close → Done (Toggle closes an open script launcher: its own client gets
/// Cancelled through launcher_close, unless debounced — see `TOGGLE_DEBOUNCE`); Dmenu while a script
/// is open → (Nothing, Some(Busy)); Dmenu otherwise → (OpenScript, None).
pub fn decide(request: Request, open: Option<bool>, since_open: Duration) -> (Action, Option<Reply>) {
    match (request, open) {
        (Request::Toggle, None) => (Action::OpenApps, Some(Reply::Done)),
        // The socket still answers Done (review/brief: alpheus-launch must exit 0), but no Close
        // action: the Hyprland press-bind refire must not take the box right back down.
        (Request::Toggle, Some(_)) if toggle_is_debounced(since_open) => (Action::Nothing, Some(Reply::Done)),
        (Request::Toggle, Some(_)) => (Action::Close(CloseReason::Toggle), Some(Reply::Done)),
        (Request::Close, None) => (Action::Nothing, Some(Reply::Done)),
        (Request::Close, Some(_)) => (Action::Close(CloseReason::Close), Some(Reply::Done)),
        (Request::Dmenu { .. }, Some(true)) => (Action::Nothing, Some(Reply::Busy)),
        (Request::Dmenu { prompt, lines }, _) => (Action::OpenScript { prompt, lines }, None),
    }
}

/// The launcher's state after `action` (None closed, Some(false) apps, Some(true) script).
pub fn open_after(open: Option<bool>, action: &Action) -> Option<bool> {
    match action {
        Action::Nothing => open,
        Action::OpenApps => Some(false),
        Action::Close(_) => None,
        Action::OpenScript { .. } => Some(true),
    }
}

/// A finished scan is for the open launcher if its generation is the open
/// one's (the normal case), or — a quick reopen while the previous open's
/// scan is still running (review item 1): `spawn_scan` allows only one scan
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

/// Whether a scan refusal (`spawn_scan`'s `Err`) is shown as an error in the
/// box: any refusal other than `WouldBlock`, right away — `WouldBlock` only
/// once the scan it is waiting on has been running at least `SCAN_STUCK_AFTER`
/// (`running_for`, `None` when no scan is known to be in flight at all).
/// A quick close-then-reopen while a normal scan is still finishing must
/// look like nothing happened, not like a failure; one that has run
/// suspiciously long deserves to say so (review item 1).
pub(crate) fn scan_refusal_is_an_error(kind: io::ErrorKind, running_for: Option<Duration>) -> bool {
    kind != io::ErrorKind::WouldBlock || running_for.is_some_and(|d| d >= SCAN_STUCK_AFTER)
}

/// launcher.toml from `config_dir`: absent → defaults silently, invalid → defaults + one `log` call.
pub(crate) fn read_config(config_dir: &Path, log: &mut dyn FnMut(&str)) -> LauncherConfig {
    let (config, warning) = LauncherConfig::load(&config_path(config_dir));
    if let Some(w) = warning {
        log(&w);
    }
    config
}

/// Only an app launcher is fed the compositor's windows: a script menu's
/// model has nothing to rank them against (`Launcher::is_script`).
pub(crate) fn feeds_windows(open: Option<&Launcher>) -> bool {
    open.is_some_and(|m| !m.is_script())
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
        // Unconditional, before the early-out below (task 15 fix round, IMPORTANT): idempotent,
        // and `keyboard_wanted` itself checks what still needs the keyboard — but if a panic
        // happens inside `launcher_close` after it already took the launcher out of `self`,
        // `self.launcher` is already `None` by the time THIS `launcher_fail` runs, and the early
        // return below used to skip the release entirely, leaking the keyboard the launcher had bound.
        self.keyboard_release();
        let Some(open) = self.launcher.take() else { return };
        let _ = guarded("close", move || {
            let mut open = open;
            reply_on_close(open.reply.take(), CloseReason::Error);
        });
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
        // just the part after this request was fully read (review item 4).
        let requested_at = conn.accepted_at();
        let open = self.launcher.as_ref().map(|o| o.model.is_script());
        let since_open = self.launcher.as_ref().map_or(Duration::ZERO, |o| o.requested_at.elapsed());
        match decide(request, open, since_open) {
            (Action::OpenScript { prompt, lines }, _) => {
                if self.launcher_is_open() {
                    self.launcher_close(CloseReason::Replaced);
                }
                let config = read_config(&paths::config_dir(), &mut |m| log(m));
                let model = Launcher::for_script(config, prompt, lines);
                self.launcher_open(qh, model, Some(conn.into_pending()), requested_at);
                self.launcher_watch_client();
            }
            (action, reply) => {
                // Answered first: alpheus-launch returns at once, whatever the
                // opening then costs. `decide` never pairs anything but
                // OpenScript (matched above) with None (review item 6: this
                // used to match `reply` with an unreachable `None` arm).
                conn.reply(&reply.expect("decide: only OpenScript answers None, matched above"));
                match action {
                    Action::OpenApps => self.launcher_open_apps(qh, requested_at),
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

    fn launcher_open_apps(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        let config = read_config(&paths::config_dir(), &mut |m| log(m));
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
        // `open.model.path_dirs` — the single copy, task 13), so the
        // launcher never says "found" for a list and "missing" for a
        // launch. Set on the model BEFORE `launcher_open`, which then keeps
        // it as is for the app launcher (review item 6: this used to read
        // the environment a second time, inside `launcher_open`, only to
        // overwrite it with the same value right back here).
        let env = ScanEnv::from_env(|k| std::env::var_os(k));
        let mut model = Launcher::for_apps(config, self.launcher_toplevels(), history, now);
        model.path_dirs = env.path_dirs.clone();
        self.launcher_open(qh, model, None, requested_at);
        let Some(open) = self.launcher.as_mut() else { return };
        let generation = open.generation;
        let Some(tx) = self.mail.clone() else { return };
        // How long the scan this reopen is about to wait on (if refused) has
        // already been running: computed BEFORE `spawn_scan`, which is the
        // one call that would start a brand new one (review item 1).
        let running_for = self.scan_started.map(|t| t.elapsed());
        match scan::spawn_scan(env, move |catalog| {
            let _ = tx.send(Mail::Catalog { generation, catalog });
        }) {
            Ok(()) => self.scan_started = Some(Instant::now()),
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
            if !catalog_is_current(open, generation) {
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

    /// Pushes the compositor's current windows to an open launcher — called
    /// at most once per `dispatch()` pass (`launch::WindowsFeed`, launch.rs),
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

    /// Called from launch.rs's loop on a pending dmenu fd becoming readable/hung up.
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

    fn dmenu() -> Request {
        Request::Dmenu { prompt: Some("Pick: ".into()), lines: vec!["a".into(), "b".into()] }
    }

    /// A Duration safely past `TOGGLE_DEBOUNCE`, for rows that exercise the
    /// ordinary (non-debounced) behaviour.
    const PAST_DEBOUNCE: Duration = Duration::from_millis(300);

    #[test]
    fn every_request_in_every_state() {
        use CloseReason as R;
        let script = Action::OpenScript { prompt: Some("Pick: ".into()), lines: vec!["a".into(), "b".into()] };
        let rows = [
            (Request::Toggle, None, Action::OpenApps, Some(Reply::Done)),
            (Request::Toggle, Some(false), Action::Close(R::Toggle), Some(Reply::Done)),
            (Request::Toggle, Some(true), Action::Close(R::Toggle), Some(Reply::Done)),
            (Request::Close, None, Action::Nothing, Some(Reply::Done)),
            (Request::Close, Some(false), Action::Close(R::Close), Some(Reply::Done)),
            (Request::Close, Some(true), Action::Close(R::Close), Some(Reply::Done)),
            (dmenu(), None, script.clone(), None),
            (dmenu(), Some(false), script.clone(), None),
            (dmenu(), Some(true), Action::Nothing, Some(Reply::Busy)),
        ];
        for (request, open, action, reply) in rows {
            let label = format!("{request:?} while {open:?}");
            assert_eq!(decide(request, open, PAST_DEBOUNCE), (action, reply), "{label}");
        }
    }

    /// A Toggle arriving less than `TOGGLE_DEBOUNCE` after the open is the
    /// Hyprland press-bind refire (spec of this fix, Mae 2026-10-06): it must
    /// be ignored, not close the box right back — but the socket still
    /// replies Done, so `alpheus-launch` exits 0.
    #[test]
    fn a_toggle_within_the_debounce_window_is_ignored_but_still_acked() {
        let (action, reply) = decide(Request::Toggle, Some(false), Duration::from_millis(100));
        assert_eq!(action, Action::Nothing, "the Hyprland press-bind refire must not close the box");
        assert_eq!(reply, Some(Reply::Done), "alpheus-launch must still exit 0");
    }

    /// Past the debounce window, a Toggle is a real one again and closes.
    #[test]
    fn a_toggle_past_the_debounce_window_closes() {
        let (action, reply) = decide(Request::Toggle, Some(false), Duration::from_millis(300));
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
        assert_eq!(open, Some(false));
        let (action, reply) = decide(Request::Toggle, open, Duration::from_millis(100));
        assert_eq!(reply, Some(Reply::Done));
        assert_eq!(open_after(open, &action), open, "the second, quick Toggle changes nothing");
        let (action, _) = decide(Request::Toggle, open, PAST_DEBOUNCE);
        assert_eq!(open_after(open, &action), None, "a Toggle once the window has passed closes it");
    }

    #[test]
    fn a_dmenu_replaces_an_app_launcher_and_a_second_dmenu_is_busy() {
        let (action, reply) = decide(dmenu(), Some(false), PAST_DEBOUNCE);
        assert!(matches!(action, Action::OpenScript { .. }) && reply.is_none(), "the connection is kept for the answer");
        let open = open_after(Some(false), &action);
        assert_eq!(open, Some(true));
        let (action, reply) = decide(dmenu(), open, PAST_DEBOUNCE);
        assert_eq!((action.clone(), reply), (Action::Nothing, Some(Reply::Busy)));
        assert_eq!(open_after(open, &action), Some(true), "the first script menu stays open");
    }

    #[test]
    fn a_stale_catalog_is_ignored_once_the_open_launcher_has_its_own() {
        assert!(catalog_is_current(Some((3, true)), 3), "its own, matching generation");
        assert!(!catalog_is_current(Some((3, true)), 2), "already fed its own: a scan from a previous open is stale");
        assert!(!catalog_is_current(None, 3), "closed: nobody to give it to");
    }

    /// Review item 1: closed and reopened (generation 3) while the previous
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
    fn a_panic_inside_guarded_returns_none_and_is_logged() {
        let mut lines = Vec::new();
        let r: Option<u8> = guarded_with("key", || panic!("boom"), &mut |m| lines.push(m.to_string()));
        assert_eq!(r, None);
        assert_eq!(lines, ["launcher error in key: boom"]);
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

    fn apps_model() -> Launcher {
        Launcher::for_apps(LauncherConfig::default(), Vec::new(), History::empty(200), 0)
    }

    fn script_model() -> Launcher {
        Launcher::for_script(LauncherConfig::default(), None, vec!["a".into()])
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

    /// Review item 8: this used to only check `scan_failure_message`'s
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
