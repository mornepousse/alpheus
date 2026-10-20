//! A camera opened outside PipeWire (privacy delivery 2, spec §§ 1-2): the pure parts. `scan` finds
//! who holds a `video*` node by reading `<proc_dir>/<pid>/fd` links (never by opening anything under
//! `dev_dir`), and `classify` turns one inotify record into what the glue should do with it. Events
//! are triggers, never counts: the state shown comes from a scan alone.

use crate::pipewire::drawable;
use crate::privacy::Indicator;
use crate::watch::Event;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

/// A trigger is followed by one scan this long after the last one (an open and its close, a burst).
pub const SCAN_DEBOUNCE: Duration = Duration::from_millis(300);
/// Video nodes watched at most.
pub const MAX_DEVICE_WATCHES: usize = 64;
/// Processes and open files per process a scan looks at, at most.
pub const MAX_PIDS: usize = 4096;
pub const MAX_FDS: usize = 4096;

/// Holders that are not an app's own use: PipeWire and its session manager hold the node for the
/// streams delivery 1 already names.
const DROPPED: [&str; 2] = ["pipewire", "wireplumber"];

/// What one scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// The holders of a video node.
    pub camera: Indicator,
    /// A bound cut the scan short: some processes or descriptors were not looked at.
    pub bounded: bool,
}

/// A node name the watch cares about.
pub fn is_video(name: &[u8]) -> bool {
    name.starts_with(b"video")
}

/// `scan_bounded` with the production bounds.
pub fn scan(proc_dir: &Path, dev_dir: &Path, uid: u32, self_pid: u32) -> Scan {
    scan_bounded(proc_dir, dev_dir, uid, self_pid, MAX_PIDS, MAX_FDS)
}

/// The holders of a `video*` node in `dev_dir`, among the processes of `uid` (never `self_pid`,
/// PipeWire or WirePlumber). Every error (a process gone, a directory unreadable) skips its entry.
pub fn scan_bounded(proc_dir: &Path, dev_dir: &Path, uid: u32, self_pid: u32, max_pids: usize, max_fds: usize) -> Scan {
    let mut out = Scan::default();
    let Ok(entries) = fs::read_dir(proc_dir) else {
        return out;
    };
    let mut seen = 0;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if seen == max_pids {
            out.bounded = true;
            break;
        }
        seen += 1;
        if pid == self_pid {
            continue;
        }
        let dir = entry.path();
        if fs::symlink_metadata(&dir).ok().map(|m| m.uid()) != Some(uid) {
            continue;
        }
        let (holds, cut) = holds_video(&dir.join("fd"), dev_dir, max_fds);
        out.bounded |= cut;
        if !holds {
            continue;
        }
        match fs::read_to_string(dir.join("comm")).map(|c| drawable(&c)) {
            Ok(name) if DROPPED.contains(&name.as_str()) => {}
            Ok(name) if !name.is_empty() => out.camera.hold(Some(&name)),
            _ => out.camera.hold(None),
        }
    }
    out
}

/// Whether one of the process's first `max_fds` descriptors links to a `video*` file directly in
/// `dev_dir`, and whether the bound cut the listing.
fn holds_video(fd_dir: &Path, dev_dir: &Path, max_fds: usize) -> (bool, bool) {
    let Ok(fds) = fs::read_dir(fd_dir) else {
        return (false, false);
    };
    for (n, fd) in fds.flatten().enumerate() {
        if n == max_fds {
            return (false, true);
        }
        let Ok(target) = fs::read_link(fd.path()) else {
            continue;
        };
        let video = target.file_name().is_some_and(|f| is_video(std::os::unix::ffi::OsStrExt::as_bytes(f)));
        if video && target.parent() == Some(dev_dir) {
            return (true, false);
        }
    }
    (false, false)
}

/// What one inotify record asks of the glue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// Something may have opened or closed a node (or records were lost): scan after the debounce.
    Trigger,
    /// A `video*` name appeared or changed on `dev_dir`: watch it (again).
    Appeared(String),
    /// A `video*` name went from `dev_dir`: forget it.
    Gone(String),
    Ignored,
}

/// An open or a close on a watched node.
// Read by `classify`, which camera_app (the next task) calls.
#[allow(dead_code)]
const NODE_MASK: u32 = libc::IN_OPEN | libc::IN_CLOSE_WRITE | libc::IN_CLOSE_NOWRITE;

/// The meaning of one record, `dev_wd` being the watch on `dev_dir` itself.
#[allow(dead_code)]
pub(crate) fn classify(ev: &Event, dev_wd: i32) -> Record {
    if ev.mask & libc::IN_Q_OVERFLOW != 0 {
        return Record::Trigger;
    }
    if ev.wd != dev_wd {
        return if ev.mask & NODE_MASK != 0 { Record::Trigger } else { Record::Ignored };
    }
    if !is_video(ev.name) {
        return Record::Ignored;
    }
    let name = || String::from_utf8_lossy(ev.name).into_owned();
    if ev.mask & (libc::IN_CREATE | libc::IN_ATTRIB) != 0 {
        Record::Appeared(name())
    } else if ev.mask & libc::IN_DELETE != 0 {
        Record::Gone(name())
    } else {
        Record::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::Event;
    use alpheus_commun::temp_path::TempPath;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    const SELF_PID: u32 = 999_999;

    fn euid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// A fake tree: `<root>/proc`, `<root>/dev/video0`.
    fn tree(name: &str) -> TempPath {
        let t = TempPath::dir(&format!("cam-{name}"));
        fs::create_dir_all(t.join("proc")).unwrap();
        fs::create_dir_all(t.join("dev")).unwrap();
        fs::write(t.join("dev/video0"), "").unwrap();
        t
    }

    /// A process holding `target` on fd 3, `comm` written when given.
    fn proc_with(t: &Path, pid: u32, comm: Option<&str>, target: &Path) {
        let d = t.join("proc").join(pid.to_string());
        fs::create_dir_all(d.join("fd")).unwrap();
        if let Some(c) = comm {
            fs::write(d.join("comm"), c).unwrap();
        }
        symlink(target, d.join("fd/3")).unwrap();
    }

    fn scan_of(t: &Path) -> Scan {
        scan(&t.join("proc"), &t.join("dev"), euid(), SELF_PID)
    }

    #[test]
    fn a_holder_is_named_by_its_comm() {
        let t = tree("named");
        proc_with(&t, 4242, Some("fakecam\n"), &t.join("dev/video0"));
        let s = scan_of(&t);
        assert_eq!(s.camera.names, vec!["fakecam".to_string()]);
        assert!(!s.camera.unnamed && !s.bounded);
    }

    #[test]
    fn two_pids_one_name() {
        let t = tree("two");
        proc_with(&t, 10, Some("fakecam\n"), &t.join("dev/video0"));
        proc_with(&t, 11, Some("fakecam\n"), &t.join("dev/video0"));
        assert_eq!(scan_of(&t).camera.names, vec!["fakecam".to_string()]);
    }

    #[test]
    fn pipewire_wireplumber_and_self_are_dropped() {
        let t = tree("drop");
        proc_with(&t, 20, Some("pipewire\n"), &t.join("dev/video0"));
        proc_with(&t, 21, Some("wireplumber\n"), &t.join("dev/video0"));
        proc_with(&t, SELF_PID, Some("alpheus-demon\n"), &t.join("dev/video0"));
        assert!(!scan_of(&t).camera.active());
    }

    #[test]
    fn non_video_and_other_dirs_are_ignored() {
        let t = tree("other");
        let d = t.join("proc/30");
        fs::create_dir_all(d.join("fd")).unwrap();
        fs::write(d.join("comm"), "recorder\n").unwrap();
        symlink(t.join("dev/snd0"), d.join("fd/3")).unwrap();
        symlink("/elsewhere/video0", d.join("fd/4")).unwrap();
        symlink("socket:[1]", d.join("fd/5")).unwrap();
        symlink(t.join("dev/sub/video0"), d.join("fd/6")).unwrap();
        assert!(!scan_of(&t).camera.active());
    }

    #[test]
    fn another_uid_is_skipped() {
        let t = tree("uid");
        proc_with(&t, 40, Some("fakecam\n"), &t.join("dev/video0"));
        let s = scan(&t.join("proc"), &t.join("dev"), euid() + 1, SELF_PID);
        assert!(!s.camera.active());
        assert!(!s.bounded);
    }

    #[test]
    fn vanished_or_unreadable_entries_are_skipped() {
        let t = tree("vanish");
        fs::create_dir_all(t.join("proc/50")).unwrap(); // no fd dir
        fs::create_dir_all(t.join("proc/self")).unwrap(); // not a pid
        proc_with(&t, 51, Some("recorder\n"), &t.join("dev/video0"));
        proc_with(&t, 52, None, &t.join("dev/video0")); // comm gone
        proc_with(&t, 53, Some("\u{200b}\n"), &t.join("dev/video0")); // cleans to empty
        let s = scan_of(&t);
        assert_eq!(s.camera.names, vec!["recorder".to_string()]);
        assert!(s.camera.unnamed);
        let none = scan(&t.join("missing"), &t.join("dev"), euid(), SELF_PID);
        assert!(!none.camera.active() && !none.bounded);
    }

    #[test]
    fn the_pid_bound_sets_bounded() {
        let t = tree("pids");
        for pid in [60, 61, 62] {
            proc_with(&t, pid, Some("fakecam\n"), &t.join("dev/video0"));
        }
        assert!(scan_bounded(&t.join("proc"), &t.join("dev"), euid(), SELF_PID, 2, 8).bounded);
        assert!(!scan_bounded(&t.join("proc"), &t.join("dev"), euid(), SELF_PID, 3, 8).bounded);
    }

    #[test]
    fn the_fd_bound_sets_bounded() {
        let t = tree("fds");
        let d = t.join("proc/70");
        fs::create_dir_all(d.join("fd")).unwrap();
        fs::write(d.join("comm"), "fakecam\n").unwrap();
        for n in 0..3 {
            symlink("socket:[1]", d.join(format!("fd/{n}"))).unwrap();
        }
        assert!(scan_bounded(&t.join("proc"), &t.join("dev"), euid(), SELF_PID, 8, 2).bounded);
        assert!(!scan_bounded(&t.join("proc"), &t.join("dev"), euid(), SELF_PID, 8, 3).bounded);
    }

    #[test]
    fn classify_table() {
        const DEV: i32 = 1;
        const NODE: i32 = 2;
        let ev = |wd, mask, name: &'static [u8]| Event { wd, mask, name };
        let c = |e: Event| classify(&e, DEV);
        assert_eq!(c(ev(DEV, libc::IN_Q_OVERFLOW, b"")), Record::Trigger);
        assert_eq!(c(ev(-1, libc::IN_Q_OVERFLOW, b"")), Record::Trigger);
        for m in [libc::IN_OPEN, libc::IN_CLOSE_WRITE, libc::IN_CLOSE_NOWRITE] {
            assert_eq!(c(ev(NODE, m, b"")), Record::Trigger, "{m:#x}");
        }
        assert_eq!(c(ev(DEV, libc::IN_OPEN, b"video0")), Record::Ignored, "an open on the dir itself is no node open");
        assert_eq!(c(ev(DEV, libc::IN_CREATE, b"video0")), Record::Appeared("video0".into()));
        assert_eq!(c(ev(DEV, libc::IN_ATTRIB, b"video12")), Record::Appeared("video12".into()));
        assert_eq!(c(ev(DEV, libc::IN_DELETE, b"video0")), Record::Gone("video0".into()));
        assert_eq!(c(ev(DEV, libc::IN_CREATE, b"snd0")), Record::Ignored);
        assert_eq!(c(ev(DEV, libc::IN_DELETE, b"tty0")), Record::Ignored);
        assert_eq!(c(ev(NODE, libc::IN_IGNORED, b"")), Record::Ignored);
        assert_eq!(c(ev(DEV, libc::IN_IGNORED, b"")), Record::Ignored);
        assert!(is_video(b"video0") && is_video(b"video") && !is_video(b"vid") && !is_video(b"xvideo0"));
    }
}
