//! A camera opened outside PipeWire (privacy delivery 2, spec §§ 1-2): the pure parts. `scan` finds
//! who holds a `video*` node by reading `<proc_dir>/<pid>/fd` links (never by opening anything under
//! `dev_dir`), and `classify` turns one inotify record into what the glue should do with it. Events
//! are triggers, never counts: the state shown comes from a scan alone.

use crate::log::write as log;
use crate::pipewire::drawable;
use crate::privacy::Indicator;
use crate::watch::{Event, Events, add_watch};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
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
const NODE_MASK: u32 = libc::IN_OPEN | libc::IN_CLOSE_WRITE | libc::IN_CLOSE_NOWRITE;

/// On `dev_dir`: names appearing, going, or changing their attributes (udev giving a node its group).
const DIR_MASK: u32 = libc::IN_CREATE | libc::IN_DELETE | libc::IN_ATTRIB | libc::IN_ONLYDIR;

/// The meaning of one record, `dev_wd` being the watch on `dev_dir` itself.
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

/// The inotify instance on `dev_dir` and on each of its `video*` nodes: an open or a close of a node
/// is a trigger, never a count. Nothing under `dev_dir` is ever opened, only watched.
pub struct CameraWatch {
    fd: OwnedFd,
    dev_dir: PathBuf,
    dev_wd: i32,
    /// Watch descriptor of a node to its name; one entry per distinct watched node.
    nodes: HashMap<i32, String>,
    /// Names whose watch was refused (root-only until udev gives the node its group): retried on
    /// their next `IN_ATTRIB` or `IN_CREATE`, forgotten on `IN_DELETE`.
    pending: HashSet<String>,
    limit_logged: bool,
}

impl CameraWatch {
    /// Watches `dev_dir`, then every `video*` node in it. Err: why `dev_dir` cannot be watched.
    pub fn start(dev_dir: &Path) -> Result<CameraWatch, String> {
        // SAFETY: inotify_init1 only takes flags; it returns a new descriptor or -1.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(format!("inotify unavailable ({})", io::Error::last_os_error()));
        }
        // SAFETY: `raw` was just returned by inotify_init1 and belongs to nothing else.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let dev_wd = add_watch(&fd, dev_dir, DIR_MASK).map_err(|e| format!("cannot watch {} ({e})", dev_dir.display()))?;
        let mut w =
            CameraWatch { fd, dev_dir: dev_dir.to_path_buf(), dev_wd, nodes: HashMap::new(), pending: HashSet::new(), limit_logged: false };
        let entries = fs::read_dir(dev_dir).map_err(|e| format!("cannot list {} ({e})", dev_dir.display()))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(name) = name.to_str().filter(|n| is_video(n.as_bytes())) {
                w.watch_node(name);
            }
        }
        Ok(w)
    }

    /// A duplicate of the inotify fd for the event loop (the same instance).
    pub fn watch_fd(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }

    /// Distinct video nodes watched.
    pub fn watched(&self) -> usize {
        self.nodes.len()
    }

    /// Reads every queued record (until EAGAIN); true when any asks for a scan.
    pub fn drain(&mut self) -> bool {
        let mut trigger = false;
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: read(2) writes at most `buf.len()` bytes into a buffer this function owns.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            // -1: EAGAIN (drained) or an error; 0 never comes from inotify. Either way, done.
            let Ok(n) = usize::try_from(n) else { break };
            if n == 0 {
                break;
            }
            for ev in Events(buf.get(..n).unwrap_or_default()) {
                match classify(&ev, self.dev_wd) {
                    Record::Trigger => trigger = true,
                    Record::Appeared(name) => {
                        self.watch_node(&name);
                        trigger = true;
                    }
                    Record::Gone(name) => {
                        self.forget(&name);
                        trigger = true;
                    }
                    Record::Ignored => {}
                }
            }
        }
        trigger
    }

    /// Adds the node watch. The kernel returns the same descriptor for the same inode, so a node
    /// already watched is not counted twice; a refusal goes to `pending`.
    fn watch_node(&mut self, name: &str) {
        match add_watch(&self.fd, &self.dev_dir.join(name), NODE_MASK) {
            Ok(wd) => {
                self.pending.remove(name);
                if self.nodes.contains_key(&wd) {
                    self.nodes.insert(wd, name.to_string());
                } else if self.nodes.len() >= MAX_DEVICE_WATCHES {
                    // SAFETY: removes the watch just added on this instance.
                    unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), wd) };
                    if !self.limit_logged {
                        self.limit_logged = true;
                        log(&format!("privacy: device watch limit reached ({MAX_DEVICE_WATCHES})"));
                    }
                } else {
                    self.nodes.insert(wd, name.to_string());
                }
            }
            Err(e) => {
                let refused = matches!(e.raw_os_error(), Some(libc::EACCES | libc::EPERM));
                // A name already pending was reported (or is an expected refusal) the first time.
                if self.pending.insert(name.to_string()) && !refused {
                    log(&format!("privacy: camera devices not watched ({e})"));
                }
            }
        }
    }

    /// The name went: its watch is dropped by the kernel (IN_IGNORED, then ignored by `classify`).
    fn forget(&mut self, name: &str) {
        self.pending.remove(name);
        self.nodes.retain(|_, n| n != name);
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

    // ---- CameraWatch ----

    fn dev(name: &str, nodes: &[&str]) -> TempPath {
        let t = TempPath::dir(&format!("cw-{name}"));
        for n in nodes {
            fs::write(t.join(n), "").unwrap();
        }
        t
    }

    #[test]
    fn an_open_and_a_close_trigger() {
        let t = dev("open", &["video0"]);
        let mut w = CameraWatch::start(&t).unwrap();
        assert_eq!(w.watched(), 1);
        drop(fs::File::open(t.join("video0")).unwrap());
        assert!(w.drain());
        assert!(!w.drain());
    }

    #[test]
    fn a_node_appearing_is_watched_then_triggers() {
        let t = dev("appear", &[]);
        let mut w = CameraWatch::start(&t).unwrap();
        assert_eq!(w.watched(), 0);
        fs::write(t.join("video1"), "").unwrap();
        assert!(w.drain());
        assert_eq!(w.watched(), 1);
        drop(fs::File::open(t.join("video1")).unwrap());
        assert!(w.drain());
    }

    #[test]
    fn a_refused_node_is_retried_on_its_attrib() {
        use std::os::unix::fs::PermissionsExt;
        if euid() == 0 {
            eprintln!("skipped: root can watch a mode 000 node");
            return;
        }
        let t = dev("refused", &[]);
        fs::write(t.join("video2"), "").unwrap();
        fs::set_permissions(t.join("video2"), fs::Permissions::from_mode(0o000)).unwrap();
        let mut w = CameraWatch::start(&t).unwrap();
        assert_eq!(w.watched(), 0);
        fs::set_permissions(t.join("video2"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(w.drain());
        assert_eq!(w.watched(), 1);
        drop(fs::File::open(t.join("video2")).unwrap());
        assert!(w.drain(), "a later open triggers");
    }

    #[test]
    fn a_node_touched_again_is_not_counted_twice() {
        use std::os::unix::fs::PermissionsExt;
        let t = dev("twice", &["video0"]);
        let mut w = CameraWatch::start(&t).unwrap();
        fs::set_permissions(t.join("video0"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(w.drain());
        assert_eq!(w.watched(), 1);
    }

    #[test]
    fn a_deleted_node_is_forgotten() {
        let t = dev("gone", &["video0"]);
        let mut w = CameraWatch::start(&t).unwrap();
        fs::remove_file(t.join("video0")).unwrap();
        assert!(w.drain());
        assert_eq!(w.watched(), 0);
        assert!(!w.drain(), "its IN_IGNORED is not a trigger");
    }

    #[test]
    fn other_names_never_trigger() {
        let t = dev("other", &["video0"]);
        let mut w = CameraWatch::start(&t).unwrap();
        fs::write(t.join("snd0"), "").unwrap();
        assert!(!w.drain());
    }

    #[test]
    fn the_watch_limit_holds() {
        let names: Vec<String> = (0..65).map(|i| format!("video{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let t = dev("limit", &refs);
        let mut w = CameraWatch::start(&t).unwrap();
        assert_eq!(w.watched(), MAX_DEVICE_WATCHES);
        // An attrib on an unwatched one past the limit changes nothing.
        for n in &names {
            let _ = fs::set_permissions(t.join(n), <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600));
        }
        w.drain();
        assert_eq!(w.watched(), MAX_DEVICE_WATCHES);
    }

    #[test]
    fn a_missing_dev_dir_is_an_error() {
        let t = dev("missing", &[]);
        let err = CameraWatch::start(&t.join("nope")).err().unwrap();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn a_watched_node_at_the_limit_keeps_its_watch_on_attrib() {
        use std::os::unix::fs::PermissionsExt;
        let names: Vec<String> = (0..MAX_DEVICE_WATCHES).map(|i| format!("video{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let t = dev("limit-attrib", &refs);
        let mut w = CameraWatch::start(&t).unwrap();
        assert_eq!(w.watched(), MAX_DEVICE_WATCHES);
        fs::set_permissions(t.join("video7"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(w.drain());
        assert_eq!(w.watched(), MAX_DEVICE_WATCHES);
        drop(fs::File::open(t.join("video7")).unwrap());
        assert!(w.drain(), "its kernel watch survived the attrib");
    }
}
