//! The launcher's catalog, rebuilt at each open on a short-lived thread
//! (spec § 1): `.desktop` files from the XDG data dirs — no cache, a NixOS
//! rebuild changes every path — and the executables on $PATH. Bounded
//! everywhere (files, depth, counts); nothing outlives the delivery.
//! Then, with sources configured, their lines (`sources.rs`), delivered second.

use super::config::SourceConfig;
use super::desktop::{DesktopEntry, Locale, ParseEnv, Verdict, desktop_id, parse};
use super::sources::{self, Entries};
use crate::log::write as log;
use crate::sources::panic_text;
use alpheus_commun::exec::Executor;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
/// Directory entries looked at per $PATH dir, whatever their kind (bounds a pathological
/// directory of mostly-non-executable files: the budget below is on commands actually found).
const MAX_ENTRIES_PER_PATH_DIR: usize = 4 * MAX_COMMANDS;

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
        // Sorted by file name so that, when two paths reach the same inode (a symlinked
        // subdirectory alias), the first one in that order always claims it — independent of
        // readdir's own order, which is unspecified. Bounded by the remaining budget: a
        // pathological directory is never fully materialized just to be sorted.
        let remaining = w.budget.saturating_sub(w.looked);
        let mut entries: Vec<_> = entries.flatten().take(remaining).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
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

/// A regular file the caller can actually execute, checked with `access(2)` rather than the
/// mode bits: a mode bit set for some other category (or a root-owned file whose only exec bit
/// belongs to its owner) does not mean this process can run it.
fn is_executable_file(p: &Path) -> bool {
    if !fs::metadata(p).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated string for the lifetime of this call; access(2)
    // only reads it and does not retain the pointer.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// Dot files are skipped (NixOS wrappers' `.foo-wrapped`), as are names that are not UTF-8.
pub fn scan_commands(path_dirs: &[PathBuf]) -> Vec<String> {
    scan_commands_capped(path_dirs, MAX_COMMANDS, MAX_ENTRIES_PER_PATH_DIR)
}

/// `scan_commands` with its limits injected (tests use small ones).
fn scan_commands_capped(path_dirs: &[PathBuf], max: usize, entries_per_dir: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    'dirs: for dir in path_dirs {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for (looked, e) in entries.flatten().enumerate() {
            if looked >= entries_per_dir {
                break;
            }
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

/// What one open reads after its apps (sources spec § 2): its configured sources, and the
/// executor that runs their `list` (the daemon's own).
pub struct SourcePlan {
    pub sources: Vec<SourceConfig>,
    pub exec: Arc<dyn Executor>,
}

/// The real source run: each source in turn (`sources::read_all`), its failures logged.
fn read_plan(plan: &SourcePlan) -> Entries {
    sources::read_all(&plan.sources, plan.exec.as_ref(), &mut |m| log(m))
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

/// Set while a thread reads sources (up to 8 × 1 s): one run at a time across opens, so a quick
/// close and reopen never piles up a second one. The reopen still gets its apps at once
/// (SCAN_RUNNING is free by then) and adopts the run in flight (`host::entries_are_current`).
static SOURCES_RUNNING: AtomicBool = AtomicBool::new(false);

/// Clears SOURCES_RUNNING however the run ends.
struct SourcesRunning;

impl Drop for SourcesRunning {
    fn drop(&mut self) {
        SOURCES_RUNNING.store(false, Ordering::Release);
    }
}

/// One open's second delivery (sources spec § 6): its sources, unless another open's are still
/// being read (then nothing: this open adopts those). A panic is caught and logged; the entries
/// are then empty, the apps already delivered.
fn read_sources_after(plan: &SourcePlan, run_sources: fn(&SourcePlan) -> Entries, deliver: impl FnOnce(Entries)) {
    if SOURCES_RUNNING.swap(true, Ordering::AcqRel) {
        log("launcher sources: the previous open's are still being read; this open adopts them");
        return;
    }
    let running = SourcesRunning;
    let entries = catch_unwind(AssertUnwindSafe(|| run_sources(plan))).unwrap_or_else(|p| {
        log(&format!("launcher sources failed: {}", panic_text(&*p)));
        Entries { sources: plan.sources.clone(), items: Vec::new() }
    });
    // Cleared before `deliver`, for SCAN_RUNNING's reason (review M1): a reopen racing this delivery.
    drop(running);
    deliver(entries);
}

/// Runs `scan` on a thread named SCAN_THREAD: `deliver` gets the catalog first, exactly as before
/// the sources; then, with a `plan`, the same thread reads the sources (`read_sources_after`) and
/// `deliver_entries` gets them — never with no plan. A panic in the scan delivers an empty
/// Catalog. One apps scan at a time: while a previous one runs, Err(WouldBlock) and no thread (no
/// sources either). Err also if the thread cannot start.
pub fn spawn_scan(
    env: ScanEnv,
    plan: Option<SourcePlan>,
    deliver: impl FnOnce(Catalog) + Send + 'static,
    deliver_entries: impl FnOnce(Entries) + Send + 'static,
) -> io::Result<()> {
    spawn_with(env, scan, plan, read_plan, deliver, deliver_entries)
}

/// `spawn_scan` with the scan and the source run injected (tests make them panic, or hang).
fn spawn_with(
    env: ScanEnv,
    run_scan: fn(&ScanEnv) -> Catalog,
    plan: Option<SourcePlan>,
    run_sources: fn(&SourcePlan) -> Entries,
    deliver: impl FnOnce(Catalog) + Send + 'static,
    deliver_entries: impl FnOnce(Entries) + Send + 'static,
) -> io::Result<()> {
    if SCAN_RUNNING.swap(true, Ordering::AcqRel) {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "the previous scan is still running"));
    }
    let running = Running;
    std::thread::Builder::new().name(SCAN_THREAD.into()).spawn(move || {
        let catalog = catch_unwind(AssertUnwindSafe(|| run_scan(&env))).unwrap_or_else(|p| {
            log(&format!("launcher scan failed: {}", panic_text(&*p)));
            Catalog::default()
        });
        // Cleared BEFORE `deliver`, not after (review item M1, final review
        // slice 1): `deliver` hands the catalog to the event loop (a channel
        // send), which may already be processed — and so may trigger a
        // reopen that calls `spawn_scan` again — before this thread's own
        // stack unwinds. A reopen racing that delivery must see SCAN_RUNNING
        // already clear and start a fresh scan of its own, not WouldBlock on
        // a scan that, by the time `deliver` runs, has nothing left to give it.
        drop(running);
        deliver(catalog);
        if let Some(plan) = plan {
            read_sources_after(&plan, run_sources, deliver_entries);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
