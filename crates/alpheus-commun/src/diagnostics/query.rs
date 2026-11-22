//! A journal read as a typed query: the one place that turns it into `journalctl` arguments, so
//! nothing the user typed or a tool printed ever becomes an option (a unit is checked against
//! systemd's name characters and attached as `--unit=<name>`, a cursor against printable ASCII).

use super::names::{valid_cursor, valid_unit};

/// Which manager a unit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    System,
    User,
}

/// "This level and more severe" (`-p <level>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Any,
    Err,
    Warning,
    Notice,
    Info,
}

impl Priority {
    /// The cycle of the `p` key: any, err, warning, notice, info, any.
    pub fn next(self) -> Priority {
        match self {
            Priority::Any => Priority::Err,
            Priority::Err => Priority::Warning,
            Priority::Warning => Priority::Notice,
            Priority::Notice => Priority::Info,
            Priority::Info => Priority::Any,
        }
    }

    fn word(self) -> Option<&'static str> {
        match self {
            Priority::Any => None,
            Priority::Err => Some("err"),
            Priority::Warning => Some("warning"),
            Priority::Notice => Some("notice"),
            Priority::Info => Some("info"),
        }
    }
}

/// The tools menu's eight filters, named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    ErrorsThisBoot,
    Usb,
    SuspendResume,
    OutOfMemory,
    Thermal,
    NetworkManager,
    Live,
    ServiceLogs,
}

impl Preset {
    pub const ALL: [Preset; 8] = [
        Preset::ErrorsThisBoot,
        Preset::Usb,
        Preset::SuspendResume,
        Preset::OutOfMemory,
        Preset::Thermal,
        Preset::NetworkManager,
        Preset::Live,
        Preset::ServiceLogs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::ErrorsThisBoot => "Errors this boot",
            Preset::Usb => "USB",
            Preset::SuspendResume => "Suspend and resume",
            Preset::OutOfMemory => "Out of memory",
            Preset::Thermal => "Thermal and throttling",
            Preset::NetworkManager => "NetworkManager",
            Preset::Live => "Live journal",
            Preset::ServiceLogs => "Logs of a service…",
        }
    }

    /// Follow starts on for these two.
    pub fn follows(self) -> bool {
        matches!(self, Preset::Live | Preset::Usb)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalQuery {
    pub preset: Preset,
    pub unit: Option<(Scope, String)>,
    pub priority: Priority,
    /// `--since=@<s> --until=@<u>`, in seconds since the epoch.
    pub since_until: Option<(u64, u64)>,
    /// Entries asked for (clamped to 1..=5,000).
    pub lines: u32,
    /// Follow reads: everything after this cursor, no line count.
    pub after_cursor: Option<String>,
    /// The bug report's read: only the three fields it prints (never a command line, an
    /// environment or an id), not the view's seven.
    pub report_fields: bool,
}

impl JournalQuery {
    /// The preset's own starting point: its unit and its priority.
    pub fn new(preset: Preset, lines: u32) -> Self {
        JournalQuery {
            preset,
            unit: (preset == Preset::NetworkManager).then(|| (Scope::System, "NetworkManager.service".to_string())),
            priority: if preset == Preset::ErrorsThisBoot { Priority::Err } else { Priority::Any },
            since_until: None,
            lines,
            after_cursor: None,
            report_fields: false,
        }
    }

    /// This boot's errors, read for the bug report.
    pub fn report_errors(lines: u32) -> Self {
        JournalQuery { report_fields: true, ..JournalQuery::new(Preset::ErrorsThisBoot, lines) }
    }
}

/// The bug report's reads start with these: three fields, no more.
const REPORT_PREFIX: [&str; 4] = ["-o", "json", "--output-fields=MESSAGE,SYSLOG_IDENTIFIER,_COMM", "--no-pager"];

/// Every read starts with these.
const PREFIX: [&str; 4] =
    ["-o", "json", "--output-fields=MESSAGE,PRIORITY,SYSLOG_IDENTIFIER,_COMM,_PID,_SYSTEMD_UNIT,_SYSTEMD_USER_UNIT", "--no-pager"];

/// The `journalctl` arguments of `q`; an invalid unit or cursor is an error and nothing is run.
pub fn journal_args(q: &JournalQuery) -> Result<Vec<String>, String> {
    let prefix = if q.report_fields { &REPORT_PREFIX } else { &PREFIX };
    let mut a: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    if q.after_cursor.is_none() {
        a.push("-n".into());
        a.push(q.lines.clamp(1, 5000).to_string());
    }
    let mut add = |words: &[&str]| a.extend(words.iter().map(|w| w.to_string()));
    match q.preset {
        Preset::ErrorsThisBoot | Preset::NetworkManager => add(&["-b"]),
        Preset::Usb => add(&["-b", "-k", "--grep=usb", "--case-sensitive=false"]),
        Preset::SuspendResume => add(&["-b", "--grep=suspend|resume|hibernate|wakeup|sleep", "--case-sensitive=false"]),
        Preset::OutOfMemory => add(&["-b", "-k", "--grep=oom|killed process|out of memory", "--case-sensitive=false"]),
        Preset::Thermal => add(&["-b", "-k", "--grep=thermal|throttl", "--case-sensitive=false"]),
        Preset::Live | Preset::ServiceLogs => {}
    }
    if let Some((scope, unit)) = &q.unit {
        if !valid_unit(unit) {
            return Err("not a unit name".into());
        }
        if *scope == Scope::User {
            a.push("--user".into());
        }
        a.push(format!("--unit={unit}"));
    }
    if let Some(level) = q.priority.word() {
        a.push("-p".into());
        a.push(level.into());
    }
    if let Some((since, until)) = q.since_until {
        a.push(format!("--since=@{since}"));
        a.push(format!("--until=@{until}"));
    }
    if let Some(cursor) = &q.after_cursor {
        if !valid_cursor(cursor) {
            return Err("not a journal cursor".into());
        }
        a.push(format!("--after-cursor={cursor}"));
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(q: &JournalQuery) -> Vec<String> {
        journal_args(q).unwrap()
    }

    fn with_prefix(rest: &[&str]) -> Vec<String> {
        PREFIX.iter().chain(rest.iter()).map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_preset_builds_its_exact_arguments() {
        let q = |p| JournalQuery::new(p, 500);
        assert_eq!(args(&q(Preset::ErrorsThisBoot)), with_prefix(&["-n", "500", "-b", "-p", "err"]));
        assert_eq!(args(&q(Preset::Usb)), with_prefix(&["-n", "500", "-b", "-k", "--grep=usb", "--case-sensitive=false"]));
        assert_eq!(
            args(&q(Preset::SuspendResume)),
            with_prefix(&["-n", "500", "-b", "--grep=suspend|resume|hibernate|wakeup|sleep", "--case-sensitive=false"])
        );
        assert_eq!(
            args(&q(Preset::OutOfMemory)),
            with_prefix(&["-n", "500", "-b", "-k", "--grep=oom|killed process|out of memory", "--case-sensitive=false"])
        );
        assert_eq!(args(&q(Preset::Thermal)), with_prefix(&["-n", "500", "-b", "-k", "--grep=thermal|throttl", "--case-sensitive=false"]));
        assert_eq!(args(&q(Preset::NetworkManager)), with_prefix(&["-n", "500", "-b", "--unit=NetworkManager.service"]));
        assert_eq!(args(&q(Preset::Live)), with_prefix(&["-n", "500"]));
        let mut service = q(Preset::ServiceLogs);
        service.unit = Some((Scope::System, "example-sync.service".into()));
        assert_eq!(args(&service), with_prefix(&["-n", "500", "--unit=example-sync.service"]));
        assert_eq!(Preset::ALL.map(Preset::label).len(), 8);
        assert!(Preset::Live.follows() && Preset::Usb.follows() && !Preset::Thermal.follows());
    }

    #[test]
    fn the_report_asks_for_three_fields_and_nothing_else() {
        let a = args(&JournalQuery::report_errors(200));
        assert_eq!(a, ["-o", "json", "--output-fields=MESSAGE,SYSLOG_IDENTIFIER,_COMM", "--no-pager", "-n", "200", "-b", "-p", "err"]);
        assert!(!a.join(" ").contains("_CMDLINE") && !a.join(" ").contains("_PID"));
    }

    #[test]
    fn a_user_unit_reads_the_user_journal() {
        let mut q = JournalQuery::new(Preset::ServiceLogs, 100);
        q.unit = Some((Scope::User, "example-app.service".into()));
        assert_eq!(args(&q), with_prefix(&["-n", "100", "--user", "--unit=example-app.service"]));
    }

    #[test]
    fn a_dash_unit_is_attached() {
        let mut q = JournalQuery::new(Preset::ServiceLogs, 100);
        q.unit = Some((Scope::System, "-.mount".into()));
        assert_eq!(args(&q), with_prefix(&["-n", "100", "--unit=-.mount"]));
    }

    #[test]
    fn an_injected_unit_or_cursor_is_refused() {
        for bad in ["--now", "a b.service", "", "x;rm.service", "--vacuum-size=1.service\n"] {
            let mut q = JournalQuery::new(Preset::ServiceLogs, 100);
            q.unit = Some((Scope::System, bad.into()));
            assert!(journal_args(&q).is_err(), "{bad:?}");
        }
        for bad in ["", "a b", "--flush", "é", "x\ny"] {
            let mut q = JournalQuery::new(Preset::Live, 100);
            q.after_cursor = Some(bad.into());
            // A leading dash is harmless once attached (`--after-cursor=--flush`): only the space
            // and non-printable ones are refused, and the empty one.
            assert_eq!(journal_args(&q).is_err(), bad != "--flush", "{bad:?}");
        }
    }

    #[test]
    fn follow_has_no_line_count() {
        let mut q = JournalQuery::new(Preset::Live, 500);
        q.after_cursor = Some("s=abc;i=1".into());
        let a = args(&q);
        assert!(!a.contains(&"-n".to_string()), "{a:?}");
        assert_eq!(a, with_prefix(&["--after-cursor=s=abc;i=1"]));
    }

    #[test]
    fn filters_priority_and_window() {
        let mut q = JournalQuery::new(Preset::Live, 20_000);
        q.priority = Priority::Warning;
        q.since_until = Some((1_763_699_940, 1_763_700_005));
        assert_eq!(args(&q), with_prefix(&["-n", "5000", "-p", "warning", "--since=@1763699940", "--until=@1763700005"]), "lines clamped");
        let mut zero = JournalQuery::new(Preset::Live, 0);
        zero.priority = Priority::Info;
        assert_eq!(args(&zero), with_prefix(&["-n", "1", "-p", "info"]));
        let cycle: Vec<Priority> = std::iter::successors(Some(Priority::Any), |p| Some(p.next())).take(6).collect();
        assert_eq!(cycle, [Priority::Any, Priority::Err, Priority::Warning, Priority::Notice, Priority::Info, Priority::Any]);
    }
}
