use super::*;
use crate::launcher::testing::TempDir;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};
// `SourceConfig`, `Entries` and `Arc` come through `use super::*;`.
use crate::launcher::sources::SourceEntry;
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::exec::{CaptureEnd, Captured};

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

/// `<root>/bin` holding an executable per name: the entries' programs, since an entry whose
/// program is not on the path is absent.
fn programs(root: &Path, names: &[&str]) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for n in names {
        executable(&bin.join(n));
    }
    bin
}

/// The fixture trees in a temporary dir, plus a symlinked file (made here:
/// a symlink committed to git is fragile across checkouts and archives), with the
/// programs their entries (and the tests' own) name on the path.
fn tree(name: &str) -> (TempDir, ScanEnv) {
    let r = temp(name);
    copy_tree(Path::new(FIXTURES), &r);
    symlink(r.join("elsewhere/linked.desktop"), r.join("user/applications/org.example.Linked.desktop")).unwrap();
    let names = [
        "example-browser",
        "example-mail",
        "example-player",
        "example-settings",
        "example-terminal",
        "linked-app",
        "zeta-notes",
        "huge",
        "locked",
        "x",
    ];
    let env = ScanEnv {
        app_dirs: vec![r.join("user/applications"), r.join("system/applications")],
        path_dirs: vec![programs(&r, &names)],
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
fn a_non_utf8_override_is_ignored() {
    let (r, env) = tree("non-utf8");
    fs::write(r.join("user/applications/org.example.Terminal.desktop"), b"[Desktop Entry]\nName=\xff\nExec=x\n").unwrap();
    let apps = scan_apps(&env);
    let terminal = apps.iter().find(|a| a.id == "org.example.Terminal.desktop").expect("a non-UTF-8 override counts as absent");
    assert_eq!((terminal.name.as_str(), terminal.path.starts_with(r.join("system"))), ("Example Terminal (system)", true));
}

/// The case above also lacks `Type=`, so it is masked whether or not the bytes are read as
/// UTF-8 (`parse` itself rejects a missing `Type`). This one is otherwise a complete,
/// showable entry: only its invalid byte distinguishes the two readings.
#[test]
fn a_non_utf8_name_in_an_otherwise_valid_override_is_ignored() {
    let (r, env) = tree("non-utf8-valid");
    fs::write(r.join("user/applications/org.example.Terminal.desktop"), b"[Desktop Entry]\nType=Application\nName=\xff\nExec=x\n").unwrap();
    let apps = scan_apps(&env);
    let terminal = apps.iter().find(|a| a.id == "org.example.Terminal.desktop").expect("a non-UTF-8 override counts as absent");
    assert_eq!((terminal.name.as_str(), terminal.path.starts_with(r.join("system"))), ("Example Terminal (system)", true));
}

/// Two symlinks in the same applications dir resolving to the same subdirectory (an alias):
/// the `seen` dedup by (dev, ino) walks that directory once, through whichever symlink is
/// visited first. Sorting by file name before walking makes that choice (and hence the
/// resulting id) independent of readdir's own order.
#[test]
fn two_aliases_to_the_same_subdirectory_give_a_stable_id() {
    let r = temp("aliases");
    let apps_dir = r.join("applications");
    let real = apps_dir.join("real");
    fs::create_dir_all(&real).unwrap();
    fs::write(real.join("app.desktop"), "[Desktop Entry]\nType=Application\nName=Aliased App\nExec=aliased-app\n").unwrap();
    // Created in an order that does not match the expected (sorted) walk order, so a
    // dependency on readdir order would likely show up as the wrong id picked.
    symlink(&real, apps_dir.join("zzz-alias")).unwrap();
    symlink(&real, apps_dir.join("aaa-alias")).unwrap();
    let env = ScanEnv { app_dirs: vec![apps_dir], path_dirs: vec![programs(&r, &["aliased-app"])], locale: None, desktops: vec![] };
    assert_eq!(ids(&scan_apps(&env)), ["aaa-alias-app.desktop"], "the alphabetically-first alias claims the id");
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
    let names: Vec<String> = (0..8).map(|i| format!("app-{i}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let env = ScanEnv { app_dirs: vec![apps_dir], path_dirs: vec![programs(&r.join("progs"), &names)], locale: None, desktops: vec![] };
    assert_eq!(scan_apps_capped(&env, 5, MAX_FILES_PER_DIR).len(), 5);
    assert_eq!(scan_apps(&env).len(), 8);
    let bin = r.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for i in 0..8 {
        executable(&bin.join(format!("cmd-{i}")));
    }
    assert_eq!(scan_commands_capped(std::slice::from_ref(&bin), 5, MAX_ENTRIES_PER_PATH_DIR).len(), 5);
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

/// Through `scan_commands_capped` with a small limit: the real one (80,000 entries) would
/// write that many files per test run.
#[test]
fn the_path_scan_looks_at_a_bounded_number_of_entries_per_dir() {
    let r = temp("path-budget");
    let bin = r.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for i in 0..10 {
        executable(&bin.join(format!("cmd-{i}")));
    }
    assert_eq!(scan_commands_capped(std::slice::from_ref(&bin), MAX_COMMANDS, 5).len(), 5, "only the first 5 entries looked at");
    assert_eq!(scan_commands_capped(&[bin], MAX_COMMANDS, MAX_ENTRIES_PER_PATH_DIR).len(), 10);
}

/// A root-owned 0700 file the caller cannot run is not constructible in these tests (we do
/// not run as a second, unprivileged user against a root-owned file); this covers the
/// ordinary case of a file the caller owns, gated by whether it has an owner-exec bit at all.
#[test]
fn is_executable_file_respects_the_owner_exec_bit() {
    let r = temp("access-owner");
    let file = r.join("script");
    fs::write(&file, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!is_executable_file(&file), "0600: no exec bit at all");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(is_executable_file(&file), "0700: the owner can execute");
}

/// Unlike `0o600` vs `0o700` above, this tells `access(2)` apart from a bare mode-bits
/// check: as the file's owner, POSIX `access()` consults only the owner's permission bits,
/// so a file executable for "other" but not for its owner must still read as not executable
/// — the same category of mistake as the root-owned case the fix targets, reproduced
/// without needing a second uid.
#[test]
fn is_executable_file_checks_the_callers_own_category_not_any_bit() {
    let r = temp("access-category");
    let file = r.join("other-only");
    fs::write(&file, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o001)).unwrap();
    if !running_as_root() {
        assert!(!is_executable_file(&file), "executable for \"other\" only, but we are its owner");
    }
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

/// The apps scan alone, as before the sources: no plan, no second delivery.
fn apps_only(env: ScanEnv, run_scan: fn(&ScanEnv) -> Catalog, deliver: impl FnOnce(Catalog) + Send + 'static) -> io::Result<()> {
    spawn_with(env, run_scan, None, read_plan, deliver, |_| {})
}

fn empty() -> ScanEnv {
    ScanEnv { app_dirs: vec![], path_dirs: vec![], locale: None, desktops: vec![] }
}

fn outils() -> SourceConfig {
    SourceConfig { name: "Outils".into(), list: vec!["fake-list".into()], run: vec!["fake-run".into(), "{line}".into()] }
}

fn plan(fake: FakeExecutor) -> SourcePlan {
    SourcePlan { sources: vec![outils()], exec: Arc::new(fake) }
}

#[derive(Debug, PartialEq)]
enum Delivered {
    Catalog(Catalog),
    Entries(Entries),
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
    spawn_scan(env, None, move |c| tx.send((c, std::thread::current().name().map(str::to_string))).unwrap(), |_| {}).unwrap();
    let (catalog, thread) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((catalog, thread.as_deref()), (expected, Some(SCAN_THREAD)));
    assert!(rx.recv_timeout(Duration::from_secs(5)).is_err(), "delivered once, then the sender is dropped");
    assert!(no_scan_thread_within(Duration::from_secs(1)), "the scan thread outlived its delivery");
}

#[test]
fn a_panicking_scan_delivers_an_empty_catalog() {
    let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
    let (tx, rx) = mpsc::channel();
    apps_only(empty(), |_| panic!("scan exploded"), move |c| tx.send(c).unwrap()).unwrap();
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

/// `SCAN_RUNNING` must be cleared
/// BEFORE `deliver` runs, not after — simulated here by having `deliver`
/// itself try to start a second scan, exactly like a reopen racing the
/// first scan's own delivery. Before the fix, `_running`'s drop was the
/// closure's own last statement (after `deliver` returned), so this
/// nested attempt would see SCAN_RUNNING still set and be refused
/// (WouldBlock) even though the outer scan has nothing left to give it.
#[test]
fn scan_running_clears_before_deliver_so_a_racing_reopen_is_never_stuck() {
    let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
    let (result_tx, result_rx) = mpsc::channel();
    let (inner_tx, inner_rx) = mpsc::channel();
    apps_only(empty(), scan, move |_catalog| {
        let started_again = apps_only(empty(), scan, move |c| inner_tx.send(c).unwrap());
        result_tx.send(started_again.is_ok()).unwrap();
    })
    .unwrap();
    assert!(
        result_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "a reopen racing the catalog's delivery must see SCAN_RUNNING already clear, not WouldBlock"
    );
    assert_eq!(inner_rx.recv_timeout(Duration::from_secs(5)).unwrap(), Catalog::default());
    assert!(no_scan_thread_within(Duration::from_secs(1)));
}

#[test]
fn one_scan_at_a_time() {
    let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
    let (tx, rx) = mpsc::channel();
    let gate = GATE.lock().unwrap_or_else(|p| p.into_inner());
    let first = tx.clone();
    apps_only(empty(), stuck_scan, move |c| first.send(c).unwrap()).unwrap();
    let refused = apps_only(empty(), stuck_scan, |_| panic!("a second scan ran")).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock, "{refused}");
    drop(gate);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Catalog::default());
    assert!(no_scan_thread_within(Duration::from_secs(1)));
    apps_only(empty(), stuck_scan, move |c| tx.send(c).unwrap()).expect("the next open scans again");
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Catalog::default());
    assert!(no_scan_thread_within(Duration::from_secs(1)));
}

#[test]
fn sources_are_read_after_the_apps_and_delivered_second() {
    let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
    let fake = FakeExecutor::new();
    let out = "Captures · Zone\nVerrouiller\n".as_bytes().to_vec();
    fake.capture_ok("fake-list", Captured { stdout: out, end: CaptureEnd::Exited(Some(0)) });
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    spawn_scan(
        empty(),
        Some(plan(fake)),
        move |c| tx.send((Delivered::Catalog(c), std::thread::current().name().map(str::to_string))).unwrap(),
        move |e| tx2.send((Delivered::Entries(e), std::thread::current().name().map(str::to_string))).unwrap(),
    )
    .unwrap();
    let thread = Some(SCAN_THREAD.to_string());
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), (Delivered::Catalog(Catalog::default()), thread.clone()));
    let expected =
        Entries { sources: vec![outils()], items: vec![SourceEntry::new(0, "Captures · Zone"), SourceEntry::new(0, "Verrouiller")] };
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), (Delivered::Entries(expected), thread), "second, on the same thread");
    assert!(no_scan_thread_within(Duration::from_secs(1)));
}

#[test]
fn with_no_source_there_is_no_second_delivery() {
    let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
    let (tx, rx) = mpsc::channel();
    let (etx, erx) = mpsc::channel::<Entries>();
    spawn_scan(empty(), None, move |c| tx.send(c).unwrap(), move |e| etx.send(e).unwrap()).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Catalog::default());
    assert!(erx.recv_timeout(Duration::from_secs(1)).is_err(), "never called: its sender went with the thread");
    assert!(no_scan_thread_within(Duration::from_secs(1)));
}

#[test]
fn a_panicking_source_run_loses_its_entries_never_the_apps() {
    let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    spawn_with(
        empty(),
        scan,
        Some(plan(FakeExecutor::new())),
        |_| panic!("source exploded"),
        move |c| tx.send(Delivered::Catalog(c)).unwrap(),
        move |e| tx2.send(Delivered::Entries(e)).unwrap(),
    )
    .unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Delivered::Catalog(Catalog::default()));
    let none = Entries { sources: vec![outils()], items: vec![] };
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Delivered::Entries(none));
    assert!(no_scan_thread_within(Duration::from_secs(1)));
}

/// Holds an injected source run until the test lets it go, like a source stuck on its 1 s.
static SOURCE_GATE: Mutex<()> = Mutex::new(());

fn stuck_sources(plan: &SourcePlan) -> Entries {
    drop(SOURCE_GATE.lock().unwrap_or_else(|p| p.into_inner()));
    Entries { sources: plan.sources.clone(), items: vec![] }
}

/// Closed and reopened while the first open's sources still run.
#[test]
fn one_source_run_at_a_time_and_a_reopen_still_gets_its_apps() {
    let _serial = SPAWNS.lock().unwrap_or_else(|p| p.into_inner());
    let (tx, rx) = mpsc::channel();
    let gate = SOURCE_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let (c1, e1) = (tx.clone(), tx.clone());
    spawn_with(
        empty(),
        scan,
        Some(plan(FakeExecutor::new())),
        stuck_sources,
        move |_| c1.send("catalog 1").unwrap(),
        move |_| e1.send("entries 1").unwrap(),
    )
    .unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "catalog 1");
    let (c2, e2) = (tx.clone(), tx.clone());
    spawn_with(
        empty(),
        scan,
        Some(plan(FakeExecutor::new())),
        stuck_sources,
        move |_| c2.send("catalog 2").unwrap(),
        move |_| e2.send("entries 2").unwrap(),
    )
    .expect("the apps scan is free again");
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "catalog 2", "the reopen gets its apps at once");
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "nothing else while the run is stuck");
    drop(gate);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "entries 1", "the run in flight delivers; the reopen adopts it");
    drop(tx);
    assert!(rx.recv_timeout(Duration::from_secs(1)).is_err(), "no second source run was started");
    assert!(no_scan_thread_within(Duration::from_secs(1)));
}
