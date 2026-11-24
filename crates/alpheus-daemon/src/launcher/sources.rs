//! The launcher's command sources: a `[[sources]]` table's `list` read once per open on the scan thread
//! (`Executor::capture`, 1 s, 2,000 lines, 256 KiB), its lines cleaned and split into an entry and its path; `run` built with
//! the chosen line as one argument. Nothing here runs between opens, nothing through a shell, and
//! no line ever reaches the log.
//!
//! See: docs/features/launcher.md and docs/decisions.md ("The launcher reads its command sources after the
//! apps, on the same thread", "A command source is bounded and cannot stall the launcher").

use crate::launcher::config::{LINE_MAX, LINE_PLACEHOLDER, SourceConfig};
use crate::text::invisible;
use alpheus_common::exec::{CaptureEnd, CaptureLimits, ExecError, Executor};
use std::collections::HashSet;
use std::time::Duration;

/// A source's `list` gets this long in all, from its spawn.
pub const LIST_DEADLINE: Duration = Duration::from_secs(1);
/// Lines read at most per source.
pub const MAX_SOURCE_LINES: usize = 2_000;
/// Bytes read at most per source.
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// Between the path and the entry: space, U+00B7, space.
pub const SEPARATOR: &str = " \u{b7} ";
/// Between two path segments, as the row shows them.
pub const PATH_JOIN: &str = " \u{203a} ";
pub const LIMITS: CaptureLimits = CaptureLimits { deadline: LIST_DEADLINE, max_bytes: MAX_SOURCE_BYTES, max_lines: MAX_SOURCE_LINES };

/// One line of a source, ready to rank and run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// Its source's index in `Entries::sources`.
    pub source: usize,
    /// The whole line, cleaned: what `run` receives, and the history key's tail.
    pub line: String,
    /// The last segment: what the row shows first, what typing matches first.
    pub entry: String,
    /// The segments before it, joined with ` › `, shown dimmed; None without a separator.
    pub path: Option<String>,
}

impl SourceEntry {
    pub fn new(source: usize, line: &str) -> SourceEntry {
        let (path, entry) = split(line);
        SourceEntry { source, line: line.to_string(), entry: entry.to_string(), path }
    }
}

/// What one open's sources gave: the sources as configured at that open (an entry's `source`
/// indexes them: their `run` and name), then every entry in launcher.toml's order, then the
/// source's own.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entries {
    pub sources: Vec<SourceConfig>,
    pub items: Vec<SourceEntry>,
}

/// Control characters (tab and `\r` included) and the invisible formatting characters the
/// notifications remove (`text::invisible`: bidi overrides, zero-width space) removed, the line and
/// paragraph separators turned into a space. Not
/// `text::clean`: it turns a tab into a space and cuts at 256 characters, and the line is what
/// `run` receives.
pub fn clean(text: &str) -> String {
    text.chars().filter(|&c| !c.is_control() && !invisible(c)).map(|c| if matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c }).collect()
}

/// (path, entry): the last ` · ` segment is the entry, the ones before it the path, joined with
/// ` › `. Nothing after the last separator: no path, the whole line is the entry. Blank segments
/// of the path are dropped; none left: no path.
pub fn split(line: &str) -> (Option<String>, &str) {
    let Some((path, entry)) = line.rsplit_once(SEPARATOR) else { return (None, line) };
    if entry.trim().is_empty() {
        return (None, line);
    }
    let segments: Vec<&str> = path.split(SEPARATOR).filter(|s| !s.trim().is_empty()).collect();
    if segments.is_empty() {
        return (None, entry);
    }
    (Some(segments.join(PATH_JOIN)), entry)
}

/// A source's output → its lines, in order: at most MAX_SOURCE_LINES read; each decoded as UTF-8
/// (an invalid one skipped) and cleaned (a trailing `\r` goes with the control characters); blank
/// ones and those over LINE_MAX bytes skipped; a repeated line keeps its first place. `cut`: the
/// read was stopped (a kill), so the text after the last newline may be half a line and is dropped.
pub fn lines(bytes: &[u8], cut: bool) -> Vec<String> {
    let mut raw: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    let last = raw.pop().filter(|l| !l.is_empty());
    if let (Some(l), false) = (last, cut) {
        raw.push(l);
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in raw.into_iter().take(MAX_SOURCE_LINES) {
        let Ok(text) = std::str::from_utf8(r) else { continue };
        let line = clean(text);
        if line.trim().is_empty() || line.len() > LINE_MAX || !seen.insert(line.clone()) {
            continue;
        }
        out.push(line);
    }
    out
}

/// `run` with its one `{line}` element replaced by `line`, whole: element by element, never a
/// substitution inside an argument (launcher.toml refuses one), never a shell.
pub fn run_argv(run: &[String], line: &str) -> Vec<String> {
    run.iter().map(|a| if a == LINE_PLACEHOLDER { line.to_string() } else { a.clone() }).collect()
}

fn refusal(e: &ExecError) -> String {
    match e {
        ExecError::NotFound => "program not found".to_string(),
        ExecError::Timeout => format!("timed out after {} s", LIST_DEADLINE.as_secs()),
        ExecError::Io(_) => "could not start".to_string(),
    }
}

/// `argv`'s program run for its end only (the Tools menu's `state`): its exit status and the caps,
/// the output dropped at once. Never through a shell. `.capture(` lives here and in `nonblocking.rs`
/// only (the guardrail), so the Tools reader calls these.
pub fn capture_status(argv: &[String], exec: &dyn Executor) -> Result<CaptureEnd, ExecError> {
    let Some((program, args)) = argv.split_first() else { return Err(ExecError::NotFound) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec.capture(program, &args, LIMITS).map(|c| c.end)
}

/// `argv`'s program run for its lines (the Tools menu's `choices_cmd`): cleaned as a source's are
/// (`lines`), the ones read before a kill or a cap kept, and how the read ended.
pub fn capture_lines(argv: &[String], exec: &dyn Executor) -> Result<(Vec<String>, CaptureEnd), ExecError> {
    let Some((program, args)) = argv.split_first() else { return Err(ExecError::NotFound) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let captured = exec.capture(program, &args, LIMITS)?;
    let kept = lines(&captured.stdout, !matches!(captured.end, CaptureEnd::Exited(_)));
    Ok((kept, captured.end))
}

/// One source's lines (`lines`), and one `log` line when it failed: could not start, exited
/// non-zero, was killed, timed out or passed a cap — with its name and how many lines were kept,
/// never a line.
pub fn read_source(source: &SourceConfig, exec: &dyn Executor, log: &mut dyn FnMut(&str)) -> Vec<String> {
    let Some((program, args)) = source.list.split_first() else { return Vec::new() };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let captured = match exec.capture(program, &args, LIMITS) {
        Ok(c) => c,
        Err(e) => {
            log(&format!("launcher source {:?}: {}", source.name, refusal(&e)));
            return Vec::new();
        }
    };
    let kept = lines(&captured.stdout, !matches!(captured.end, CaptureEnd::Exited(_)));
    let why = match captured.end {
        CaptureEnd::Exited(Some(0)) => None,
        CaptureEnd::Exited(Some(code)) => Some(format!("exited with code {code}")),
        CaptureEnd::Exited(None) => Some("killed by a signal".to_string()),
        CaptureEnd::TimedOut => Some(format!("timed out after {} s", LIST_DEADLINE.as_secs())),
        CaptureEnd::Capped => Some(format!("output cap reached ({MAX_SOURCE_LINES} lines or {} KiB)", MAX_SOURCE_BYTES / 1024)),
    };
    if let Some(why) = why {
        log(&format!("launcher source {:?}: {why}, {} lines kept", source.name, kept.len()));
    }
    kept
}

/// Every source, one after another in launcher.toml's order (at most 8 × 1 s).
pub fn read_all(sources: &[SourceConfig], exec: &dyn Executor, log: &mut dyn FnMut(&str)) -> Entries {
    let mut items = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        items.extend(read_source(s, exec, log).iter().map(|line| SourceEntry::new(i, line)));
    }
    Entries { sources: sources.to_vec(), items }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::testing::TempDir;
    use alpheus_common::exec::fake::FakeExecutor;
    use alpheus_common::exec::{Captured, SystemExecutor};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    #[test]
    fn clean_keeps_a_flag_and_turns_separators_into_spaces() {
        assert_eq!(clean("a\u{2028}b\u{2029}c\td"), "a b cd", "the tab is dropped as before");
        let flag = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}";
        assert_eq!(clean(flag), flag);
    }

    /// An `sh` script in `dir`: a fake source's `list` (never tools-menu).
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        p
    }

    /// The script runs as `sh <script>`, read rather than executed: a file just written can't be
    /// exec'd while another test's fork still holds its write descriptor ("Text file busy").
    fn source(name: &str, list: &Path) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            list: vec!["sh".into(), list.display().to_string()],
            run: vec!["fake-run".into(), "{line}".into()],
        }
    }

    /// The lines a real `sh` script gives, and what was logged.
    fn read(s: &SourceConfig) -> (Vec<String>, Vec<String>) {
        let mut logged = Vec::new();
        let lines = read_source(s, &SystemExecutor::new(Duration::from_secs(5)), &mut |m| logged.push(m.to_string()));
        (lines, logged)
    }

    fn alive(pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|st| st.rsplit_once(')').and_then(|(_, r)| r.trim_start().chars().next()) != Some('Z'))
    }

    fn pid_in(f: &Path) -> i32 {
        std::fs::read_to_string(f).unwrap().trim().parse().unwrap()
    }

    #[test]
    fn split_gives_the_entry_and_its_path() {
        assert_eq!(split("Captures · Screenshot zone"), (Some("Captures".to_string()), "Screenshot zone"));
        assert_eq!(split("Apparence · Schéma couleurs · Vibrant"), (Some("Apparence › Schéma couleurs".to_string()), "Vibrant"));
        assert_eq!(split("Verrouiller"), (None, "Verrouiller"));
        assert_eq!(split("Wi-Fi·Off"), (None, "Wi-Fi·Off"), "a middle dot without its spaces is text");
    }

    #[test]
    fn split_handles_odd_separators() {
        assert_eq!(split("Captures · "), (None, "Captures · "), "nothing after the last separator: the whole line");
        assert_eq!(split(" · Zone"), (None, "Zone"), "an empty path is no path");
        assert_eq!(split("A ·  · B"), (Some("A".to_string()), "B"), "a blank segment is dropped");
    }

    #[test]
    fn lines_drop_a_cut_fragment_but_keep_a_last_line() {
        assert_eq!(lines(b"a\nb\npart", true), ["a", "b"], "cut off: the fragment may be half a line");
        assert_eq!(lines(b"a\nb\nlast", false), ["a", "b", "last"], "ended by itself: a last line without a newline");
    }

    #[test]
    fn a_source_s_lines_are_cleaned_and_deduplicated_in_order() {
        let d = TempDir::new("sources-clean");
        let body = r#"printf 'Captures \302\267 Zone\r\n'
printf 'Verrouiller\n\n   \n'
printf '\342\200\256Inverse\n'
printf 'Tab\there\001\n'
printf 'Verrouiller\n'
printf '\377bad\n'
printf '%0600d\n' 0
printf 'Derni\303\250re'"#;
        let (lines, logged) = read(&source("Outils", &script(&d, "fake-list", body)));
        assert_eq!(lines, ["Captures · Zone", "Verrouiller", "Inverse", "Tabhere", "Dernière"]);
        assert!(logged.is_empty(), "exit 0: nothing to say: {logged:?}");
    }

    #[test]
    fn a_non_zero_exit_keeps_its_lines_and_is_logged_once_without_them() {
        let d = TempDir::new("sources-exit");
        let list = script(&d, "fake-list", "echo 'Captures · Zone'\necho bruit >&2\nexit 4");
        let (lines, logged) = read(&source("Outils", &list));
        assert_eq!(lines, ["Captures · Zone"], "stderr ignored, the lines kept");
        assert_eq!(logged, ["launcher source \"Outils\": exited with code 4, 1 lines kept"]);
    }

    #[test]
    fn a_hang_is_killed_at_one_second_and_its_lines_so_far_are_kept() {
        let d = TempDir::new("sources-hang");
        let pid_file = d.join("child.pid");
        let body = format!("sleep 30 &\necho $! > '{}'\necho 'Lent · Une'\necho 'Lent · Deux'\nwait", pid_file.display());
        let t = Instant::now();
        let (lines, logged) = read(&source("Lent", &script(&d, "fake-list", &body)));
        let took = t.elapsed();
        assert!(took >= LIST_DEADLINE && took < Duration::from_millis(1800), "{took:?}");
        assert_eq!(lines, ["Lent · Une", "Lent · Deux"]);
        assert_eq!(logged, ["launcher source \"Lent\": timed out after 1 s, 2 lines kept"]);
        let pid = pid_in(&pid_file);
        let t = Instant::now();
        while alive(pid) {
            assert!(t.elapsed() < Duration::from_secs(2), "the source's child {pid} survived its kill");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_flood_stops_at_the_line_cap() {
        let d = TempDir::new("sources-flood");
        let list = script(&d, "fake-list", "i=0\nwhile :; do i=$((i+1)); echo \"Entrée $i\"; done");
        let (lines, logged) = read(&source("Déluge", &list));
        assert_eq!(lines.len(), MAX_SOURCE_LINES);
        assert_eq!((lines[0].as_str(), lines[MAX_SOURCE_LINES - 1].as_str()), ("Entrée 1", "Entrée 2000"));
        assert_eq!(logged, ["launcher source \"Déluge\": output cap reached (2000 lines or 256 KiB), 2000 lines kept"]);
    }

    #[test]
    fn a_flood_of_long_lines_stops_at_the_byte_cap() {
        let d = TempDir::new("sources-bytes");
        let list = script(&d, "fake-list", "i=0\nwhile :; do i=$((i+1)); printf '%0399d\\n' \"$i\"; done");
        let (lines, logged) = read(&source("Gros", &list));
        assert_eq!(lines.len(), MAX_SOURCE_BYTES / 400, "whole 400-byte lines within 256 KiB; the cut one dropped");
        assert!(lines.iter().all(|l| l.len() == 399));
        assert_eq!(logged.len(), 1);
        assert!(logged[0].contains("output cap reached"), "{logged:?}");
    }

    #[test]
    fn a_missing_program_is_logged_and_gives_nothing() {
        let absent = SourceConfig {
            name: "Absent".into(),
            list: vec!["/nonexistent/alpheus-fake-list".into()],
            run: vec!["fake-run".into(), "{line}".into()],
        };
        let (lines, logged) = read(&absent);
        assert!(lines.is_empty());
        assert_eq!(logged, ["launcher source \"Absent\": program not found"]);
    }

    #[test]
    fn read_all_numbers_entries_by_source_and_goes_on_after_a_failure() {
        let fake = FakeExecutor::new();
        let out = "Captures · Zone\nVerrouiller\n".as_bytes().to_vec();
        fake.capture_ok("fake-list", Captured { stdout: out, end: CaptureEnd::Exited(Some(0)) });
        let ok = SourceConfig { name: "Outils".into(), list: vec!["fake-list".into()], run: vec!["fake-run".into(), "{line}".into()] };
        let gone = SourceConfig { name: "Absent".into(), list: vec!["absent-list".into()], run: vec!["fake-run".into(), "{line}".into()] };
        let mut logged = Vec::new();
        let e = read_all(&[gone.clone(), ok.clone()], &fake, &mut |m| logged.push(m.to_string()));
        assert_eq!(e.sources, [gone, ok]);
        assert_eq!(e.items, [SourceEntry::new(1, "Captures · Zone"), SourceEntry::new(1, "Verrouiller")]);
        assert_eq!(e.items[0].path.as_deref(), Some("Captures"));
        assert_eq!(logged, ["launcher source \"Absent\": program not found"]);
        assert_eq!(fake.calls(), ["absent-list", "fake-list"], "in launcher.toml's order, one after another");
    }

    #[test]
    fn run_argv_puts_the_whole_line_in_one_argument() {
        let run: Vec<String> = ["tools-menu", "--run", "{line}", "--then"].map(String::from).to_vec();
        for line in ["Captures · Screenshot zone", "it's \"quoted\" $HOME `x`", "{line}", "-rf", "a  b"] {
            assert_eq!(run_argv(&run, line), ["tools-menu", "--run", line, "--then"], "{line:?}");
        }
        // Only the element that is exactly `{line}` is replaced: one that merely contains it passes
        // through unchanged (launcher.toml refuses it anyway; this keeps run_argv honest on its own).
        let run: Vec<String> = ["tools-menu", "--line={line}", "{line}"].map(String::from).to_vec();
        assert_eq!(run_argv(&run, "Verrouiller"), ["tools-menu", "--line={line}", "Verrouiller"]);
    }
}
