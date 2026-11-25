//! Tests of the locker lookup (`lockscreen.rs`): a synthetic `/proc` tree in a temporary folder.
//! Nothing here reads the real `/proc`.

use super::*;
use crate::launcher::testing::TempDir;
use std::fs;

const ME: u32 = 1000;

/// A process folder with its `comm` and its owner, as `/proc` shows them.
fn process(root: &Path, pid: &str, comm: &str, uid: u32) {
    state(root, pid, comm, uid, "S (sleeping)");
}

/// The same, in the given `State:`.
fn state(root: &Path, pid: &str, comm: &str, uid: u32, state: &str) {
    let dir = root.join(pid);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
    fs::write(dir.join("status"), format!("Name:\t{comm}\nState:\t{state}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n")).unwrap();
}

#[test]
fn the_known_lockers_are_told_by_name() {
    for comm in ["hyprlock", "swaylock", "gtklock", "waylock", "i3lock", "xsecurelock"] {
        assert!(is_locker(comm), "{comm}");
    }
    // A variant after a dash, and a Nix wrapper (its name is cut at 15 characters).
    for comm in ["swaylock-effects", "i3lock-color", ".hyprlock-wrapp", ".swaylock-wrapp"] {
        assert!(is_locker(comm), "{comm}");
    }
    // Not a locker: another program, a name that only starts like one, the empty name.
    for comm in ["alpheus-demon", "bash", "hyprland", "hyprlocker", "swaylockd", "lock", "mylock", ""] {
        assert!(!is_locker(comm), "{comm:?}");
    }
}

#[test]
fn a_running_locker_of_the_user_is_found() {
    let root = TempDir::new("lock-found");
    process(&root, "100", "bash", ME);
    process(&root, "200", "hyprlock", ME);
    assert!(locker_running(&root, ME));
}

#[test]
fn no_locker_means_not_locked() {
    let root = TempDir::new("lock-none");
    process(&root, "100", "bash", ME);
    process(&root, "101", "alpheus-demon", ME);
    assert!(!locker_running(&root, ME));
}

#[test]
fn another_users_locker_is_not_ours() {
    let root = TempDir::new("lock-other-user");
    process(&root, "200", "swaylock", ME + 1);
    assert!(!locker_running(&root, ME));
    assert!(locker_running(&root, ME + 1));
}

#[test]
fn only_numeric_folders_are_processes() {
    let root = TempDir::new("lock-numeric");
    process(&root, "self", "hyprlock", ME);
    process(&root, "sys", "hyprlock", ME);
    assert!(!locker_running(&root, ME));
}

#[test]
fn an_unreadable_proc_is_not_locked() {
    let root = TempDir::new("lock-unreadable");
    assert!(!locker_running(&root.join("missing"), ME), "no folder: the behaviour before the lookup");
    // A process whose files vanished between the listing and the read, or are odd.
    fs::create_dir_all(root.join("300")).unwrap();
    process(&root, "301", "swaylock", ME);
    fs::remove_file(root.join("301/status")).unwrap();
    assert!(!locker_running(&root, ME), "a locker whose owner cannot be read is not counted");
}

#[test]
fn a_zombie_locker_holds_no_lock() {
    let root = TempDir::new("lock-zombie");
    state(&root, "200", "hyprlock", ME, "Z (zombie)");
    assert!(!locker_running(&root, ME), "it has let go; its parent has not read its end");
    state(&root, "201", "hyprlock", ME, "R (running)");
    assert!(locker_running(&root, ME), "a live one beside it counts");
}
