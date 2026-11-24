//! Small files the daemon keeps: the mode for the TUI (`$XDG_RUNTIME_DIR/alpheus/mode`) and
//! do-not-disturb (`$XDG_STATE_HOME/alpheus/dnd`). One hardened writer: an owner-only directory
//! (never a symlink, never open to others), a fresh temporary file never followed, renamed over the
//! old one, so a reader sees the old file or the new one, never half of one.
//!
//! Never:
//! - follow a link or write where someone else could: the directory is made owner-only and checked,
//!   and the temporary file is fresh and never followed;
//! - leave half a file behind: the rename is atomic, so a reader sees the old one or the new one;
//! - be bypassed: the notepad reaches the disk only through here
//!   (`notes_files_are_written_only_through_state_file`), and this file never panics
//!   (`the_notification_server_has_no_panicking_path` has it on its list);
//! - `fsync`: the rename is the guarantee, and a daemon that writes a note on every pause in the
//!   typing must not wait for a disk.
//!
//! See: docs/features/notes.md (what a failed save does), docs/features/notifications.md (the
//! do-not-disturb file and its three forms), docs/features/themes.md (the mode file the control
//! center reads), docs/decisions.md ("The do-not-disturb file holds one of three forms").

use std::fs::{DirBuilder, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Do-not-disturb's file under the state directory (notifications spec § 5): any file means on to
/// other readers (a session script reading the state); its text says whether it ends
/// (`until <unix>`) or came from a screen share (`sharing`). See docs/decisions.md, "The
/// do-not-disturb file holds one of three forms".
pub const DND_FILE: &str = "dnd";

/// The focus timer's file: one line of the timer spec § 6, removed while the timer is idle.
pub const TIMER_FILE: &str = "timer";

fn euid() -> u32 {
    // SAFETY: geteuid(2) cannot fail.
    unsafe { libc::geteuid() }
}

/// `name` is one plain file name: never empty, `.`, `..`, nor holding a `/` or a NUL.
fn plain(name: &str) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name:?} is not a plain file name")))
    } else {
        Ok(())
    }
}

/// Whether `dir` may hold our files: a real directory (never a symlink), ours, closed to group and
/// others. Missing is fine (it is created 0700).
pub fn dir_is_ours(dir: &Path) -> io::Result<()> {
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        Err(io::Error::other(format!("{} is not a real directory", dir.display())))
    } else if meta.uid() != euid() {
        Err(io::Error::other(format!("{} is not ours", dir.display())))
    } else if meta.mode() & 0o077 != 0 {
        Err(io::Error::other(format!("{} is open to others", dir.display())))
    } else {
        Ok(())
    }
}

/// `text` to `dir/name`. The directory is created owner-only (0700) when missing and refused when
/// it is a symlink, someone else's or open to others; the text goes to a fresh temporary file
/// `.<name>.<pid>` in it (a stale one removed first, never followed), then is renamed over the old
/// file. Err: the caller logs one line.
pub fn write_private(dir: &Path, name: &str, text: &[u8]) -> io::Result<()> {
    use std::io::Write;
    plain(name)?;
    dir_is_ours(dir)?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    dir_is_ours(dir)?;
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = OpenOptions::new().write(true).create_new(true).custom_flags(libc::O_NOFOLLOW).mode(0o600).open(&tmp)?;
    let written = file.write_all(text);
    drop(file);
    written.and_then(|()| std::fs::rename(&tmp, dir.join(name))).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `dir/name` removed; already gone (or no directory) is Ok. The directory must be ours, as for
/// writing.
pub fn remove_private(dir: &Path, name: &str) -> io::Result<()> {
    plain(name)?;
    dir_is_ours(dir)?;
    match std::fs::remove_file(dir.join(name)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// What `read_private` found.
#[derive(Debug, PartialEq, Eq)]
pub enum PrivateRead {
    Absent,
    Bytes(Vec<u8>),
    /// More than the cap: its first `cap` bytes.
    TooLarge(Vec<u8>),
    NotOurs,
    Failed,
}

/// `dir/name` read as bytes, at most `cap`: a regular file of ours (never a symlink, a directory or
/// someone else's), in a directory that passes `dir_is_ours`, opened without following links and
/// without blocking. More than `cap` bytes is `TooLarge` with the first `cap`, a refused file or directory `NotOurs`, an
/// I/O error `Failed`. Nothing is logged here.
pub fn read_private(dir: &Path, name: &str, cap: u64) -> PrivateRead {
    use std::io::Read;
    if plain(name).is_err() || dir_is_ours(dir).is_err() {
        return PrivateRead::NotOurs;
    }
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return PrivateRead::Absent,
        Err(_) => return PrivateRead::Failed,
        Ok(m) if !m.file_type().is_file() || m.uid() != euid() => return PrivateRead::NotOurs,
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    let read = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .and_then(|f| f.take(cap.saturating_add(1)).read_to_end(&mut bytes));
    match read {
        Err(_) => PrivateRead::Failed,
        Ok(_) if bytes.len() as u64 > cap => {
            bytes.truncate(cap as usize);
            PrivateRead::TooLarge(bytes)
        }
        Ok(_) => PrivateRead::Bytes(bytes),
    }
}

/// Do-not-disturb as saved: its text (at most 64 bytes, lossy UTF-8) for a regular file of ours at
/// `state_dir/dnd` (followed by nothing), in a directory that passes `dir_is_ours`, as for writing.
/// Absent, a symlink, a directory, someone else's: None (off); a directory not ours is logged (read
/// once, at startup). A file that cannot be read after those checks is `Some("")`: it exists, so on.
pub fn read_dnd(state_dir: &Path) -> Option<String> {
    use std::io::Read;
    if let Err(e) = dir_is_ours(state_dir) {
        crate::log::write(&format!("notifications: do-not-disturb read as off: {word}", word = crate::log::error_word(&e)));
        return None;
    }
    let path = state_dir.join(DND_FILE);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file() && m.uid() == euid()) {
        return None;
    }
    let mut text = Vec::new();
    let read = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .and_then(|f| f.take(DND_READ_CAP).read_to_end(&mut text));
    Some(if read.is_ok() { String::from_utf8_lossy(&text).into_owned() } else { String::new() })
}

/// Bytes of the file read: the longest form is `until <20 digits>\n`.
const DND_READ_CAP: u64 = 64;

/// Whether do-not-disturb is saved on (any file is "on").
#[cfg(test)]
pub fn dnd_on(state_dir: &Path) -> bool {
    read_dnd(state_dir).is_some()
}

/// Saves do-not-disturb: the text written (empty means on, untimed), None removes the file.
pub fn save_dnd(state_dir: &Path, text: Option<&str>) -> io::Result<()> {
    match text {
        Some(t) => write_private(state_dir, DND_FILE, t.as_bytes()),
        None => remove_private(state_dir, DND_FILE),
    }
}

/// Bytes of the timer file read: its longest line is under 100.
const TIMER_READ_CAP: u64 = 256;

/// The focus timer as saved: its text for a regular file of ours at `state_dir/timer` (followed by
/// nothing), in a directory that passes `dir_is_ours`. Absent, a symlink, a directory, someone
/// else's or a directory not ours: None. At most `TIMER_READ_CAP` bytes are read: a longer file,
/// or one that cannot be read, is `Some("")`, which the model refuses (the file is then dropped).
pub fn read_timer(state_dir: &Path) -> Option<String> {
    use std::io::Read;
    if dir_is_ours(state_dir).is_err() {
        crate::log::write("timer: state directory refused, read as idle");
        return None;
    }
    let path = state_dir.join(TIMER_FILE);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file() && m.uid() == euid()) {
        return None;
    }
    let mut text = Vec::new();
    let read = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .and_then(|f| f.take(TIMER_READ_CAP + 1).read_to_end(&mut text));
    Some(if read.is_ok() && text.len() as u64 <= TIMER_READ_CAP { String::from_utf8_lossy(&text).into_owned() } else { String::new() })
}

/// Saves the focus timer: the text written, None removes the file.
pub fn save_timer(state_dir: &Path, text: Option<&str>) -> io::Result<()> {
    match text {
        Some(t) => write_private(state_dir, TIMER_FILE, t.as_bytes()),
        None => remove_private(state_dir, TIMER_FILE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// A fresh scratch directory for one test (removed first, in case a run was interrupted).
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-state-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    // ---- the writer, ported from appearance.rs's mode-file tests ----

    #[test]
    fn a_file_is_written_whole_in_an_owner_only_directory() {
        let root = scratch("write");
        let dir = root.join("alpheus");
        write_private(&dir, "x", b"one\n").unwrap();
        assert_eq!(mode(&dir), 0o700, "created owner-only");
        assert_eq!(mode(&dir.join("x")), 0o600);
        assert_eq!(std::fs::read(dir.join("x")).unwrap(), b"one\n");
        write_private(&dir, "x", b"two\n").unwrap();
        assert_eq!(std::fs::read(dir.join("x")).unwrap(), b"two\n", "replaced");
        assert_eq!(names(&dir), ["x"], "no temporary file left behind");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_directory_open_to_others_or_a_symlink_is_refused() {
        let root = scratch("unsafe");
        let dir = root.join("alpheus");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(write_private(&dir, "x", b"").is_err(), "0755 refused");
        assert!(remove_private(&dir, "x").is_err(), "removal too");
        assert!(!dir.join("x").exists());
        std::fs::remove_dir_all(&dir).unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        assert!(write_private(&dir, "x", b"").is_err(), "a symlinked directory refused");
        assert!(!elsewhere.join("x").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_temporary_symlink_is_never_followed() {
        let root = scratch("stale");
        let dir = root.join("alpheus");
        write_private(&dir, "x", b"old").unwrap();
        let victim = root.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join(format!(".x.{}", std::process::id()))).unwrap();
        write_private(&dir, "x", b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(std::fs::read(dir.join("x")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_write_is_an_error_never_a_panic() {
        let root = scratch("blocked");
        std::fs::write(root.join("alpheus"), "a file where the directory should be").unwrap();
        assert!(write_private(&root.join("alpheus"), "x", b"").is_err());
        let dir = root.join("ok");
        std::fs::create_dir_all(dir.join("x")).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(write_private(&dir, "x", b"").is_err(), "a directory in the file's place is never replaced");
        assert!(dir.join("x").is_dir());
        assert_eq!(names(&dir), ["x"], "the temporary file removed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_plain_file_name_is_accepted() {
        let root = scratch("names");
        let dir = root.join("alpheus");
        for bad in ["", ".", "..", "a/b", "../x", "a\0b"] {
            let e = write_private(&dir, bad, b"").unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
            assert_eq!(remove_private(&dir, bad).unwrap_err().kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert!(!dir.exists(), "nothing created");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_a_missing_file_or_directory_is_fine() {
        let root = scratch("remove");
        assert!(remove_private(&root.join("nowhere"), "x").is_ok(), "no directory");
        let dir = root.join("alpheus");
        write_private(&dir, "x", b"").unwrap();
        remove_private(&dir, "x").unwrap();
        assert!(!dir.join("x").exists());
        assert!(remove_private(&dir, "x").is_ok(), "already gone");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- do-not-disturb (notifications spec § 5) ----

    #[test]
    fn do_not_disturb_is_an_empty_file_saved_and_removed() {
        let root = scratch("dnd");
        let state = root.join("alpheus");
        assert!(!dnd_on(&state), "no directory: off");
        save_dnd(&state, Some("")).unwrap();
        assert!(dnd_on(&state));
        assert_eq!(std::fs::read(state.join(DND_FILE)).unwrap(), b"", "an empty file");
        assert_eq!((mode(&state), mode(&state.join(DND_FILE))), (0o700, 0o600));
        save_dnd(&state, Some("")).unwrap();
        assert!(dnd_on(&state), "on twice: still on");
        save_dnd(&state, None).unwrap();
        assert!(!dnd_on(&state) && !state.join(DND_FILE).exists(), "off: no file");
        save_dnd(&state, None).unwrap();
        assert_eq!(names(&state), Vec::<String>::new(), "off twice: nothing left");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_dnd_returns_the_text_capped() {
        let root = scratch("dnd-read");
        let state = root.join("alpheus");
        assert_eq!(read_dnd(&state), None, "no directory");
        write_private(&state, DND_FILE, b"until 123\n").unwrap();
        assert_eq!(read_dnd(&state).as_deref(), Some("until 123\n"));
        write_private(&state, DND_FILE, &[b'x'; 1000]).unwrap();
        assert_eq!(read_dnd(&state), Some("x".repeat(64)), "the first 64 bytes");
        write_private(&state, DND_FILE, b"").unwrap();
        assert_eq!(read_dnd(&state).as_deref(), Some(""), "an empty file is on");
        write_private(&state, DND_FILE, &[0xff, 0xfe]).unwrap();
        assert!(read_dnd(&state).is_some(), "lossy UTF-8, never an error");
        std::fs::remove_file(state.join(DND_FILE)).unwrap();
        let victim = root.join("victim");
        std::fs::write(&victim, "until 5").unwrap();
        std::os::unix::fs::symlink(&victim, state.join(DND_FILE)).unwrap();
        assert_eq!(read_dnd(&state), None, "a symlink");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_dnd_writes_or_removes() {
        let root = scratch("dnd-save");
        let state = root.join("alpheus");
        save_dnd(&state, Some("until 5\n")).unwrap();
        assert_eq!(std::fs::read_to_string(state.join(DND_FILE)).unwrap(), "until 5\n");
        assert_eq!(mode(&state.join(DND_FILE)), 0o600);
        save_dnd(&state, None).unwrap();
        assert!(!state.join(DND_FILE).exists());
        save_dnd(&state, None).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `dnd` that is a symlink or a directory is refused and reads as off.
    #[test]
    fn a_dnd_symlink_or_directory_is_refused_and_reads_as_off() {
        let root = scratch("dnd-hostile");
        let state = root.join("alpheus");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let victim = root.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, state.join(DND_FILE)).unwrap();
        assert!(!dnd_on(&state), "a symlink, even to a regular file: off");
        save_dnd(&state, Some("")).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep", "the link replaced, never followed");
        assert!(!std::fs::symlink_metadata(state.join(DND_FILE)).unwrap().file_type().is_symlink());
        assert!(dnd_on(&state));
        std::fs::remove_file(state.join(DND_FILE)).unwrap();
        std::fs::create_dir(state.join(DND_FILE)).unwrap();
        assert!(!dnd_on(&state), "a directory: off");
        assert!(save_dnd(&state, Some("")).is_err(), "never replaces a directory");
        assert!(save_dnd(&state, None).is_err(), "nor removes one");
        assert!(state.join(DND_FILE).is_dir() && !dnd_on(&state));
        assert_eq!(names(&state), [DND_FILE], "no temporary file left");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Reading checks the directory as writing does: a `dnd` in a directory open to others (or a
    /// symlinked one) reads as off, whatever the file.
    #[test]
    fn a_dnd_in_a_directory_not_ours_reads_as_off() {
        let root = scratch("dnd-dir");
        let state = root.join("alpheus");
        save_dnd(&state, Some("")).unwrap();
        assert!(dnd_on(&state));
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!dnd_on(&state), "open to others: off");
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&state, &link).unwrap();
        assert!(!dnd_on(&link), "a symlinked directory: off");
        assert!(dnd_on(&state));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_timer_file_round_trips_and_goes_when_idle() {
        let root = scratch("timer");
        let state = root.join("alpheus");
        assert_eq!(read_timer(&state), None, "no directory");
        save_timer(&state, Some("countdown paused 1000 2000\n")).unwrap();
        assert_eq!(mode(&state), 0o700, "created owner-only");
        assert_eq!(read_timer(&state).as_deref(), Some("countdown paused 1000 2000\n"));
        assert_eq!(mode(&state.join(TIMER_FILE)), 0o600);
        save_timer(&state, None).unwrap();
        assert_eq!(read_timer(&state), None);
        assert!(!state.join(TIMER_FILE).exists(), "idle: no file");
        save_timer(&state, None).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_timer_file_is_read_capped_and_never_through_a_symlink() {
        let root = scratch("timer-cap");
        let state = root.join("alpheus");
        save_timer(&state, Some(&"x".repeat(256))).unwrap();
        assert_eq!(read_timer(&state), Some("x".repeat(256)), "the cap itself is read whole");
        save_timer(&state, Some(&"x".repeat(257))).unwrap();
        assert_eq!(read_timer(&state).as_deref(), Some(""), "one byte over: refused, not truncated");
        save_timer(&state, Some(&"countdown paused 1 1000\n".repeat(1 << 16))).unwrap();
        assert_eq!(read_timer(&state).as_deref(), Some(""), "a huge file: refused");
        save_timer(&state, None).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", state.join(TIMER_FILE)).unwrap();
        assert_eq!(read_timer(&state), None, "a symlink");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_private_reads_caps_and_refuses() {
        let root = scratch("read");
        let dir = root.join("alpheus");
        assert_eq!(read_private(&dir, "f", 8), PrivateRead::Absent, "no directory");
        write_private(&dir, "f", b"abc").unwrap();
        assert_eq!(read_private(&dir, "f", 8), PrivateRead::Bytes(b"abc".to_vec()));
        assert_eq!(read_private(&dir, "g", 8), PrivateRead::Absent);
        write_private(&dir, "f", b"123456789").unwrap();
        assert_eq!(read_private(&dir, "f", 8), PrivateRead::TooLarge(b"12345678".to_vec()), "cap + 1 bytes: the first cap kept");
        write_private(&dir, "f", b"12345678").unwrap();
        assert_eq!(read_private(&dir, "f", 8), PrivateRead::Bytes(b"12345678".to_vec()), "exactly the cap");
        std::os::unix::fs::symlink(dir.join("f"), dir.join("link")).unwrap();
        assert_eq!(read_private(&dir, "link", 8), PrivateRead::NotOurs, "a symlink");
        std::fs::create_dir(dir.join("d")).unwrap();
        assert_eq!(read_private(&dir, "d", 8), PrivateRead::NotOurs, "a directory named like the file");
        assert_eq!(read_private(&dir, "../f", 8), PrivateRead::NotOurs, "not a plain name");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(read_private(&dir, "f", 8), PrivateRead::NotOurs, "a directory open to others");
        let _ = std::fs::remove_dir_all(&root);
    }
}
