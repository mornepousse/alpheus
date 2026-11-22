//! The application launcher (spec docs/specs/2026-10-05-alpheus-launcher-design.md):
//! one more pair of layer-shell surfaces of the daemon, built at open and
//! freed at close. Pure parts first (configuration, history, sources,
//! matching, model, view), the Wayland side last.

pub mod calc;
pub mod config;
pub mod desktop;
pub mod emoji;
pub mod history;
pub mod host;
pub mod matching;
pub mod model;
pub mod prefix;
pub mod run;
pub mod scan;
pub mod socket;
pub(crate) mod socket_glue;
pub mod sources;
pub mod surface;
pub mod tools;
pub mod view;

/// Test helpers shared by the launcher's modules.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    /// `$TMPDIR/alpheus-<pid>-<name>`, emptied and created; removed when dropped,
    /// so a test run leaves nothing behind (even when the test fails).
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new(name: &str) -> TempDir {
            let d = std::env::temp_dir().join(format!("alpheus-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
