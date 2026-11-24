//! A test's own temporary directory, removed when its guard drops (even while a
//! failed assert unwinds), so test runs leave nothing behind in `$TMPDIR`.
//!
//! Never:
//! - leave a directory behind: the guard removes it even while a failed assert unwinds, so a test
//!   run leaves nothing in `$TMPDIR` (AGENTS.md: the worktrees and the scratch files of an agent);
//! - be used outside a test.
//!
//! See: CONTRIBUTING.md (the tests' isolation), docs/ARCHITECTURE.md ("Tests").

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// A path inside `$TMPDIR/alpheus-<pid>-<name>`; dropping it removes that whole directory.
///
/// Derefs to the path. It is not `Clone`: `guard.clone()` or `guard.join(…)` give a plain
/// `PathBuf`, so the directory lives as long as the guard itself; `below` keeps the guard.
#[derive(Debug)]
pub struct TempPath {
    root: PathBuf,
    path: PathBuf,
}

impl TempPath {
    /// `$TMPDIR/alpheus-<pid>-<name>`, emptied first and not created.
    pub fn new(name: &str) -> TempPath {
        let root = std::env::temp_dir().join(format!("alpheus-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        TempPath { path: root.clone(), root }
    }

    /// Like `new`, the directory created.
    pub fn dir(name: &str) -> TempPath {
        let t = TempPath::new(name);
        std::fs::create_dir_all(&t.root).unwrap();
        t
    }

    /// A path below this one, under the same guard.
    pub fn below(mut self, part: impl AsRef<Path>) -> TempPath {
        let path = self.path.join(part);
        // `self` drops with an empty root: removing "" fails, and is ignored.
        let root = std::mem::take(&mut self.root);
        TempPath { root, path }
    }
}

impl Deref for TempPath {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

impl AsRef<Path> for TempPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_guard_removes_its_directory() {
        let t = TempPath::dir("temp-path-drop");
        let root = t.to_path_buf();
        std::fs::write(t.join("f"), "x").unwrap();
        assert!(root.is_dir());
        drop(t);
        assert!(!root.exists(), "the directory goes with its guard");
    }

    #[test]
    fn a_joined_guard_removes_the_root_not_just_its_leaf() {
        let root = TempPath::new("temp-path-join").to_path_buf();
        let leaf = TempPath::new("temp-path-join").below("a").below("b.toml");
        assert_eq!(*leaf, root.join("a").join("b.toml"));
        std::fs::create_dir_all(leaf.parent().unwrap()).unwrap();
        std::fs::write(&leaf, "x").unwrap();
        drop(leaf);
        assert!(!root.exists(), "the whole root goes");
    }

    #[test]
    fn a_clone_is_a_plain_path_and_the_guard_still_owns_the_directory() {
        let t = TempPath::dir("temp-path-clone");
        let p: PathBuf = t.clone();
        drop(p);
        assert!(t.is_dir());
    }
}
