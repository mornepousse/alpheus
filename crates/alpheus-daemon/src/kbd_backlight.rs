//! The keyboard's backlight, read when the quick panel opens (or 150 ms after its keyboard
//! backlight command): the first `<kbd_backlight_dir>/*::kbd_backlight` LED in sorted order, its
//! `brightness` and `max_brightness`, each a small number. Read only and capped (`read_capped`);
//! never watched, never written: `brightnessctl --class=leds` changes it (`quick/mod.rs`,
//! `kbd_command`). The level is the device's own (three on a ThinkPad: off, low, high), shown as a
//! percentage of its maximum.
//!
//! Never:
//! - write a level, watch the device or run a command (`the_tray_spawns_no_thread` has this file on
//!   its `NO_THREAD_NOR_COMMAND` list, `the_radio_readers_only_read` lists it);
//! - take another LED for the backlight (a capslock or micmute LED), or a device whose maximum is 0
//!   or above `MAX_LEVEL`;
//! - panic on what sysfs says (`the_radio_readers_have_no_panicking_path`).
//!
//! See: docs/features/quick-settings.md (the keyboard backlight row), docs/configuration.md
//! (`[quick] kbd_backlight`, `kbd_backlight_dir`).

use crate::backlight::{MAX_ENTRIES, plain};
use crate::osd::parse_level;
use alpheus_common::theme::settings::read_capped;
use std::path::Path;

/// What a keyboard backlight's LED name ends with (`tpacpi::kbd_backlight`, `asus::kbd_backlight`).
pub const SUFFIX: &str = "::kbd_backlight";
/// The largest maximum taken: a larger `max_brightness` hides the row (a level must fit a `u8`, and
/// no keyboard has that many).
pub const MAX_LEVEL: u8 = 255;

/// The keyboard backlight as the panel shows it: its LED (`brightnessctl --device=`), its level and
/// its maximum (1 to `MAX_LEVEL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kbd {
    pub device: String,
    pub level: u8,
    pub max: u8,
}

impl Kbd {
    /// The level as a percentage of the maximum, rounded; 0 with a zero maximum.
    pub fn percent(&self) -> u8 {
        if self.max == 0 {
            return 0;
        }
        let (l, m) = (u32::from(self.level.min(self.max)), u32::from(self.max));
        ((l * 100 + m / 2) / m) as u8
    }

    /// One step up or down: one level when the maximum is at most 10, else `step_pct` percent of the
    /// maximum (rounded, at least one level); clamped to `0..=max`.
    pub fn stepped(&self, up: bool, step_pct: u8) -> u8 {
        let m = u32::from(self.max);
        let step = if self.max <= 10 { 1 } else { ((m * u32::from(step_pct) + 50) / 100).max(1) };
        let from = u32::from(self.level.min(self.max));
        let to = if up { (from + step).min(m) } else { from.saturating_sub(step) };
        to as u8
    }

    /// The level nearest to `percent` of the maximum (a press on the bar), never above it.
    pub fn nearest(&self, percent: u8) -> u8 {
        let (p, m) = (u32::from(percent.min(100)), u32::from(self.max));
        ((p * m + 50) / 100) as u8
    }

    /// The next level, the maximum wrapping to 0 (Space and Enter).
    pub fn cycled(&self) -> u8 {
        if self.level >= self.max { 0 } else { self.level + 1 }
    }
}

/// `device` under `dir` read as a level and a maximum; None when it is not there, a file is not a
/// small number, or the maximum is 0 or above `MAX_LEVEL`. A level above the maximum is the maximum.
fn read(dir: &Path, device: &str) -> Option<Kbd> {
    let d = dir.join(device);
    if !plain(device) || !d.is_dir() {
        return None;
    }
    let max = parse_level(&read_capped(&d.join("max_brightness")).ok()?)?;
    let max = u8::try_from(max).ok().filter(|m| *m >= 1)?;
    let level = parse_level(&read_capped(&d.join("brightness")).ok()?)?.min(u32::from(max)) as u8;
    Some(Kbd { device: device.to_string(), level, max })
}

/// The keyboard backlight: the first LED in sorted order whose name ends with `SUFFIX` and that
/// reads as a level (at most `MAX_ENTRIES` names looked at); None hides the row.
pub fn find(dir: &Path) -> Option<Kbd> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .take(MAX_ENTRIES)
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| plain(n) && n.ends_with(SUFFIX))
        .collect();
    names.sort();
    names.iter().find_map(|n| read(dir, n))
}

/// A re-read of the LED the panel shows (150 ms after its command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdReread {
    Level(u8),
    /// The LED is there but its files did not read as a level this time: the last level stands.
    Unreadable,
    /// The LED's directory went: its row with it.
    Gone,
}

/// Re-reads `device` (a name `find` gave) under `dir`.
pub fn reread(dir: &Path, device: &str) -> KbdReread {
    if !plain(device) || !dir.join(device).is_dir() {
        return KbdReread::Gone;
    }
    match read(dir, device) {
        Some(k) => KbdReread::Level(k.level),
        None => KbdReread::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A fresh fake LED directory (never /sys).
    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-leds-{name}", std::process::id()));
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

    fn kbd(level: u8, max: u8) -> Kbd {
        Kbd { device: "tpacpi::kbd_backlight".into(), level, max }
    }

    #[test]
    fn kbd_backlight_row_hidden_without_a_led() {
        let root = tree("none");
        assert_eq!(find(&root), None, "an empty directory");
        assert_eq!(find(&root.join("missing")), None, "no directory");
        fake(&root, "input0::capslock", "0\n", "1\n");
        assert_eq!(find(&root), None, "a capslock LED is no keyboard backlight");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn kbd_backlight_reads_level_and_max() {
        let root = tree("read");
        fake(&root, "input0::capslock", "1\n", "1\n");
        fake(&root, "tpacpi::kbd_backlight", "1\n", "2\n");
        assert_eq!(find(&root), Some(Kbd { device: "tpacpi::kbd_backlight".into(), level: 1, max: 2 }));
        fake(&root, "asus::kbd_backlight", "3\n", "3\n");
        assert_eq!(find(&root).map(|k| k.device), Some("asus::kbd_backlight".into()), "two matches: the first sorted");
        let _ = fs::remove_dir_all(&root);
        for (case, max) in [("zero", "0\n"), ("huge", "256\n"), ("words", "lots\n")] {
            let root = tree(case);
            fake(&root, "tpacpi::kbd_backlight", "1\n", max);
            assert_eq!(find(&root), None, "max {max:?}");
            let _ = fs::remove_dir_all(&root);
        }
        let root = tree("fifo");
        let d = root.join("tpacpi::kbd_backlight");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("max_brightness"), "2\n").unwrap();
        let path = std::ffi::CString::new(d.join("brightness").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(find(&root), None, "a FIFO is refused, never read (it would block)");
        let _ = fs::remove_dir_all(&root);
        let root = tree("above");
        fake(&root, "tpacpi::kbd_backlight", "9\n", "2\n");
        assert_eq!(find(&root), Some(kbd(2, 2)), "a level above max counts as max");
        let _ = fs::remove_dir_all(&root);
    }

    /// One failed read is not a device gone.
    #[test]
    fn kbd_backlight_reread_tells_gone_from_unreadable() {
        let root = tree("reread");
        let name = "tpacpi::kbd_backlight";
        fake(&root, name, "1\n", "2\n");
        assert_eq!(reread(&root, name), KbdReread::Level(1));
        fs::write(root.join(name).join("brightness"), "").unwrap();
        assert_eq!(reread(&root, name), KbdReread::Unreadable, "a file that does not read as a level");
        fs::remove_file(root.join(name).join("max_brightness")).unwrap();
        assert_eq!(reread(&root, name), KbdReread::Unreadable, "a file missing for now");
        fs::remove_dir_all(root.join(name)).unwrap();
        assert_eq!(reread(&root, name), KbdReread::Gone, "the device's directory went");
        assert_eq!(reread(&root, ""), KbdReread::Gone, "no device shown");
        assert_eq!(reread(&root, ".."), KbdReread::Gone, "never a path");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn kbd_backlight_steps_by_level_or_by_percent() {
        assert_eq!(kbd(1, 2).stepped(true, 5), 2, "one level at a time on a small max");
        assert_eq!(kbd(2, 2).stepped(true, 5), 2, "clamped to max");
        assert_eq!(kbd(0, 2).stepped(false, 5), 0, "clamped to 0");
        assert_eq!(kbd(1, 2).stepped(false, 5), 0);
        assert_eq!(kbd(10, 10).stepped(false, 25), 9, "max 10 still steps by one level");
        assert_eq!(kbd(100, 255).stepped(true, 5), 113, "5 % of 255: 13");
        assert_eq!(kbd(100, 255).stepped(false, 5), 87);
        assert_eq!(kbd(250, 255).stepped(true, 5), 255);
        assert_eq!(kbd(5, 20).stepped(true, 1), 6, "at least one level");
        assert_eq!(kbd(0, 2).nearest(50), 1);
        assert_eq!(kbd(0, 2).nearest(100), 2);
        assert_eq!(kbd(0, 2).nearest(4), 0);
        assert_eq!(kbd(0, 255).nearest(200), 255, "never above max");
        assert_eq!(kbd(1, 2).percent(), 50);
        assert_eq!(kbd(1, 3).percent(), 33);
        assert_eq!(kbd(2, 3).percent(), 67);
        assert_eq!(kbd(9, 2).percent(), 100, "never above 100");
        assert_eq!(Kbd { max: 0, ..kbd(0, 0) }.percent(), 0, "a zero max never divides");
    }

    #[test]
    fn kbd_backlight_cycles_and_allows_zero() {
        assert_eq!(kbd(0, 2).cycled(), 1);
        assert_eq!(kbd(1, 2).cycled(), 2);
        assert_eq!(kbd(2, 2).cycled(), 0, "max wraps to 0: an unlit keyboard");
        assert_eq!(kbd(9, 2).cycled(), 0, "above max wraps too");
    }
}
