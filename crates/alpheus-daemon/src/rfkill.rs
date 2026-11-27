//! The radios' kill switches, read when the quick panel opens (or 300 ms after a radio command):
//! at most `MAX_ENTRIES` entries `<rfkill_dir>/rfkill*/{type,soft,hard}`, each a short word or a
//! `0`/`1`. Only the Wi-Fi, Bluetooth and WWAN switches count (`Kind`); a GPS, NFC or UWB one is
//! left alone. Read only and capped (`read_capped`); never watched, never written: `rfkill
//! block|unblock wlan bluetooth wwan` changes them (`quick/mod.rs`, `airplane_command`).
//!
//! Never:
//! - write a switch, open `/dev/rfkill`, watch the switches or run a command (`the_tray_spawns_no_thread`
//!   has this file on its `NO_THREAD_NOR_COMMAND` list, `the_radio_readers_only_read` lists it);
//! - count a hard-blocked radio as one airplane mode can switch: a kill switch wins;
//! - panic on what sysfs says (`the_radio_readers_have_no_panicking_path`).
//!
//! See: docs/features/quick-settings.md (the airplane mode row), docs/configuration.md
//! (`[quick] airplane`, `rfkill_dir`).

use crate::backlight::plain;
use alpheus_common::theme::settings::read_capped;
use std::path::Path;

/// Entries of the rfkill directory looked at, at most (a laptop lists two to four).
pub const MAX_ENTRIES: usize = 32;

/// The radio types airplane mode switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wlan,
    Bluetooth,
    Wwan,
}

impl Kind {
    /// From the `type` file's word; None for any other type (`gps`, `nfc`, `uwb`, `fm`).
    fn of(word: &str) -> Option<Kind> {
        match word {
            "wlan" => Some(Kind::Wlan),
            "bluetooth" => Some(Kind::Bluetooth),
            "wwan" => Some(Kind::Wwan),
            _ => None,
        }
    }
}

/// One switch: its type, blocked by software (`rfkill block`) and by hardware (a kill switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Radio {
    pub kind: Kind,
    pub soft: bool,
    pub hard: bool,
}

/// The switches of the three kinds, never empty (`read` gives None instead).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Radios(pub Vec<Radio>);

/// What the airplane mode row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Airplane {
    /// Every radio rfkill can switch is soft-blocked.
    On,
    Off,
    /// Every radio is held off by a kill switch: nothing to switch.
    HardBlocked,
}

impl Radios {
    /// The radios rfkill can switch are those not hard-blocked: none of them, `HardBlocked`; all of
    /// them soft-blocked, `On`; else `Off`.
    pub fn state(&self) -> Airplane {
        let mut usable = self.0.iter().filter(|r| !r.hard).peekable();
        if usable.peek().is_none() {
            return Airplane::HardBlocked;
        }
        if usable.all(|r| r.soft) { Airplane::On } else { Airplane::Off }
    }
}

/// `0` or `1`, a trailing newline allowed; None for anything else.
fn flag(path: &Path) -> Option<bool> {
    match read_capped(path).ok()?.trim_end_matches('\n') {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// One entry under `dir`: None when it is not a switch of the three kinds or a file does not read.
fn radio(dir: &Path, name: &str) -> Option<Radio> {
    if !plain(name) || !name.starts_with("rfkill") {
        return None;
    }
    let d = dir.join(name);
    let kind = Kind::of(read_capped(&d.join("type")).ok()?.trim_end_matches('\n'))?;
    Some(Radio { kind, soft: flag(&d.join("soft"))?, hard: flag(&d.join("hard"))? })
}

/// The Wi-Fi, Bluetooth and WWAN switches under `dir` (at most `MAX_ENTRIES` entries looked at);
/// None without one, which hides the row.
pub fn read(dir: &Path) -> Option<Radios> {
    let radios: Vec<Radio> = std::fs::read_dir(dir)
        .ok()?
        .take(MAX_ENTRIES)
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| radio(dir, &n))
        .collect();
    (!radios.is_empty()).then_some(Radios(radios))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A fresh fake rfkill directory (never /sys).
    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-rfkill-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake(root: &Path, name: &str, kind: &str, soft: &str, hard: &str) {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("type"), format!("{kind}\n")).unwrap();
        fs::write(d.join("soft"), soft).unwrap();
        fs::write(d.join("hard"), hard).unwrap();
    }

    /// The airplane state of a fresh directory holding `entries` (name, type, soft, hard).
    fn state_of(case: &str, entries: &[(&str, &str, &str, &str)]) -> Option<Airplane> {
        let root = tree(case);
        for (name, kind, soft, hard) in entries {
            fake(&root, name, kind, soft, hard);
        }
        let s = read(&root).map(|r| r.state());
        let _ = fs::remove_dir_all(&root);
        s
    }

    #[test]
    fn airplane_hidden_without_a_radio() {
        let root = tree("none");
        assert_eq!(read(&root), None, "an empty directory");
        assert_eq!(read(&root.join("missing")), None, "no directory");
        let _ = fs::remove_dir_all(&root);
        assert_eq!(state_of("gps", &[("rfkill0", "gps", "0\n", "0\n"), ("rfkill1", "nfc", "1\n", "0\n")]), None, "only GPS and NFC");
    }

    #[test]
    fn airplane_state_from_sysfs() {
        let (wlan, bt) = ("rfkill0", "rfkill1");
        assert_eq!(state_of("on", &[(wlan, "wlan", "1\n", "0\n"), (bt, "bluetooth", "1\n", "0\n")]), Some(Airplane::On));
        assert_eq!(state_of("off", &[(wlan, "wlan", "0\n", "0\n"), (bt, "bluetooth", "1\n", "0\n")]), Some(Airplane::Off), "one radio on");
        assert_eq!(
            state_of("hard-on", &[(wlan, "wlan", "0\n", "1\n"), (bt, "bluetooth", "1\n", "0\n")]),
            Some(Airplane::On),
            "a kill switch on the Wi-Fi, the Bluetooth soft-blocked: nothing on"
        );
        assert_eq!(
            state_of("hard-off", &[(wlan, "wlan", "0\n", "1\n"), (bt, "bluetooth", "0\n", "0\n")]),
            Some(Airplane::Off),
            "a kill switch on the Wi-Fi, the Bluetooth on"
        );
        assert_eq!(state_of("all-hard", &[(wlan, "wlan", "0\n", "1\n"), (bt, "bluetooth", "1\n", "1\n")]), Some(Airplane::HardBlocked));
        assert_eq!(
            state_of("gps", &[(wlan, "wlan", "1\n", "0\n"), ("rfkill2", "gps", "0\n", "0\n")]),
            Some(Airplane::On),
            "an unblocked GPS is not a radio airplane mode switches"
        );
        assert_eq!(state_of("wwan", &[(wlan, "wlan", "1\n", "0\n"), ("rfkill2", "wwan", "0\n", "0\n")]), Some(Airplane::Off));
        for (case, soft) in [("two", "2\n"), ("words", "blocked\n"), ("empty", "")] {
            assert_eq!(
                state_of(case, &[(wlan, "wlan", "1\n", "0\n"), (bt, "bluetooth", soft, "0\n")]),
                Some(Airplane::On),
                "a soft of {soft:?}: that entry skipped"
            );
        }
        assert_eq!(state_of("not-rfkill", &[("foo", "wlan", "0\n", "0\n"), (bt, "bluetooth", "1\n", "0\n")]), Some(Airplane::On));
        let root = tree("fifo");
        fake(&root, wlan, "wlan", "1\n", "0\n");
        let d = root.join(bt);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("type"), "bluetooth\n").unwrap();
        fs::write(d.join("hard"), "0\n").unwrap();
        let path = std::ffi::CString::new(d.join("soft").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert_eq!(read(&root).map(|r| r.state()), Some(Airplane::On), "a FIFO is refused, never read (it would block)");
        let _ = fs::remove_dir_all(&root);
        let root = tree("many");
        for i in 0..40 {
            fake(&root, &format!("rfkill{i}"), "wlan", "1\n", "0\n");
        }
        assert_eq!(read(&root).map(|r| r.0.len()), Some(MAX_ENTRIES), "40 entries: only 32 read");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn airplane_reads_each_radio() {
        let root = tree("each");
        fake(&root, "rfkill3", "bluetooth", "0\n", "1\n");
        assert_eq!(read(&root), Some(Radios(vec![Radio { kind: Kind::Bluetooth, soft: false, hard: true }])));
        let _ = fs::remove_dir_all(&root);
    }
}
