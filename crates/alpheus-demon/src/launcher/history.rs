//! The launcher's history, for frecency ranking: which entries were
//! launched, how often, how recently. It holds entry ids ("app:<desktop
//! id>", "cmd:<name>", "source:<name>:<line>") and numbers only, never typed text — the API takes
//! ids. File `$XDG_STATE_HOME/alpheus/launcher-history`, one line per entry
//! "<last>\t<count>\t<id>", bounded, written atomically.

use super::config::{HISTORY_MAX, LINE_MAX, NAME_MAX};
use super::tools::config::{CHOICE_MAX, TOOL_NAME_MAX};
use std::collections::{HashMap, HashSet};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "launcher-history";
const COUNT_MAX: u32 = 10_000;
/// A source entry's id starts so (sources spec § 3).
const SOURCE_PREFIX: &str = "source:";
/// A tool's id starts so (tools spec § 5): "tool:builtin:<id>" or "tool:user:<category>/<name>[/<choice>]".
const TOOL_PREFIX: &str = "tool:";
/// A user entry's id starts so: "tool:user:<category>/<name>[/<choice>]".
const TOOL_USER_PREFIX: &str = "tool:user:";
/// The longest source key: "source:" + a name of NAME_MAX four-byte characters + ':' + a line of
/// LINE_MAX bytes (648).
const SOURCE_ID_MAX: usize = SOURCE_PREFIX.len() + 4 * NAME_MAX + 1 + LINE_MAX;
/// The longest tool key: "tool:user:" + a category and an entry name of TOOL_NAME_MAX four-byte
/// characters each, + a fixed choice of CHOICE_MAX four-byte characters, two '/' between.
const TOOL_ID_MAX: usize = TOOL_USER_PREFIX.len() + 4 * TOOL_NAME_MAX + 1 + 4 * TOOL_NAME_MAX + 1 + 4 * CHOICE_MAX;
/// Longer ids are refused (and skipped when read): the longer of the two forms above.
pub(crate) const ID_MAX: usize = if SOURCE_ID_MAX > TOOL_ID_MAX { SOURCE_ID_MAX } else { TOOL_ID_MAX };
/// What `load` reads at most: the largest valid file, HISTORY_MAX lines of
/// "<last: 20 digits>\t<count: 5 digits>\t<id>\n" (≈ 660 KiB; 200 typical entries ≈ 12 KiB).
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

    /// Frecency of every entry, split by its "app:"/"cmd:" prefix and keyed by the id with
    /// that prefix stripped (borrowed from the entry: no `format!`). Built once from the
    /// at most `limit` entries (≤ 200 in practice), so a catalog of thousands of apps and
    /// commands looks each one up in a `HashMap` instead of formatting a key and scanning
    /// the history for every one of them.
    pub fn frecency_maps(&self, now: u64) -> (HashMap<&str, u32>, HashMap<&str, u32>) {
        let mut apps = HashMap::new();
        let mut commands = HashMap::new();
        for u in &self.entries {
            let f = frecency_of(u, now);
            if let Some(id) = u.id.strip_prefix("app:") {
                apps.insert(id, f);
            } else if let Some(name) = u.id.strip_prefix("cmd:") {
                commands.insert(name, f);
            }
        }
        (apps, commands)
    }

    /// Frecency of every "source:" entry, keyed by the id with that prefix stripped
    /// (`source_id`: "<name>:<line>"), borrowed from the entry. Built once per catalog of
    /// entries, only when there are entries to rank (matching.rs).
    pub fn source_frecency(&self, now: u64) -> HashMap<&str, u32> {
        self.entries.iter().filter_map(|u| u.id.strip_prefix(SOURCE_PREFIX).map(|id| (id, frecency_of(u, now)))).collect()
    }

    /// Frecency of every "tool:" entry, keyed by its whole id (`tool:builtin:<id>`,
    /// `tool:user:<category>/<name>[/<choice>]`), borrowed from the entry. Built once per
    /// catalog of tool rows, only when there are rows to rank (matching.rs).
    pub fn tool_frecency(&self, now: u64) -> HashMap<&str, u32> {
        self.entries.iter().filter(|u| u.id.starts_with(TOOL_PREFIX)).map(|u| (u.id.as_str(), frecency_of(u, now))).collect()
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

/// A source entry's id without its prefix: what `History::source_frecency` keys by.
pub fn source_id(name: &str, line: &str) -> String {
    format!("{name}:{line}")
}

/// A source entry's history id (sources spec § 3): its source's name and its whole line.
pub fn source_key(name: &str, line: &str) -> String {
    format!("{SOURCE_PREFIX}{}", source_id(name, line))
}

pub fn history_path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

#[cfg(test)]
mod tests;
