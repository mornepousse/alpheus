//! The launcher's history, for frecency ranking: which entries were
//! launched, how often, how recently. It holds entry ids ("app:<desktop
//! id>", "cmd:<name>") and numbers only, never typed text — the API takes
//! ids. File `$XDG_STATE_HOME/alpheus/launcher-history`, one line per entry
//! "<last>\t<count>\t<id>", bounded, written atomically.

use super::config::HISTORY_MAX;
use std::collections::HashSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "launcher-history";
const COUNT_MAX: u32 = 10_000;
/// Longer ids are refused (and skipped when read).
const ID_MAX: usize = 256;
/// What `load` reads at most: the largest valid file, HISTORY_MAX lines of
/// "<last: 20 digits>\t<count: 5 digits>\t<id>\n" (≈ 277 KiB; 200 typical entries ≈ 12 KiB).
const READ_MAX: u64 = (HISTORY_MAX * (20 + 1 + 5 + 1 + ID_MAX + 1)) as u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub id: String,
    pub count: u32,
    /// Unix seconds.
    pub last: u64,
}

/// `History::default()` has a limit of 0: disabled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct History {
    entries: Vec<Use>,
    limit: usize,
}

/// Recency weight: < 1 h → 100, < 1 day → 70, < 1 week → 50, < 30 days → 30, else 10.
pub fn weight(age_secs: u64) -> u32 {
    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;
    match age_secs {
        a if a < HOUR => 100,
        a if a < DAY => 70,
        a if a < 7 * DAY => 50,
        a if a < 30 * DAY => 30,
        _ => 10,
    }
}

/// frecency = min(count, 1000) * weight(now.saturating_sub(last)).
fn frecency_of(u: &Use, now: u64) -> u32 {
    u.count.min(1000) * weight(now.saturating_sub(u.last))
}

fn valid_id(id: &str) -> bool {
    // U+FFFD is what lossy UTF-8 decoding leaves behind for invalid bytes (see `load`):
    // an id carrying it came from a corrupted line, not a real entry.
    !id.is_empty() && id.len() <= ID_MAX && !id.contains(['\t', '\n', '\r']) && !id.contains('\u{FFFD}')
}

impl History {
    pub fn empty(limit: usize) -> History {
        History { entries: Vec::new(), limit }
    }

    /// Unparseable lines skipped; ids with '\t' or '\n' rejected; over `limit`, lowest frecency evicted.
    /// A repeated id keeps its first line.
    pub fn parse(text: &str, limit: usize, now: u64) -> History {
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for line in text.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(last), Some(count), Some(id)) = (fields.next(), fields.next(), fields.next()) else { continue };
            let (Ok(last), Ok(count)) = (last.parse::<u64>(), count.parse::<u32>()) else { continue };
            if count == 0 || !valid_id(id) || !seen.insert(id) {
                continue;
            }
            entries.push(Use { id: id.to_string(), count: count.min(COUNT_MAX), last });
        }
        if entries.len() > limit {
            // Stable: among equal frecencies, file order decides.
            entries.sort_by_key(|u| std::cmp::Reverse(frecency_of(u, now)));
            entries.truncate(limit);
        }
        History { entries, limit }
    }

    /// Absent → empty, no message; unreadable → empty + message; limit 0 → empty, file not read.
    pub fn load(path: &Path, limit: usize, now: u64) -> (History, Option<String>) {
        if limit == 0 {
            return (History::empty(0), None);
        }
        let mut bytes = Vec::new();
        match File::open(path).and_then(|f| f.take(READ_MAX).read_to_end(&mut bytes)) {
            Ok(_) => {
                if bytes.len() as u64 == READ_MAX {
                    // Cut at the limit: the last line may be partial, i.e. a shorter id that looks valid.
                    bytes.truncate(bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1));
                }
                (History::parse(&String::from_utf8_lossy(&bytes), limit, now), None)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (History::empty(limit), None),
            Err(e) => (History::empty(limit), Some(format!("launcher history unreadable ({}): {e}", path.display()))),
        }
    }

    /// count += 1 (saturating, capped at 10_000), last = now; new id evicts the lowest frecency when full;
    /// no-op when limit is 0 (and for an id that would break the file).
    pub fn record(&mut self, id: &str, now: u64) {
        if self.limit == 0 || !valid_id(id) {
            return;
        }
        if let Some(u) = self.entries.iter_mut().find(|u| u.id == id) {
            u.count = u.count.saturating_add(1).min(COUNT_MAX);
            u.last = now;
            return;
        }
        while self.entries.len() >= self.limit {
            let lowest = (0..self.entries.len()).min_by_key(|&i| (frecency_of(&self.entries[i], now), self.entries[i].last));
            match lowest {
                Some(i) => self.entries.remove(i),
                None => break,
            };
        }
        self.entries.push(Use { id: id.to_string(), count: 1, last: now });
    }

    pub fn frecency(&self, id: &str, now: u64) -> u32 {
        self.entries.iter().find(|u| u.id == id).map_or(0, |u| frecency_of(u, now))
    }

    pub fn entries(&self) -> &[Use] {
        &self.entries
    }

    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for u in &self.entries {
            s.push_str(&format!("{}\t{}\t{}\n", u.last, u.count, u.id));
        }
        s
    }

    /// Atomic: write "<path>.tmp-<pid>" in the same directory, rename; directory created 0700;
    /// no fsync (it runs on the event loop; a crash may lose the last launches, never the file);
    /// stale "<name>.tmp-<pid>" files of dead processes removed. limit 0 → Ok(()) without touching the disk.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.limit == 0 {
            return Ok(());
        }
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let name = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "history path without a file name"))?;
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let tmp = dir.join(format!("{}.tmp-{}", name.to_string_lossy(), std::process::id()));
        let written = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| {
            f.write_all(self.to_text().as_bytes())?;
            fs::rename(&tmp, path)
        });
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written?;
        remove_stale_temporaries(dir, &name.to_string_lossy());
        Ok(())
    }
}

/// "<name>.tmp-<pid>" files left by a process that died between its write
/// and its rename (only ours and those of running processes are kept).
fn remove_stale_temporaries(dir: &Path, name: &str) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let prefix = format!("{name}.tmp-");
    for e in entries.flatten() {
        let file = e.file_name();
        let Some(pid) = file.to_str().and_then(|f| f.strip_prefix(prefix.as_str())) else { continue };
        let dead = pid.parse::<u32>().is_ok_and(|p| p != std::process::id() && !Path::new(&format!("/proc/{p}")).exists());
        if dead {
            let _ = fs::remove_file(e.path());
        }
    }
}

pub fn app_key(desktop_id: &str) -> String {
    format!("app:{desktop_id}")
}

pub fn command_key(name: &str) -> String {
    format!("cmd:{name}")
}

pub fn history_path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::testing::TempDir;
    use std::os::unix::fs::PermissionsExt;

    const NOW: u64 = 1_800_000_000;
    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;

    /// An empty temporary dir, removed when the returned guard drops.
    fn dir(name: &str) -> TempDir {
        TempDir::new(&format!("history-{name}"))
    }

    fn running_as_root() -> bool {
        // SAFETY: geteuid(2) cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    fn ids(h: &History) -> Vec<&str> {
        h.entries().iter().map(|u| u.id.as_str()).collect()
    }

    #[test]
    fn weight_buckets() {
        let ages = [0, HOUR - 1, HOUR, DAY - 1, DAY, 7 * DAY - 1, 7 * DAY, 30 * DAY - 1, 30 * DAY, 400 * DAY];
        assert_eq!(ages.map(weight), [100, 100, 70, 70, 50, 50, 30, 30, 10, 10]);
    }

    #[test]
    fn frecency_weighs_count_by_recency() {
        let text = format!(
            "{}\t3\tapp:recent.desktop\n{}\t3\tapp:earlier.desktop\n{}\t20\tapp:old-favourite.desktop\n{}\t2000\tcmd:busy\n",
            NOW - 60,
            NOW - 2 * HOUR,
            NOW - 60 * DAY,
            NOW - 60
        );
        let h = History::parse(&text, 10, NOW);
        assert_eq!(h.frecency("app:recent.desktop", NOW), 300);
        assert_eq!(h.frecency("app:earlier.desktop", NOW), 210);
        assert_eq!(h.frecency("app:old-favourite.desktop", NOW), 200);
        assert_eq!(h.frecency("cmd:busy", NOW), 100_000, "count counted up to 1000");
        assert_eq!(h.frecency("app:never.desktop", NOW), 0);
        let future = History::parse(&format!("{}\t1\tapp:future.desktop\n", NOW + DAY), 10, NOW);
        assert_eq!(future.frecency("app:future.desktop", NOW), 100, "a clock moved back counts as just now");
    }

    #[test]
    fn parse_and_to_text_round_trip() {
        let text = format!("{}\t4\tapp:org.example.Browser.desktop\n{}\t1\tcmd:htop\n", NOW - 10, NOW - DAY);
        let h = History::parse(&text, 200, NOW);
        assert_eq!(h.to_text(), text);
        assert_eq!(
            h.entries(),
            [
                Use { id: "app:org.example.Browser.desktop".into(), count: 4, last: NOW - 10 },
                Use { id: "cmd:htop".into(), count: 1, last: NOW - DAY }
            ]
        );
    }

    #[test]
    fn corrupted_lines_are_skipped() {
        let text = "garbage\n12\t3\n12\tx\tapp:a\nx\t3\tapp:b\n12\t0\tapp:zero\n12\t3\t\n12\t3\tapp:tab\tinside\n-5\t1\tapp:neg\n12\t3\tapp:ok\n13\t9\tapp:ok\n\u{FFFD}\u{FFFD}\n";
        let h = History::parse(text, 200, NOW);
        assert_eq!(h.entries(), [Use { id: "app:ok".into(), count: 3, last: 12 }], "the first line of a repeated id is kept");
    }

    #[test]
    fn over_the_limit_the_lowest_frecency_goes() {
        let text = format!("{}\t1\tapp:low\n{}\t5\tapp:high\n{}\t2\tapp:mid\n", NOW - 90 * DAY, NOW, NOW);
        assert_eq!(ids(&History::parse(&text, 2, NOW)), ["app:high", "app:mid"]);
    }

    #[test]
    fn record_counts_and_refreshes() {
        let mut h = History::empty(10);
        h.record("app:a.desktop", NOW - DAY);
        h.record("app:a.desktop", NOW);
        assert_eq!(h.entries(), [Use { id: "app:a.desktop".into(), count: 2, last: NOW }]);
        let mut h = History::parse(&format!("{NOW}\t10000\tcmd:busy\n"), 10, NOW);
        h.record("cmd:busy", NOW);
        assert_eq!(h.entries()[0].count, 10_000, "capped");
    }

    #[test]
    fn a_new_id_evicts_the_lowest_frecency_when_full() {
        // The stale (lowest-frecency) entry comes second in the file, not first: a FIFO
        // ("drop the oldest slot") eviction would wrongly keep it and drop "app:fresh" instead.
        let mut h = History::parse(&format!("{}\t3\tapp:fresh\n{}\t1\tapp:stale\n", NOW, NOW - 90 * DAY), 2, NOW);
        h.record("cmd:new", NOW);
        assert_eq!(ids(&h), ["app:fresh", "cmd:new"], "the lowest-frecency entry goes, not the first one in the file");
    }

    #[test]
    fn a_tied_frecency_falls_back_to_the_older_last() {
        // Both entries score the same frecency (count 1, age under an hour: weight 100); only
        // `last` tells them apart, and the older one is the one that must go.
        let mut h = History::parse(&format!("{}\t1\tapp:newer\n{}\t1\tapp:older\n", NOW, NOW - 10), 2, NOW);
        h.record("cmd:new", NOW);
        assert_eq!(ids(&h), ["app:newer", "cmd:new"], "tied frecency: the older last loses the tie-break");
    }

    #[test]
    fn ids_that_would_break_the_file_are_refused() {
        let mut h = History::empty(10);
        let long = "x".repeat(ID_MAX + 1);
        for bad in ["", "app:a\tb", "app:a\nb", "app:a\rb", long.as_str()] {
            h.record(bad, NOW);
        }
        assert!(h.entries().is_empty(), "{:?}", h.entries());
    }

    #[test]
    fn an_id_with_the_replacement_character_is_refused() {
        let d = dir("replacement");
        // \xe9 alone is not valid UTF-8; from_utf8_lossy turns it into a single U+FFFD.
        fs::write(history_path(&d), b"5\t1\tapp:caf\xe9.desktop\n").unwrap();
        let (h, w) = History::load(&history_path(&d), 200, NOW);
        assert_eq!((ids(&h), w), (vec![], None), "the id decoded with a replacement character is skipped, not kept lossily");
    }

    #[test]
    fn limit_zero_never_reads_nor_writes() {
        let d = dir("zero");
        fs::create_dir_all(history_path(&d)).unwrap(); // reading this "file" would fail loudly
        let (mut h, w) = History::load(&history_path(&d), 0, NOW);
        assert_eq!((h.entries().len(), w), (0, None));
        h.record("app:a.desktop", NOW);
        assert!(h.entries().is_empty());
        let kept = dir("zero-kept");
        fs::write(history_path(&kept), "1\t1\tapp:kept\n").unwrap();
        h.save(&history_path(&kept)).unwrap();
        assert_eq!(fs::read_to_string(history_path(&kept)).unwrap(), "1\t1\tapp:kept\n", "left untouched");
        let none = dir("zero-none");
        History::empty(0).save(&history_path(&none.join("alpheus"))).unwrap();
        assert!(!none.join("alpheus").exists(), "no directory created");
    }

    #[test]
    fn load_absent_is_empty_and_unreadable_says_so() {
        let d = dir("load");
        assert_eq!(History::load(&history_path(&d), 200, NOW), (History::empty(200), None));
        fs::create_dir_all(history_path(&d)).unwrap();
        let (h, w) = History::load(&history_path(&d), 200, NOW);
        assert_eq!(h, History::empty(200));
        let w = w.unwrap();
        assert!(w.contains("launcher history unreadable"), "{w}");
        let ok = dir("load-ok");
        fs::write(history_path(&ok), b"5\t2\tapp:a\n\xff\xfe\n").unwrap();
        let (h, w) = History::load(&history_path(&ok), 200, NOW);
        assert_eq!((ids(&h), w), (vec!["app:a"], None), "invalid UTF-8 lines skipped, not fatal");
    }

    #[test]
    fn save_is_atomic_and_creates_a_private_directory() {
        let d = dir("save");
        let p = history_path(&d.join("alpheus"));
        let mut h = History::empty(200);
        h.record(&app_key("org.example.Browser.desktop"), NOW);
        h.save(&p).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), h.to_text());
        assert_eq!(fs::metadata(d.join("alpheus")).unwrap().permissions().mode() & 0o777, 0o700);
        h.record(&command_key("htop"), NOW);
        h.save(&p).unwrap();
        assert_eq!(History::load(&p, 200, NOW).0, h);
        let left: Vec<_> = fs::read_dir(d.join("alpheus")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from(FILE_NAME)], "no temporary file left");
    }

    #[test]
    fn a_failed_save_keeps_the_original() {
        if running_as_root() {
            return; // root ignores directory permissions
        }
        let d = dir("readonly");
        let p = history_path(&d);
        fs::write(&p, "1\t1\tapp:original\n").unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o500)).unwrap();
        let mut h = History::empty(200);
        h.record("app:new", NOW);
        let result = h.save(&p);
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err(), "the temporary file cannot be created in a read-only directory");
        assert_eq!(fs::read_to_string(&p).unwrap(), "1\t1\tapp:original\n");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no temporary file left");
    }

    #[test]
    fn the_file_holds_only_ids_and_numbers() {
        let d = dir("content");
        let p = history_path(&d);
        let mut h = History::empty(200);
        h.record(&app_key("org.example.Browser.desktop"), NOW);
        h.record(&command_key("htop"), NOW - 5);
        h.save(&p).unwrap();
        for line in fs::read_to_string(&p).unwrap().lines() {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 3, "{line:?}");
            assert!(f[..2].iter().all(|n| n.bytes().all(|b| b.is_ascii_digit())), "{line:?}");
            assert!(f[2].starts_with("app:") || f[2].starts_with("cmd:"), "{line:?}");
        }
    }

    #[test]
    fn the_largest_history_fits_the_read_limit() {
        let mut h = History::empty(HISTORY_MAX);
        for i in 0..HISTORY_MAX {
            h.record(&format!("{i:0>width$}", width = ID_MAX), u64::MAX);
        }
        for u in &mut h.entries {
            u.count = COUNT_MAX;
        }
        assert_eq!(h.entries().len(), HISTORY_MAX);
        assert!(h.to_text().len() as u64 <= READ_MAX, "{} bytes", h.to_text().len());
    }

    #[test]
    fn a_file_cut_at_the_read_limit_loses_its_partial_last_line() {
        let d = dir("cut");
        let (head, tail) = ("5\t2\tapp:a\n", "7\t1\tapp:bc\n");
        // Padding (one unparseable line) so that the limit falls right after "app:b".
        let padding = READ_MAX as usize - head.len() - (tail.len() - 2);
        let text = format!("{head}{}\n{tail}", "#".repeat(padding - 1));
        assert!(text.as_bytes()[..READ_MAX as usize].ends_with(b"\tapp:b"));
        fs::write(history_path(&d), &text).unwrap();
        let (h, w) = History::load(&history_path(&d), 200, NOW);
        assert_eq!((ids(&h), w), (vec!["app:a"], None), "the cut line is not read as app:b");
    }

    #[test]
    fn a_save_removes_temporary_files_left_by_dead_processes() {
        let d = dir("stale");
        // pid_max is at most 2^22, so pid 999999999 never runs; pid 1 always does.
        for f in ["launcher-history.tmp-999999999", "launcher-history.tmp-1", "launcher-history.tmp-x", "notes.tmp-999999999"] {
            fs::write(d.join(f), "").unwrap();
        }
        let mut h = History::empty(200);
        h.record("app:a", NOW);
        h.save(&history_path(&d)).unwrap();
        let mut left: Vec<String> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["launcher-history", "launcher-history.tmp-1", "launcher-history.tmp-x", "notes.tmp-999999999"]);
    }

    #[test]
    fn keys_and_path() {
        assert_eq!(app_key("org.example.Browser.desktop"), "app:org.example.Browser.desktop");
        assert_eq!(command_key("htop"), "cmd:htop");
        assert_eq!(history_path(Path::new("/s/alpheus")), Path::new("/s/alpheus/launcher-history"));
    }
}
