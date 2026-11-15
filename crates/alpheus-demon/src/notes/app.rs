//! The pad as one feature of the App: the toggle decided purely (`decide`), its guard (a pad error
//! saves what was typed and closes the pad, never the bar), the outcomes of its keys, the autosave
//! timer (armed only while the pad is open and edited) and the copy command. Logs carry fixed words
//! only, never the note (spec § 5).

use crate::deadline::DeadlineTimer;
use crate::launcher::host::TOGGLE_DEBOUNCE;
use crate::launcher::run;
use crate::launcher::scan::has_program;
use crate::launcher::surface::CloseReason;
use crate::log::write as log;
use crate::notes::keys::NoteKey;
use crate::notes::model::NotesOutcome;
use crate::notes::store;
use crate::wayland::App;
use alpheus_commun::exec::{ExecError, Executor};
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::surface::OpenNotes;

/// The pad's state on the App: the pad while open (None at rest: nothing of it resident) and its
/// save timer.
pub struct NotesApp {
    pub(crate) open: Option<OpenNotes>,
    pub(crate) timer: DeadlineTimer,
}

impl NotesApp {
    pub fn new() -> NotesApp {
        NotesApp { open: None, timer: DeadlineTimer::default() }
    }
}

impl Default for NotesApp {
    fn default() -> NotesApp {
        NotesApp::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesAction {
    Open,
    Close,
    Nothing,
}

/// What the verb does: closed → open; open within `TOGGLE_DEBOUNCE` of its own open → nothing
/// (Hyprland re-fires a press bind); open past it → close.
pub fn decide(open: bool, since_open: Duration) -> NotesAction {
    if !open {
        NotesAction::Open
    } else if since_open < TOGGLE_DEBOUNCE {
        NotesAction::Nothing
    } else {
        NotesAction::Close
    }
}

/// The copy program's failure, in the launcher's words: the program, never the text.
pub fn copy_failure(prog: &str, e: &ExecError) -> String {
    run::failure(prog, e.clone())
}

/// Hands `text` to the `copy` command (launcher.toml) on its standard input, never its argv; a
/// program missing from `path_dirs` is said without running anything.
pub(crate) fn copy_call(copy: &[String], path_dirs: &[PathBuf], exec: &dyn Executor, text: &str) -> Result<(), String> {
    let (prog, args) = copy.split_first().ok_or_else(|| "copy: no program configured".to_string())?;
    if !has_program(path_dirs, prog) {
        return Err(copy_failure(prog, &ExecError::NotFound));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec.spawn_with_input(prog, &args, &[text.as_bytes()]).map_err(|e| copy_failure(prog, &e))
}

/// A held key repeats moves and edits, never a copy, a cut, a paste or the close (P8): each repeat
/// would start another copy program or reopen the clipboard.
pub(crate) fn repeat_ignored(key: NoteKey) -> bool {
    matches!(key, NoteKey::Copy | NoteKey::Cut | NoteKey::Paste | NoteKey::Escape)
}

/// Runs `f`, catching a panic: logs "notes error in <what>" — never the panic's message, which can
/// embed the note's text (P9) — and returns None.
fn guarded_with<R>(what: &str, f: impl FnOnce() -> R, log: &mut dyn FnMut(&str)) -> Option<R> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            log(&format!("notes error in {what}"));
            None
        }
    }
}

impl App {
    /// The verb: open, close, or nothing within the debounce.
    pub(crate) fn notes_toggle(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        let since = self.notes.open.as_ref().map(|o| o.requested_at.elapsed());
        match decide(since.is_some(), since.unwrap_or_default()) {
            NotesAction::Open => self.notes_open(qh, requested_at),
            NotesAction::Close => self.notes_close(CloseReason::Toggle),
            NotesAction::Nothing => log("notes: toggle debounced"),
        }
    }

    /// Runs a pad entry point; a panic saves what was typed and closes the pad (decision 17), the
    /// bar goes on.
    pub(crate) fn notes_guarded<R>(&mut self, what: &str, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let r = guarded_with(what, || f(self), &mut |m| log(m));
        if r.is_none() {
            self.notes_fail();
        }
        r
    }

    /// After a panic: the text saved if edited (itself guarded), the pad dropped (guarded too), the
    /// timer disarmed and the keyboard released once the pad is out (it is wanted while `Some`).
    fn notes_fail(&mut self) {
        let open = self.notes.open.take();
        let was_open = open.is_some();
        if let Some(open) = open {
            let _ = guarded_with(
                "close",
                move || {
                    if open.model.dirty && open.model.read_only.is_none() {
                        if let Err(e) = store::save(&paths::state_dir(), open.model.pad.text()) {
                            log(&format!("notes: not saved ({:?})", e.kind()));
                        }
                    }
                    drop(open);
                },
                &mut |m| log(m),
            );
        }
        self.notes_timer_sync();
        self.keyboard_release();
        if !was_open {
            return;
        }
        log("notes closed (error)");
        crate::memory::release_memory();
    }

    /// Saves (if edited), drops everything, disarms the timer, gives the keyboard back.
    pub(crate) fn notes_close(&mut self, reason: CloseReason) {
        if self.notes.open.is_none() {
            return;
        }
        // Before anything is dropped: every close saves.
        self.notes_save();
        let open = self.notes.open.take();
        drop(open);
        self.notes_timer_sync();
        self.keyboard_release();
        log(&format!("notes closed ({})", reason.text()));
        // The pad's text, its pool and buffers: give the freed pages back.
        crate::memory::release_memory();
    }

    pub fn notes_is_open(&self) -> bool {
        self.notes.open.is_some()
    }

    /// Applies what a key (or the copy's result) asked.
    pub(crate) fn notes_outcome(&mut self, o: NotesOutcome) {
        match o {
            NotesOutcome::Nothing => {}
            NotesOutcome::Redraw => self.notes_redraw(),
            NotesOutcome::Close => self.notes_close(CloseReason::Escape),
            NotesOutcome::Copy { text, cut } => {
                let App { notes, exec, .. } = self;
                let Some(open) = notes.open.as_mut() else { return };
                let result = copy_call(&open.copy, &open.path_dirs, exec.as_ref(), &text);
                let out = open.model.after_copy(cut, result);
                open.stamp_dirty();
                self.notes_timer_sync();
                self.notes_outcome(out);
            }
            NotesOutcome::Paste => self.notes_paste(),
        }
    }

    /// Writes the pad if it is edited and may be written; a failure is one log line and `not
    /// saved`, tried again after the next edit or at the close.
    pub(crate) fn notes_save(&mut self) {
        let Some(open) = self.notes.open.as_mut() else { return };
        if !open.model.dirty || open.model.read_only.is_some() {
            return;
        }
        // Stamped again by the next edit, either way: a failure must not re-arm at once.
        open.dirty_since = None;
        match store::save(&paths::state_dir(), open.model.pad.text()) {
            Ok(()) => {
                open.model.dirty = false;
                open.model.save_failed = false;
            }
            Err(e) => {
                open.model.save_failed = true;
                log(&format!("notes: not saved ({:?})", e.kind()));
            }
        }
    }

    /// The save timer: armed `SAVE_AFTER` after the first unsaved edit, only while the pad is open.
    pub(crate) fn notes_timer_sync(&mut self) {
        let deadline = store::save_deadline(self.notes.open.is_some(), self.notes.open.as_ref().and_then(|o| o.dirty_since));
        self.notes.timer.sync_opt(self.loop_handle.as_ref(), deadline, App::notes_save_fire);
    }

    fn notes_save_fire(app: &mut App, _now: Instant) {
        app.notes.timer.fired();
        app.notes_guarded("save", |app| {
            app.notes_save();
            app.notes_timer_sync();
            // The title row's word: `saved` or `not saved`.
            app.notes_redraw();
        });
    }

    /// Ctrl+V. Delivery 1, task 4: nothing to read yet (the data device comes with task 6).
    pub(crate) fn notes_paste(&mut self) {
        let Some(open) = self.notes.open.as_mut() else { return };
        open.model.show_message("nothing to paste".to_string());
        self.notes_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::testing::TempDir;
    use alpheus_commun::exec::fake::FakeExecutor;

    #[test]
    fn decide_opens_closes_and_debounces() {
        assert_eq!(decide(false, Duration::ZERO), NotesAction::Open);
        assert_eq!(decide(false, Duration::from_secs(60)), NotesAction::Open, "closed: open, whatever the time");
        assert_eq!(decide(true, Duration::from_millis(10)), NotesAction::Nothing, "Hyprland's second press");
        assert_eq!(decide(true, Duration::from_millis(250)), NotesAction::Close);
    }

    #[test]
    fn copy_failure_names_the_program_only() {
        assert_eq!(copy_failure("wl-copy", &ExecError::NotFound), "wl-copy: program not found");
        assert_eq!(copy_failure("wl-copy", &ExecError::Timeout), "wl-copy: timed out");
        assert_eq!(copy_failure("xclip", &ExecError::Io("broken pipe".into())), "xclip: broken pipe");
    }

    fn stub(name: &str, programs: &[&str]) -> TempDir {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new(&format!("notes-{name}"));
        for p in programs {
            std::fs::write(d.join(p), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(d.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        d
    }

    #[test]
    fn copy_puts_the_selection_on_stdin_only() {
        let bin = stub("copy-stdin", &["wl-copy"]);
        let dirs = [bin.to_path_buf()];
        let fake = FakeExecutor::new();
        fake.ok("wl-copy", "");
        let copy = vec!["wl-copy".to_string()];
        assert_eq!(copy_call(&copy, &dirs, &fake, "token 1234"), Ok(()));
        assert_eq!(fake.calls(), ["wl-copy"], "one call, no argument");
        assert_eq!(fake.inputs(), [("wl-copy".to_string(), b"token 1234".to_vec())]);

        let fake = FakeExecutor::new();
        assert_eq!(copy_call(&[], &dirs, &fake, "token 1234"), Err("copy: no program configured".to_string()));
        let empty = stub("copy-none", &[]);
        let err = copy_call(&copy, &[empty.to_path_buf()], &fake, "token 1234").unwrap_err();
        assert_eq!(err, "wl-copy: program not found");
        assert!(!err.contains("token"), "never the text: {err}");
        assert!(fake.calls().is_empty(), "nothing run for a missing program or no command");
    }

    #[test]
    fn repeats_never_copy_paste_or_close() {
        for k in [NoteKey::Copy, NoteKey::Cut, NoteKey::Paste, NoteKey::Escape] {
            assert!(repeat_ignored(k), "{k:?}");
        }
        for k in [NoteKey::Char('a'), NoteKey::Backspace, NoteKey::Enter, NoteKey::SelectAll] {
            assert!(!repeat_ignored(k), "{k:?} repeats");
        }
    }

    #[test]
    fn a_panic_logs_the_place_never_the_payload() {
        let mut lines = Vec::new();
        let r: Option<()> =
            guarded_with("key", || panic!("byte index 3 is not a char boundary of `secret note`"), &mut |m| lines.push(m.to_string()));
        assert_eq!(r, None);
        assert_eq!(lines, ["notes error in key"]);
        assert_eq!(guarded_with("key", || 7, &mut |_| {}), Some(7));
    }
}
