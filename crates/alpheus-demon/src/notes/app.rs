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
use crate::notes::model::{Notes, NotesOutcome};
use crate::notes::store::{self, Loaded};
use crate::wayland::App;
use alpheus_commun::exec::{ExecError, Executor};
use alpheus_commun::launcher_ipc::{Reply, Request};
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::surface::OpenNotes;

/// The pad's state on the App: the pad while open (None at rest: nothing of it resident), its save
/// timer, and the text a close could not write (I2), kept for the next open; lost if the daemon
/// stops before then.
pub struct NotesApp {
    pub(crate) open: Option<OpenNotes>,
    pub(crate) timer: DeadlineTimer,
    pub(crate) unsaved: Option<Kept>,
}

/// What a failed close keeps (I2, I-1): the text, and the file as it was when the pad last loaded
/// or saved it, so a later open can tell the file moved on.
pub(crate) struct Kept {
    pub(crate) text: String,
    pub(crate) seen: Fingerprint,
}

/// The notes file as the pad last saw it: device, inode, mtime (s, ns) and size, or absent. A
/// symlink is not followed (the file is refused then, and never written).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fingerprint(Option<(u64, u64, i64, i64, u64)>);

impl Fingerprint {
    pub(crate) fn of(state_dir: &std::path::Path) -> Fingerprint {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(state_dir.join(store::FILE)) {
            Ok(m) => Fingerprint(Some((m.dev(), m.ino(), m.mtime(), m.mtime_nsec(), m.size()))),
            // Absent, or unreadable: a file that later appears, or is readable, differs.
            Err(_) => Fingerprint(None),
        }
    }
}

/// The help row's words when the kept text is not opened: fixed, never the text.
pub(crate) const KEPT_DROPPED: &str = "earlier unsaved text not written: the file changed";

impl NotesApp {
    pub fn new() -> NotesApp {
        NotesApp { open: None, timer: DeadlineTimer::default(), unsaved: None }
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

/// Whether the pad has something to write: edited, and not read only (a refused file is never
/// overwritten).
pub(crate) fn wants_save(model: &Notes) -> bool {
    model.dirty && model.read_only.is_none()
}

/// The flags after a save: written → `saved`; failed → `not saved`, still edited, so the next
/// autosave or the close (shutdown included) tries again.
pub(crate) fn after_save(model: &mut Notes, result: &std::io::Result<()>) {
    match result {
        Ok(()) => {
            model.dirty = false;
            model.save_failed = false;
        }
        Err(_) => model.save_failed = true,
    }
}

/// The pad an open shows: the text a failed close kept (`not saved`, still edited, so the next
/// close or edit writes it) when the file is still as the pad last saw it (`now`); if the file
/// changed meanwhile, the file as `load` reads it (a refused one read only) and the kept text is
/// dropped unwritten, with a help-row message: the file on disk wins (I-1). Else the file.
pub(crate) fn opening_model(kept: Option<&Kept>, now: Fingerprint, load: impl FnOnce() -> Loaded) -> Notes {
    let stale = kept.is_some_and(|k| k.seen != now);
    if let Some(k) = kept.filter(|k| k.seen == now) {
        let mut model = Notes::new(k.text.clone(), None);
        model.dirty = true;
        model.save_failed = true;
        return model;
    }
    let mut model = match load() {
        Loaded::Text(t) => Notes::new(t, None),
        Loaded::Refused(r, shown) => {
            log(&format!("notes: read only ({})", r.word()));
            Notes::new(shown, Some(r))
        }
    };
    if stale {
        log("notes: kept edit dropped (the file changed)");
        model.show_message(KEPT_DROPPED.to_string());
    }
    model
}

/// What a close keeps after its save: the text when it is still to be written (the save failed).
pub(crate) fn kept_after_close(model: &Notes, seen: Fingerprint) -> Option<Kept> {
    wants_save(model).then(|| Kept { text: model.pad.text().to_string(), seen })
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
    /// `alpheus-launch --notes` (`launcher/socket_glue.rs`, `watch_connection`): toggles the pad.
    /// Done; a request that is not the pad's is answered Done too (`route` never sends one).
    pub(crate) fn notes_verb(&mut self, qh: &QueueHandle<App>, request: &Request) -> Reply {
        if matches!(request, Request::Notes) {
            let requested_at = Instant::now();
            self.notes_guarded("verb", |app| app.notes_toggle(qh, requested_at));
        }
        Reply::Done
    }

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
            let kept = guarded_with(
                "close",
                move || {
                    let mut kept = None;
                    if wants_save(&open.model) {
                        if let Err(e) = store::save(&paths::state_dir(), open.model.pad.text()) {
                            log(&format!("notes: not saved ({:?})", e.kind()));
                            kept = kept_after_close(&open.model, open.seen);
                        }
                    }
                    drop(open);
                    kept
                },
                &mut |m| log(m),
            );
            self.notes.unsaved = kept.flatten();
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
        // Before anything is dropped: every close saves; what it could not write waits for the next
        // open (I2).
        self.notes_save();
        let open = self.notes.open.take();
        self.notes.unsaved = open.as_ref().and_then(|o| kept_after_close(&o.model, o.seen));
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
        if !wants_save(&open.model) {
            return;
        }
        // Stamped again by the next edit, either way: a failure must not re-arm at once.
        open.dirty_since = None;
        let result = store::save(&paths::state_dir(), open.model.pad.text());
        if let Err(e) = &result {
            log(&format!("notes: not saved ({:?})", e.kind()));
        }
        after_save(&mut open.model, &result);
        if result.is_ok() {
            open.seen = Fingerprint::of(&paths::state_dir());
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
    fn a_failed_save_stays_edited_so_the_close_saves_again() {
        let mut n = Notes::new(String::new(), None);
        assert!(!wants_save(&n), "nothing typed: nothing to write");
        n.key(NoteKey::Char('a'));
        assert!(wants_save(&n));
        after_save(&mut n, &Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
        assert!(n.save_failed && n.dirty, "not saved, still edited");
        assert!(wants_save(&n), "the close (or shutdown) writes it again");
        assert_eq!(n.status(), "not saved");
        after_save(&mut n, &Ok(()));
        assert!(!n.dirty && !n.save_failed && !wants_save(&n));
        assert_eq!(n.status(), "saved");
        let ro = Notes::new(String::new(), Some(crate::notes::model::Refusal::NotOurs));
        assert!(!wants_save(&ro), "a refused file is never written");
        // The model refuses edits itself; this is the second layer, so force `dirty`.
        let mut ro = ro;
        ro.dirty = true;
        assert!(!wants_save(&ro), "an edited read-only pad is still never written");
    }

    fn failed(text: &str) -> Notes {
        let mut n = Notes::new(String::new(), None);
        for c in text.chars() {
            n.key(NoteKey::Char(c));
        }
        after_save(&mut n, &Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)));
        n
    }

    /// I2: a close whose save failed keeps the text; the next open shows it (`not saved`, still
    /// edited so the next close or edit writes it) while the file is as the pad last saw it, without
    /// reading the file; a close that saved keeps nothing.
    #[test]
    fn a_failed_close_save_keeps_the_text_for_the_next_open() {
        use crate::notes::model::Refusal;
        let seen = Fingerprint(Some((1, 2, 3, 4, 5)));
        let mut n = failed("a");
        let kept = kept_after_close(&n, seen).expect("kept");
        assert_eq!(kept.text, "a");
        let reopened = opening_model(Some(&kept), seen, || panic!("the file is not read while text is kept"));
        assert_eq!(reopened.pad.text(), "a");
        assert_eq!(reopened.status(), "not saved");
        assert!(wants_save(&reopened), "the next close writes it again");
        assert_eq!(reopened.message(), None, "an unchanged file: nothing to say");

        after_save(&mut n, &Ok(()));
        assert!(kept_after_close(&n, seen).is_none(), "saved: nothing kept");
        assert!(kept_after_close(&Notes::new("x".into(), None), seen).is_none(), "never edited: nothing kept");

        let from_file = opening_model(None, seen, || Loaded::Text("file".into()));
        assert_eq!((from_file.pad.text(), from_file.status()), ("file", "saved"));
        let refused = opening_model(None, seen, || Loaded::Refused(Refusal::TooLarge, "start".into()));
        assert_eq!((refused.pad.text(), refused.read_only), ("start", Some(Refusal::TooLarge)));
    }

    /// I-1: the file changed since the pad last saw it (written elsewhere, replaced, removed, now
    /// refused): the file wins, the kept text is not opened (so never written over it) and the help
    /// row says so in fixed words.
    #[test]
    fn a_file_that_changed_wins_over_the_kept_text() {
        use crate::notes::model::Refusal;
        let seen = Fingerprint(Some((1, 2, 3, 4, 5)));
        let kept = kept_after_close(&failed("kept qz"), seen).unwrap();
        for now in
            [Fingerprint(Some((1, 2, 3, 4, 6))), Fingerprint(Some((1, 9, 3, 4, 5))), Fingerprint(Some((1, 2, 3, 5, 5))), Fingerprint(None)]
        {
            let m = opening_model(Some(&kept), now, || Loaded::Text("newer".into()));
            assert_eq!((m.pad.text(), m.status()), ("newer", "saved"), "{now:?}");
            assert!(!wants_save(&m), "the kept text is never written");
            assert_eq!(m.message(), Some(KEPT_DROPPED));
            assert!(!KEPT_DROPPED.contains("qz"));
        }
        let refused = opening_model(Some(&kept), Fingerprint(Some((9, 9, 9, 9, 9))), || Loaded::Refused(Refusal::NotClean, "shown".into()));
        assert_eq!((refused.pad.text(), refused.read_only), ("shown", Some(Refusal::NotClean)));
        assert!(!wants_save(&refused), "a refused newer file is never rewritten");
        assert_eq!(refused.message(), Some(KEPT_DROPPED));
        let large = opening_model(Some(&kept), Fingerprint(None), || Loaded::Refused(Refusal::TooLarge, "start".into()));
        assert!(!wants_save(&large) && large.read_only == Some(Refusal::TooLarge));
    }

    #[test]
    fn the_fingerprint_follows_the_file() {
        let d = TempDir::new("notes-fingerprint");
        let absent = Fingerprint::of(&d);
        assert_eq!(absent, Fingerprint(None));
        std::fs::write(d.join(store::FILE), "one").unwrap();
        let one = Fingerprint::of(&d);
        assert_ne!(one, absent);
        assert_eq!(Fingerprint::of(&d), one, "stable while untouched");
        std::fs::write(d.join(store::FILE), "three").unwrap();
        assert_ne!(Fingerprint::of(&d), one, "size moved");
        std::fs::remove_file(d.join(store::FILE)).unwrap();
        assert_eq!(Fingerprint::of(&d), absent);
    }

    /// M-2: the kept text is released only once the pad is built, just before it is stored, so an
    /// open that returns early or panics keeps it.
    #[test]
    fn the_kept_text_is_released_just_before_the_pad_is_stored() {
        let src = include_str!("surface.rs");
        let open = &src[src.find("fn notes_open").unwrap()..];
        let open = &open[..open.find("\n    /// The seat's data device").unwrap()];
        let release = open.find("self.notes.unsaved = None;").expect("released");
        let store = open.find("self.notes.open = Some(open);").expect("stored");
        assert!(open[release..store].lines().count() <= 2, "released right before the store");
        assert!(open.rfind("return;").unwrap() < release, "no early return after the release");
        assert!(!open.contains("unsaved.take()"), "never taken early");
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
