//! The laptop's backlight, read once per `alpheus-launch --osd brightness`, and when
//! the quick panel opens (or 150 ms after its brightness command):
//! `<backlight_dir>/<device>/brightness` and `max_brightness`, each a small number. Read only and
//! capped (`read_capped`); never watched, never written. The brightness keys change it outside
//! alpheus, then call the verb.
//!
//! Never:
//! - write a level or watch the device: the OSD shows what it is asked to show, and a watcher would
//!   pop the pill up every time an idle daemon dimmed the screen (`the_osd_only_reads`,
//!   docs/decisions.md, "Brightness is shown on request, read once");
//! - panic on what sysfs says (`the_osd_has_no_panicking_path`), or start a thread or a command
//!   (`the_tray_spawns_no_thread` has this file on its list).
//!
//! See: docs/features/osd.md ("Brightness"), docs/features/quick-settings.md (the brightness row),
//! docs/configuration.md (`[osd] backlight`, `backlight_dir`).

use crate::osd;
use alpheus_common::theme::settings::read_capped;
use std::path::{Path, PathBuf};

/// Entries of the backlight directory looked at, at most (a laptop lists one or two).
pub const MAX_ENTRIES: usize = 64;

/// A device name as sysfs lists it: no path, nothing hidden.
pub(crate) fn plain(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0']) && !name.chars().any(char::is_control)
}

/// The device directory: `wanted` when given (and only it), else the first entry in sorted order
/// that is a directory (sysfs lists symlinks to the devices).
pub fn device(dir: &Path, wanted: &str) -> Option<PathBuf> {
    if !wanted.is_empty() {
        let d = dir.join(wanted);
        return (plain(wanted) && d.is_dir()).then_some(d);
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .take(MAX_ENTRIES)
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| plain(n))
        .collect();
    names.sort();
    names.into_iter().map(|n| dir.join(n)).find(|d| d.is_dir())
}

/// The device's name (what `brightnessctl --device=` takes) and its level, read once (the quick
/// panel's row); None as `level`.
pub fn named_level(dir: &Path, wanted: &str) -> Option<(String, u8)> {
    let d = device(dir, wanted)?;
    let name = d.file_name()?.to_str()?.to_string();
    let brightness = read_capped(&d.join("brightness")).ok()?;
    let max = read_capped(&d.join("max_brightness")).ok()?;
    Some((name, osd::brightness_percent(&brightness, &max)?))
}

/// A re-read of the device the quick panel shows (150 ms after its brightness command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reread {
    Level(u8),
    /// The device is there but its files did not read as a level this time (a transient sysfs
    /// error): the last level stands.
    Unreadable,
    /// The device's directory went: its row with it.
    Gone,
}

/// Re-reads `device` (a name `named_level` gave) under `dir`.
pub fn reread(dir: &Path, device: &str) -> Reread {
    if device.is_empty() || self::device(dir, device).is_none() {
        return Reread::Gone;
    }
    match named_level(dir, device) {
        Some((_, p)) => Reread::Level(p),
        None => Reread::Unreadable,
    }
}

/// The backlight's level in percent, read once; None without a device, or with a file that is not
/// a small number, or a zero maximum.
pub fn level(dir: &Path, wanted: &str) -> Option<u8> {
    named_level(dir, wanted).map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh fake backlight directory (never /sys).
    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-backlight-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake(root: &Path, name: &str, brightness: &str, max: &str) {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("brightness"), brightness).unwrap();
        fs::write(d.join("max_brightness"), max).unwrap();
    }

    #[test]
    fn the_first_device_in_sorted_order_unless_one_is_named() {
        let root = tree("sorted");
        fake(&root, "intel_backlight", "9696\n", "19393\n");
        fake(&root, "acpi_video0", "3\n", "12\n");
        fake(&root, "nv_backlight", "3\n", "4\n");
        // The directory's own order (creation, its reverse, or a hash of the names) must not put
        // the sorted first first, or the sort goes untested: renamed until it does not.
        let listed_first = || fs::read_dir(&root).unwrap().next().unwrap().unwrap().file_name();
        let mut first = "acpi_video0".to_string();
        for i in 1..=32 {
            if listed_first() != first.as_str() {
                break;
            }
            let next = format!("acpi_video{i}");
            fs::rename(root.join(&first), root.join(&next)).unwrap();
            first = next;
        }
        assert_ne!(listed_first(), first.as_str(), "the directory lists another device first");
        assert_eq!(device(&root, ""), Some(root.join(&first)), "{first} sorts first");
        assert_eq!(level(&root, ""), Some(25), "{first} sorts first");
        assert_eq!(level(&root, "intel_backlight"), Some(50), "the configured one");
        assert_eq!(level(&root, "nope"), None, "a named device that is not there: nothing, never another one");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn no_device_or_no_directory_is_nothing() {
        let root = tree("empty");
        assert_eq!(level(&root, ""), None, "no device");
        fs::write(root.join("a-stray"), "1\n").unwrap();
        assert_eq!(level(&root, ""), None, "a plain file is no device");
        fake(&root, "panel", "50\n", "100\n");
        assert_eq!(level(&root, ""), Some(50), "the stray file sorts first and is skipped");
        assert_eq!(level(&root.join("missing"), ""), None, "no directory");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_device_reached_through_a_symlink_is_read() {
        let root = tree("symlink");
        let real = tree("symlink-target");
        fake(&real, "panel", "50\n", "100\n");
        std::os::unix::fs::symlink(real.join("panel"), root.join("panel")).unwrap();
        assert_eq!(level(&root, ""), Some(50), "sysfs lists symlinks to the devices");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&real);
    }

    #[test]
    fn hostile_files_show_nothing_and_never_block() {
        let root = tree("hostile");
        fake(&root, "zero", "5\n", "0\n");
        assert_eq!(level(&root, "zero"), None, "a zero maximum");
        fake(&root, "words", "lots\n", "100\n");
        assert_eq!(level(&root, "words"), None);
        fake(&root, "huge", &"9".repeat(100_000), "100\n");
        assert_eq!(level(&root, "huge"), None, "capped, then refused: more than 10 digits");
        let fifo = root.join("fifo");
        fs::create_dir_all(&fifo).unwrap();
        fs::write(fifo.join("max_brightness"), "100\n").unwrap();
        let path = std::ffi::CString::new(fifo.join("brightness").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(level(&root, "fifo"), None, "a FIFO is not a regular file: refused, never read (it would block)");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_named_level_says_which_device() {
        let root = tree("named");
        fake(&root, "intel_backlight", "9696\n", "19393\n");
        assert_eq!(named_level(&root, ""), Some(("intel_backlight".to_string(), 50)), "brightnessctl's --device");
        assert_eq!(named_level(&root, "nope"), None);
        let _ = fs::remove_dir_all(&root);
    }

    /// One failed read is not a device gone.
    #[test]
    fn a_re_read_tells_an_unreadable_device_from_a_gone_one() {
        let root = tree("reread");
        fake(&root, "panel", "50\n", "100\n");
        assert_eq!(reread(&root, "panel"), Reread::Level(50));
        fs::write(root.join("panel").join("brightness"), "").unwrap();
        assert_eq!(reread(&root, "panel"), Reread::Unreadable, "a file that does not read as a level");
        fs::remove_file(root.join("panel").join("max_brightness")).unwrap();
        assert_eq!(reread(&root, "panel"), Reread::Unreadable, "a file missing for now");
        fs::remove_dir_all(root.join("panel")).unwrap();
        assert_eq!(reread(&root, "panel"), Reread::Gone, "the device's directory went");
        assert_eq!(reread(&root, ""), Reread::Gone, "no device shown");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_named_device_is_a_plain_name() {
        let root = tree("plain");
        // Files at the root itself: `.` would read them.
        fs::write(root.join("brightness"), "50\n").unwrap();
        fs::write(root.join("max_brightness"), "100\n").unwrap();
        fake(&root, "panel", "10\n", "100\n");
        for bad in [".", "..", "panel/..", "../plain"] {
            assert_eq!(level(&root, bad), None, "{bad:?}");
        }
        assert_eq!(level(&root, "panel"), Some(10));
        let _ = fs::remove_dir_all(&root);
    }
}
