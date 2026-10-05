//! The launcher's catalog, rebuilt at each open on a short-lived thread
//! (spec § 1): `.desktop` files from the XDG data dirs — no cache, a NixOS
//! rebuild changes every path — and the executables on $PATH. Bounded
//! everywhere (files, depth, counts); nothing outlives the delivery.

use super::desktop::{DesktopEntry, Locale, ParseEnv, Verdict, desktop_id, parse};
use crate::log::write as log;
use crate::sources::panic_text;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const SCAN_THREAD: &str = "launcher-scan";
pub const MAX_APPS: usize = 4_096;
pub const MAX_COMMANDS: usize = 20_000;
/// Larger .desktop files are skipped as invalid.
pub const MAX_DESKTOP_FILE: u64 = 64 * 1024;
/// Subdirectory depth under each applications dir.
pub const MAX_DEPTH: usize = 4;
/// Directory entries looked at per applications dir, whatever their kind
/// (bounds a pathological tree: thousands of other files, deep subdirectories).
const MAX_FILES_PER_DIR: usize = 4 * MAX_APPS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEnv {
    /// "<dir>/applications" for $XDG_DATA_HOME (default ~/.local/share) then each $XDG_DATA_DIRS
    /// entry (default /usr/local/share:/usr/share), in that priority order, duplicates removed.
    pub app_dirs: Vec<PathBuf>,
    /// $PATH entries in order, empty and duplicate entries removed.
    pub path_dirs: Vec<PathBuf>,
    pub locale: Option<Locale>,
    pub desktops: Vec<String>,
}

impl ScanEnv {
    /// Relative entries are dropped too (XDG spec; "." in $PATH is never searched).
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> ScanEnv {
        let absolute = |p: PathBuf| p.is_absolute().then_some(p);
        let split = |name: &str| -> Vec<PathBuf> {
            var(name).map(|v| std::env::split_paths(&v).filter_map(absolute).collect()).unwrap_or_default()
        };
        let data_home = var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .and_then(absolute)
            .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".local/share")).and_then(absolute));
        let mut data_dirs = split("XDG_DATA_DIRS");
        if data_dirs.is_empty() {
            data_dirs = vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")];
        }
        let desktops = var("XDG_CURRENT_DESKTOP")
            .and_then(|v| v.into_string().ok())
            .map(|v| v.split(':').filter(|d| !d.is_empty()).map(str::to_string).collect())
            .unwrap_or_default();
        ScanEnv {
            app_dirs: unique(data_home.into_iter().chain(data_dirs).map(|d| d.join("applications"))),
            path_dirs: unique(split("PATH")),
            locale: Locale::from_env(|k| var(k).and_then(|v| v.into_string().ok())),
            desktops,
        }
    }
}

fn unique(dirs: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    /// Shown apps, sorted by name (case-insensitive) then id.
    pub apps: Vec<DesktopEntry>,
    /// Executable file names on $PATH, sorted, unique (first dir wins), at most MAX_COMMANDS.
    pub commands: Vec<String>,
}

/// `.desktop` files under `dir`, sorted, and the number of directory entries
/// looked at: symlinks followed, at most MAX_DEPTH levels of subdirectories,
/// at most `budget` entries of any kind, unreadable dirs skipped, a directory
/// reached twice (symlink loop) walked once.
fn desktop_files(dir: &Path, budget: usize) -> (Vec<PathBuf>, usize) {
    struct Walk {
        seen: HashSet<(u64, u64)>,
        out: Vec<PathBuf>,
        looked: usize,
        budget: usize,
    }
    fn walk(d: &Path, depth: usize, w: &mut Walk) {
        let Ok(meta) = fs::metadata(d) else { return };
        if !w.seen.insert((meta.dev(), meta.ino())) {
            return;
        }
        let Ok(entries) = fs::read_dir(d) else { return };
        for e in entries.flatten() {
            if w.looked >= w.budget {
                return;
            }
            w.looked += 1;
            let p = e.path();
            let Ok(m) = fs::metadata(&p) else { continue }; // dangling symlink
            if m.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&p, depth + 1, w);
                }
            } else if m.is_file() && p.extension().is_some_and(|x| x == "desktop") {
                w.out.push(p);
            }
        }
    }
    let mut w = Walk { seen: HashSet::new(), out: Vec::new(), looked: 0, budget };
    walk(dir, 0, &mut w);
    w.out.sort();
    (w.out, w.looked)
}

/// None: unreadable, larger than MAX_DESKTOP_FILE, or not UTF-8.
fn read_small(file: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(file).ok()?.take(MAX_DESKTOP_FILE + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_DESKTOP_FILE {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// First occurrence of each desktop id claims it — a Mask verdict included — BEFORE filtering;
/// an invalid or unreadable file (or > MAX_DESKTOP_FILE) is as if absent, so a lower dir can supply that id.
pub fn scan_apps(env: &ScanEnv) -> Vec<DesktopEntry> {
    scan_apps_capped(env, MAX_APPS, MAX_FILES_PER_DIR)
}

/// `scan_apps` with its limits injected (tests use small ones).
fn scan_apps_capped(env: &ScanEnv, max_apps: usize, entries_per_dir: usize) -> Vec<DesktopEntry> {
    let has = |program: &str| has_program(&env.path_dirs, program);
    let parse_env = ParseEnv { locale: env.locale.as_ref(), desktops: &env.desktops, has_program: &has };
    let mut claimed = HashSet::new();
    let mut apps = Vec::new();
    'dirs: for dir in &env.app_dirs {
        for file in desktop_files(dir, entries_per_dir).0 {
            let Some(id) = desktop_id(dir, &file) else { continue };
            if claimed.contains(&id) {
                continue;
            }
            let Some(text) = read_small(&file) else { continue };
            match parse(&id, &file, &text, &parse_env) {
                Err(_) => {}
                Ok(Verdict::Mask(_)) => {
                    claimed.insert(id);
                }
                Ok(Verdict::Show(entry)) => {
                    claimed.insert(id);
                    apps.push(entry);
                    if apps.len() >= max_apps {
                        break 'dirs;
                    }
                }
            }
        }
    }
    apps.sort_by_cached_key(|e| (e.name.to_lowercase(), e.id.clone()));
    apps
}

fn is_executable_file(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Dot files are skipped (NixOS wrappers' `.foo-wrapped`), as are names that are not UTF-8.
pub fn scan_commands(path_dirs: &[PathBuf]) -> Vec<String> {
    scan_commands_capped(path_dirs, MAX_COMMANDS)
}

/// `scan_commands` with its limit injected (tests use a small one).
fn scan_commands_capped(path_dirs: &[PathBuf], max: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    'dirs: for dir in path_dirs {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let Ok(name) = e.file_name().into_string() else { continue };
            if name.starts_with('.') || seen.contains(&name) || !is_executable_file(&e.path()) {
                continue;
            }
            seen.insert(name);
            if seen.len() >= max {
                break 'dirs;
            }
        }
    }
    let mut commands: Vec<String> = seen.into_iter().collect();
    commands.sort();
    commands
}

/// Absolute path: an executable regular file; bare name: executable in one of `path_dirs`.
pub fn has_program(path_dirs: &[PathBuf], program: &str) -> bool {
    if program.starts_with('/') {
        return is_executable_file(Path::new(program));
    }
    !program.is_empty() && !program.contains('/') && path_dirs.iter().any(|d| is_executable_file(&d.join(program)))
}

pub fn scan(env: &ScanEnv) -> Catalog {
    Catalog { apps: scan_apps(env), commands: scan_commands(&env.path_dirs) }
}

/// Set while a scan thread runs: one scan at a time, so a scan stuck on a
/// hung NFS/FUSE mount cannot pile up one thread per open.
static SCAN_RUNNING: AtomicBool = AtomicBool::new(false);

/// Clears SCAN_RUNNING when the scan thread's work ends, however it ends
/// (also when the thread cannot start: the closure holding it is dropped).
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        SCAN_RUNNING.store(false, Ordering::Release);
    }
}

/// Runs `scan` on a thread named SCAN_THREAD that ends once `deliver` returns; a panic inside
/// is caught, logged, and delivers an empty Catalog. One scan at a time: while a previous scan
/// thread still runs, Err(WouldBlock) and no thread. Err also if the thread cannot start.
pub fn spawn_scan(env: ScanEnv, deliver: impl FnOnce(Catalog) + Send + 'static) -> io::Result<()> {
    spawn_with(env, scan, deliver)
}

/// `spawn_scan` with the scan injected (tests make it panic, or hang).
fn spawn_with(env: ScanEnv, run_scan: fn(&ScanEnv) -> Catalog, deliver: impl FnOnce(Catalog) + Send + 'static) -> io::Result<()> {
    if SCAN_RUNNING.swap(true, Ordering::AcqRel) {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "the previous scan is still running"));
    }
    let running = Running;
    std::thread::Builder::new().name(SCAN_THREAD.into()).spawn(move || {
        let _running = running;
        let catalog = catch_unwind(AssertUnwindSafe(|| run_scan(&env))).unwrap_or_else(|p| {
            log(&format!("launcher scan failed: {}", panic_text(&*p)));
            Catalog::default()
        });
        deliver(catalog);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::testing::TempDir;
    use std::os::unix::fs::symlink;
    use std::sync::{Mutex, mpsc};
    use std::time::{Duration, Instant};

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/desktop-tree");

    fn running_as_root() -> bool {
        // SAFETY: geteuid(2) cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    /// An empty temporary dir, removed when the returned guard drops.
    fn temp(name: &str) -> TempDir {
        TempDir::new(&format!("scan-{name}"))
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if e.file_type().unwrap().is_dir() {
                copy_tree(&src, &dst);
            } else {
                fs::copy(&src, &dst).unwrap();
            }
        }
    }

    /// The fixture trees in a temporary dir, plus a symlinked file (made here:
    /// a symlink committed to git is fragile across checkouts and archives).
    fn tree(name: &str) -> (TempDir, ScanEnv) {
        let r = temp(name);
        copy_tree(Path::new(FIXTURES), &r);
        symlink(r.join("elsewhere/linked.desktop"), r.join("user/applications/org.example.Linked.desktop")).unwrap();
        let env = ScanEnv {
            app_dirs: vec![r.join("user/applications"), r.join("system/applications")],
            path_dirs: vec![],
            locale: None,
            desktops: vec![],
        };
        (r, env)
    }

    fn ids(apps: &[DesktopEntry]) -> Vec<&str> {
        apps.iter().map(|a| a.id.as_str()).collect()
    }

    fn executable(path: &Path) {
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let owned: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| owned.iter().find(|(n, _)| n == k).map(|(_, v)| OsString::from(v))
    }

    fn texts(dirs: &[PathBuf]) -> Vec<&str> {
        dirs.iter().map(|p| p.to_str().unwrap()).collect()
    }

    #[test]
    fn scan_env_defaults() {
        let e = ScanEnv::from_env(vars(&[("HOME", "/home/u")]));
        assert_eq!(texts(&e.app_dirs), ["/home/u/.local/share/applications", "/usr/local/share/applications", "/usr/share/applications"]);
        assert_eq!((e.path_dirs.len(), e.locale, e.desktops.len()), (0, None, 0));
        assert_eq!(texts(&ScanEnv::from_env(vars(&[])).app_dirs), ["/usr/local/share/applications", "/usr/share/applications"]);
    }

    #[test]
    fn scan_env_keeps_nixos_order_and_drops_duplicates() {
        let e = ScanEnv::from_env(vars(&[
            ("HOME", "/home/u"),
            ("XDG_DATA_HOME", "/home/u/.local/share"),
            (
                "XDG_DATA_DIRS",
                "/home/u/.nix-profile/share:/etc/profiles/per-user/u/share:/run/current-system/sw/share::relative/share:/home/u/.nix-profile/share",
            ),
            ("PATH", "/run/wrappers/bin::/home/u/.nix-profile/bin:/run/current-system/sw/bin:/run/wrappers/bin:bin"),
            ("XDG_CURRENT_DESKTOP", "Hyprland:wlroots"),
            ("LANG", "fr_FR.UTF-8"),
        ]));
        assert_eq!(
            texts(&e.app_dirs),
            [
                "/home/u/.local/share/applications",
                "/home/u/.nix-profile/share/applications",
                "/etc/profiles/per-user/u/share/applications",
                "/run/current-system/sw/share/applications"
            ]
        );
        assert_eq!(texts(&e.path_dirs), ["/run/wrappers/bin", "/home/u/.nix-profile/bin", "/run/current-system/sw/bin"]);
        assert_eq!(e.desktops, ["Hyprland", "wlroots"]);
        assert_eq!(e.locale, Locale::parse("fr_FR"));
    }

    #[test]
    fn apps_are_sorted_by_name_and_the_first_id_wins() {
        let (r, env) = tree("sorted");
        let apps = scan_apps(&env);
        assert_eq!(
            ids(&apps),
            [
                "org.example.Mail.desktop",
                "org.example.Player.desktop",
                "kde-example-settings.desktop",
                "org.example.Terminal.desktop",
                "org.example.Linked.desktop",
                "only-user.desktop"
            ]
        );
        let terminal = apps.iter().find(|a| a.id == "org.example.Terminal.desktop").unwrap();
        assert_eq!((terminal.name.as_str(), terminal.path.starts_with(r.join("user"))), ("Example Terminal (user)", true));
    }

    #[test]
    fn user_override_masks_and_broken_override_is_ignored() {
        let (r, env) = tree("override");
        let apps = scan_apps(&env);
        assert!(!apps.iter().any(|a| a.id == "org.example.Browser.desktop"), "a NoDisplay override hides the system entry");
        let mail = apps.iter().find(|a| a.id == "org.example.Mail.desktop").expect("a broken override counts as absent");
        assert_eq!((mail.name.as_str(), mail.path.starts_with(r.join("system"))), ("Example Mail", true));
        // An override above MAX_DESKTOP_FILE, and an unreadable one: absent too.
        let mut huge = String::from("[Desktop Entry]\nType=Application\nName=Huge\nExec=huge\n");
        huge.push_str(&"# padding\n".repeat(MAX_DESKTOP_FILE as usize / 10 + 1));
        fs::write(r.join("user/applications/org.example.Player.desktop"), huge).unwrap();
        fs::create_dir_all(r.join("user/applications/kde")).unwrap();
        let locked = r.join("user/applications/kde/example-settings.desktop");
        fs::write(&locked, "[Desktop Entry]\nType=Application\nName=Locked\nExec=locked\n").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let apps = scan_apps(&env);
        let name = |id: &str| apps.iter().find(|a| a.id == id).map(|a| a.name.clone());
        assert_eq!(name("org.example.Player.desktop").as_deref(), Some("example player"));
        if !running_as_root() {
            assert_eq!(name("kde-example-settings.desktop").as_deref(), Some("Example Settings"));
        }
        // The usual two-line override ("[Desktop Entry]" + NoDisplay=true) hides the system entry.
        fs::write(r.join("user/applications/org.example.Mail.desktop"), "[Desktop Entry]\nNoDisplay=true\n").unwrap();
        assert!(!scan_apps(&env).iter().any(|a| a.id == "org.example.Mail.desktop"));
    }

    #[test]
    fn symlink_loops_and_unreadable_dirs_do_not_abort_the_scan() {
        let (r, env) = tree("loops");
        let before = scan_apps(&env).len();
        symlink(r.join("user/applications"), r.join("user/applications/loop")).unwrap();
        let locked = r.join("user/applications/locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(locked.join("hidden-away.desktop"), "[Desktop Entry]\nType=Application\nName=Hidden away\nExec=x\n").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let apps = scan_apps(&env);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if !running_as_root() {
            assert_eq!(apps.len(), before, "{:?}", ids(&apps));
        }
        assert!(ids(&apps).contains(&"only-user.desktop"));
    }

    /// Through the `*_capped` functions with small limits: the real ones
    /// (4,096 apps, 20,000 commands) would write 24,000 files per test run.
    #[test]
    fn the_catalog_is_capped() {
        let r = temp("caps");
        let apps_dir = r.join("applications");
        fs::create_dir_all(&apps_dir).unwrap();
        for i in 0..8 {
            let text = format!("[Desktop Entry]\nType=Application\nName=App {i}\nExec=app-{i}\n");
            fs::write(apps_dir.join(format!("app-{i}.desktop")), text).unwrap();
        }
        let env = ScanEnv { app_dirs: vec![apps_dir], path_dirs: vec![], locale: None, desktops: vec![] };
        assert_eq!(scan_apps_capped(&env, 5, MAX_FILES_PER_DIR).len(), 5);
        assert_eq!(scan_apps(&env).len(), 8);
        let bin = r.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for i in 0..8 {
            executable(&bin.join(format!("cmd-{i}")));
        }
        assert_eq!(scan_commands_capped(std::slice::from_ref(&bin), 5).len(), 5);
        assert_eq!(scan_commands(&[bin]).len(), 8);
    }

    #[test]
    fn the_walk_looks_at_a_bounded_number_of_entries() {
        let r = temp("entries");
        fs::create_dir_all(r.join("sub")).unwrap();
        for i in 0..50 {
            fs::write(r.join(format!("note-{i}.txt")), "").unwrap();
            fs::write(r.join(format!("sub/other-{i}.txt")), "").unwrap();
        }
        let (files, looked) = desktop_files(&r, 20);
        assert_eq!((files.len(), looked), (0, 20), "entries that are not .desktop files count too");
        assert_eq!(desktop_files(&r, 1000).1, 101, "every entry, the subdirectory and its files included");
    }

    #[test]
    fn commands_are_executable_files_sorted_and_unique() {
        let r = temp("commands");
        let (a, b) = (r.join("a"), r.join("b"));
        fs::create_dir_all(a.join("subdir")).unwrap();
        fs::create_dir_all(&b).unwrap();
        executable(&a.join("tool"));
        executable(&a.join(".tool-wrapped"));
        fs::write(a.join("notes"), "plain\n").unwrap();
        executable(&r.join("target"));
        symlink(r.join("target"), a.join("linked")).unwrap();
        symlink(r.join("nowhere"), b.join("dangling")).unwrap();
        executable(&b.join("tool"));
        executable(&b.join("other"));
        assert_eq!(scan_commands(&[a, b, r.join("missing")]), ["linked", "other", "tool"]);
    }

    #[test]
    fn has_program_both_forms() {
        let root = temp("has");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        executable(&bin.join("present"));
        fs::write(bin.join("plain"), "").unwrap();
        let dirs = [bin.clone()];
        assert!(has_program(&dirs, "present"));
        assert!(!has_program(&dirs, "plain"), "not executable");
        assert!(!has_program(&dirs, "absent"));
        assert!(has_program(&dirs, bin.join("present").to_str().unwrap()));
        assert!(!has_program(&dirs, bin.join("plain").to_str().unwrap()));
        assert!(!has_program(&dirs, bin.to_str().unwrap()), "a directory");
        assert!(!has_program(&[bin.parent().unwrap().to_path_buf()], "bin/present"), "a relative path is not searched");
        assert!(!has_program(&dirs, ""));
    }

    /// The spawn tests look for threads by name: one at a time.
    static SPAWNS: Mutex<()> = Mutex::new(());

    fn scan_threads() -> usize {
        fs::read_dir("/proc/self/task")
            .unwrap()
            .flatten()
            .filter(|t| fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim_end() == SCAN_THREAD))
            .count()
    }

    fn no_scan_thread_within(limit: Duration) -> bool {
        let start = Instant::now();
        while scan_threads() > 0 {
            if start.elapsed() > limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    #[test]
    fn spawn_scan_delivers_once_and_the_thread_is_gone() {
        let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
        let (_root, env) = tree("spawn");
        let expected = scan(&env);
        let (tx, rx) = mpsc::channel();
        spawn_scan(env, move |c| tx.send((c, std::thread::current().name().map(str::to_string))).unwrap()).unwrap();
        let (catalog, thread) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((catalog, thread.as_deref()), (expected, Some(SCAN_THREAD)));
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_err(), "delivered once, then the sender is dropped");
        assert!(no_scan_thread_within(Duration::from_secs(1)), "the scan thread outlived its delivery");
    }

    #[test]
    fn a_panicking_scan_delivers_an_empty_catalog() {
        let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
        let env = ScanEnv { app_dirs: vec![], path_dirs: vec![], locale: None, desktops: vec![] };
        let (tx, rx) = mpsc::channel();
        spawn_with(env, |_| panic!("scan exploded"), move |c| tx.send(c).unwrap()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Catalog::default());
        assert!(no_scan_thread_within(Duration::from_secs(1)));
    }

    /// Holds the injected scan inside its thread until the test lets it go,
    /// like a directory on a hung network mount.
    static GATE: Mutex<()> = Mutex::new(());

    fn stuck_scan(_: &ScanEnv) -> Catalog {
        drop(GATE.lock().unwrap_or_else(|p| p.into_inner()));
        Catalog::default()
    }

    #[test]
    fn one_scan_at_a_time() {
        let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
        let empty = || ScanEnv { app_dirs: vec![], path_dirs: vec![], locale: None, desktops: vec![] };
        let (tx, rx) = mpsc::channel();
        let gate = GATE.lock().unwrap_or_else(|p| p.into_inner());
        let first = tx.clone();
        spawn_with(empty(), stuck_scan, move |c| first.send(c).unwrap()).unwrap();
        let refused = spawn_with(empty(), stuck_scan, |_| panic!("a second scan ran")).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock, "{refused}");
        drop(gate);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Catalog::default());
        assert!(no_scan_thread_within(Duration::from_secs(1)));
        spawn_with(empty(), stuck_scan, move |c| tx.send(c).unwrap()).expect("the next open scans again");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Catalog::default());
        assert!(no_scan_thread_within(Duration::from_secs(1)));
    }
}
