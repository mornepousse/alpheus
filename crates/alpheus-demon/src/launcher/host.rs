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
use std::time::Instant;

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

/// open: None (closed), Some(false) (apps), Some(true) (script). The Reply, when Some, is sent at once
/// on this connection: Toggle/Close → Done (Toggle closes an open script launcher: its own client gets
/// Cancelled through launcher_close); Dmenu while a script is open → (Nothing, Some(Busy));
/// Dmenu otherwise → (OpenScript, None).
pub fn decide(request: Request, open: Option<bool>) -> (Action, Option<Reply>) {
    match (request, open) {
        (Request::Toggle, None) => (Action::OpenApps, Some(Reply::Done)),
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

/// A finished scan is for the open launcher only if its generation is the open one's.
pub fn catalog_is_current(open_generation: Option<u64>, generation: u64) -> bool {
    open_generation == Some(generation)
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
    /// the bar down either).
    fn launcher_fail(&mut self) {
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
        let requested_at = Instant::now();
        let open = self.launcher.as_ref().map(|o| o.model.is_script());
        match decide(request, open) {
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
                // opening then costs.
                match reply {
                    Some(r) => conn.reply(&r),
                    None => drop(conn),
                }
                match action {
                    Action::OpenApps => self.launcher_open_apps(qh, requested_at),
                    Action::Close(reason) => self.launcher_close(reason),
                    Action::Nothing | Action::OpenScript { .. } => {}
                }
            }
        }
    }

    /// Bar button and Toggle: open (reads launcher.toml and the history now, spawns the scan with a new
    /// generation, decides the spawner) or close.
    pub fn launcher_toggle(&mut self, qh: &QueueHandle<App>) {
        let qh = qh.clone();
        self.launcher_guarded("toggle", move |app| {
            if app.launcher_is_open() {
                app.launcher_close(CloseReason::Toggle);
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
        // The scan's environment first: its $PATH is also the one activation
        // checks programs against (RunContext::path_dirs, via
        // `open.model.path_dirs` — the single copy, task 13), so the
        // launcher never says "found" for a list and "missing" for a
        // launch.
        let env = ScanEnv::from_env(|k| std::env::var_os(k));
        let model = Launcher::for_apps(config, self.launcher_toplevels(), history, now);
        self.launcher_open(qh, model, None, requested_at);
        let Some(open) = self.launcher.as_mut() else { return };
        open.model.path_dirs = env.path_dirs.clone();
        let generation = open.generation;
        let Some(tx) = self.mail.clone() else { return };
        if let Err(e) = scan::spawn_scan(env, move |catalog| {
            let _ = tx.send(Mail::Catalog { generation, catalog });
        }) {
            // A previous scan is still stuck (e.g. a hung mount): the box
            // keeps the windows and history it already has, a message
            // replaces the (empty) catalog, one log line — never closed,
            // never retried in a loop.
            log(&format!("launcher scan: {e}"));
            if let Some(open) = self.launcher.as_mut() {
                open.model.set_catalog(Catalog::default());
                open.model.show_message(scan_failure_message(&e));
            }
            self.launcher_redraw();
        }
    }

    pub fn launcher_catalog(&mut self, generation: u64, catalog: Catalog) {
        self.launcher_guarded("catalog", move |app| {
            if !catalog_is_current(app.launcher.as_ref().map(|o| o.generation), generation) {
                return; // closed, or reopened since: a stale scan
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
            assert_eq!(decide(request, open), (action, reply), "{label}");
        }
    }

    #[test]
    fn toggle_twice_quickly_leaves_it_closed() {
        let mut open = None;
        for _ in 0..2 {
            let (action, reply) = decide(Request::Toggle, open);
            assert_eq!(reply, Some(Reply::Done));
            open = open_after(open, &action);
        }
        assert_eq!(open, None, "Mod+Space twice: opened, then closed — never a second box");
        let (action, _) = decide(Request::Toggle, open);
        assert_eq!(open_after(open, &action), Some(false), "and the third opens again");
    }

    #[test]
    fn a_dmenu_replaces_an_app_launcher_and_a_second_dmenu_is_busy() {
        let (action, reply) = decide(dmenu(), Some(false));
        assert!(matches!(action, Action::OpenScript { .. }) && reply.is_none(), "the connection is kept for the answer");
        let open = open_after(Some(false), &action);
        assert_eq!(open, Some(true));
        let (action, reply) = decide(dmenu(), open);
        assert_eq!((action.clone(), reply), (Action::Nothing, Some(Reply::Busy)));
        assert_eq!(open_after(open, &action), Some(true), "the first script menu stays open");
    }

    #[test]
    fn a_stale_catalog_is_ignored() {
        assert!(catalog_is_current(Some(3), 3));
        assert!(!catalog_is_current(Some(3), 2), "a scan from a previous open (closed and reopened since) is stale");
        assert!(!catalog_is_current(None, 3), "closed: nobody to give it to");
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

    #[test]
    fn a_busy_scan_shows_a_message_instead_of_closing() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "the previous scan is still running");
        assert_eq!(scan_failure_message(&e), "could not scan applications: the previous scan is still running");
    }
}
