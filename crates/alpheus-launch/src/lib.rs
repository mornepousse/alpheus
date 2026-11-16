//! alpheus-launch — the bridge to the launcher that lives inside
//! `alpheus-demon` (spec docs/specs/2026-10-05-alpheus-launcher-design.md,
//! § 1). No Wayland, no fonts: it sends one request on
//! `$XDG_RUNTIME_DIR/alpheus-launcher.sock` and waits for the answer.
//!
//! `--dmenu` keeps `fuzzel --dmenu`'s contract, so scripts switch without
//! other changes: the chosen line on stdout and exit 0; Escape → nothing,
//! exit 1; anything wrong (another menu open, bad usage, no answer) → a
//! message on stderr, exit 2. One code of its own: 3 when the daemon is
//! unreachable (not running) or too old for `--index`/`--password`/`--visible`, the only
//! cases where a caller may fall back to another menu.

use alpheus_commun::launcher_ipc::{
    DND_MAX_SECS, DND_MIN_SECS, DmenuFlags, DndEnd, DndVerb, MAX_DMENU_BYTES, MAX_LINES, MAX_PROMPT, OsdVerb, QuickVerb, REPLY_TIMEOUT,
    Reply, Request, TIMER_MAX_SECS, TIMER_MIN_SECS, TimerVerb, exchange_waiting, numbered_lines, wipe,
};
use alpheus_commun::local_time;
pub use alpheus_commun::local_time::parse_clock;
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::Duration;

pub const EXIT_OK: i32 = 0;
/// dmenu cancelled (fuzzel's contract).
pub const EXIT_CANCELLED: i32 = 1;
/// busy (another menu open), usage error, protocol error or no answer, input too large.
pub const EXIT_ERROR: i32 = 2;
/// the daemon is unreachable: no socket, connection refused (not running). The only
/// code on which callers (`shell-ctl launcher`, `tools-menu`) fall back to fuzzel.
pub const EXIT_UNREACHABLE: i32 = 3;
/// toggle/close: give up waiting for "done" after this. (Client side; the daemon's
/// own `socket::REPLY_TIMEOUT` bounds its reply writes, so a stuck client cannot hold the daemon.)
pub const CLIENT_REPLY_TIMEOUT: Duration = REPLY_TIMEOUT;

const USAGE: &str = "\
Usage: alpheus-launch [--toggle | --close | --dmenu [--prompt TEXT] [--index] [--password] [--visible N]
                      | --dnd on|off|toggle [--for 1h30m | --until HH:MM] | --clear-notifications | --osd brightness | --quick [sound|wifi|bluetooth] | --calc | --windows
                      | --timer start DURATION|pomodoro|stopwatch|pause|resume|toggle|stop|skip|popup | --help]

  (no option), --toggle   open the launcher, or close it if it is open
  --close                 close the launcher
  --dmenu                 show the lines read on standard input, print the chosen one
  -p, --prompt TEXT       with --dmenu: text shown before the input
  --index                 with --dmenu: print the chosen line's position (from 0) instead of
                          its text; -1 for typed text that matches no line
  --password              with --dmenu: show the typed text as dots and no list (standard
                          input is ignored); the text is printed as typed, never logged
  --visible N             with --dmenu: list only the first N lines until something is typed;
                          typing searches every line (not with --password)
  --placeholder TEXT, --width N, --lines N
                          with --dmenu: accepted for fuzzel's sake, ignored
  --dnd on|off|toggle     the bar's do-not-disturb (notifications module)
  --for DURATION, --until HH:MM
                          with --dnd on: end it after 30m, 2h or 1h30m (1m to 24h), or at the
                          next local HH:MM; either one replaces an earlier end
  --clear-notifications   empty the bar's notification history
  --osd brightness        show the laptop's backlight level (read once; never changed)
  --quick [sound|wifi|bluetooth]
                          open or close the bar's quick settings (at the volume with sound, on the Wi-Fi
                          with wifi, on Bluetooth with bluetooth)
  --calc                  open the launcher as a calculator (Enter copies the result), or close it
  --windows               open the window switcher (the previous window selected), or select the next window when it is open
  --timer start DURATION  start the bar's focus timer: 90s, 25m, 1h30m or a bare number of minutes (1 s to 24 h)
  --timer pomodoro|stopwatch|pause|resume|toggle|stop|skip|popup
                          start a Pomodoro or a stopwatch, pause, resume, toggle or stop the timer,
                          skip to a Pomodoro's next phase, open or close its popup
  -h, --help              show this help

Exit codes: 0 done or chosen (--osd: also when there is nothing to show; --quick: also without the quick module), 1 menu cancelled,
2 error (another menu open, bad usage, no answer, the bar restarting mid-request, the notifications
or timer module not in bar.toml), 3 alpheus-demon not running (launcher unreachable), or older than
--index/--password/--visible/--for/--until/--quick (or its wifi or bluetooth word)/--calc/--windows/--timer (restarted since the upgrade? until then, use another menu).

--dmenu waits for a choice with no timeout of its own: as long as the
person takes to pick a line, however long that is.
";

/// How a timed `--dnd on` ends, as typed: a length, or a local time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndArg {
    For(u32),
    Until(u32, u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Toggle,
    Close,
    Dmenu { prompt: Option<String>, flags: DmenuFlags },
    Dnd(DndVerb, Option<EndArg>),
    ClearNotifications,
    Osd(OsdVerb),
    Quick(QuickVerb),
    Calc,
    Windows,
    Timer(TimerVerb),
    Help,
}

/// No argument or --toggle → Toggle; --close; --dmenu [--prompt TEXT | --prompt=TEXT | -p TEXT]
/// [--index] [--password] [--placeholder TEXT] [--width N] [--lines N]; -h/--help. The last three
/// are fuzzel's, accepted (also as `--name=value`) and ignored: nothing of them is sent. A dmenu
/// option without --dmenu, unknown options, two modes, a second prompt → Err(usage message).
/// `args` excludes the program name.
pub fn parse_args(args: &[OsString]) -> Result<Invocation, String> {
    let mut mode: Option<Invocation> = None;
    let mut prompt: Option<String> = None;
    let mut flags = DmenuFlags::default();
    // The first dmenu-only option seen, for the message when --dmenu is missing.
    let mut dmenu_only: Option<&'static str> = None;
    // `--for` / `--until`, as typed (the first one's flag and value, and whether both came).
    let mut end: Option<(&'static str, EndArg)> = None;
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let arg = utf8(arg)?;
        match arg {
            "--toggle" => choose(&mut mode, Invocation::Toggle)?,
            "--close" => choose(&mut mode, Invocation::Close)?,
            "--dmenu" => choose(&mut mode, Invocation::Dmenu { prompt: None, flags: DmenuFlags::default() })?,
            "--dnd" => choose(&mut mode, Invocation::Dnd(verb_arg(&mut it, "--dnd", "on, off or toggle", DndVerb::parse)?, None))?,
            "--clear-notifications" => choose(&mut mode, Invocation::ClearNotifications)?,
            "--osd" => choose(&mut mode, Invocation::Osd(verb_arg(&mut it, "--osd", "brightness", OsdVerb::parse)?))?,
            "--quick" => {
                // The one optional word: taken only when it is one of QuickVerb's.
                let verb = match it.peek().and_then(|a| a.to_str()).and_then(QuickVerb::parse) {
                    Some(v) => {
                        it.next();
                        v
                    }
                    None => QuickVerb::Top,
                };
                choose(&mut mode, Invocation::Quick(verb))?
            }
            "--for" => {
                let value = it.next().ok_or_else(|| "--for needs a duration".to_string())?;
                set_end(&mut end, "--for", utf8(value)?)?;
            }
            "--until" => {
                let value = it.next().ok_or_else(|| "--until needs a time".to_string())?;
                set_end(&mut end, "--until", utf8(value)?)?;
            }
            "--timer" => choose(&mut mode, Invocation::Timer(timer_arg(&mut it)?))?,
            "--calc" => choose(&mut mode, Invocation::Calc)?,
            "--windows" => choose(&mut mode, Invocation::Windows)?,
            "-h" | "--help" => choose(&mut mode, Invocation::Help)?,
            "-p" | "--prompt" => {
                let value = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
                set_prompt(&mut prompt, utf8(value)?)?;
                dmenu_only.get_or_insert("--prompt");
            }
            "--index" => {
                flags.index = true;
                dmenu_only.get_or_insert("--index");
            }
            "--password" => {
                flags.password = true;
                dmenu_only.get_or_insert("--password");
            }
            "--visible" => {
                let value = it.next().ok_or_else(|| "--visible needs a number".to_string())?;
                set_visible(&mut flags, utf8(value)?)?;
                dmenu_only.get_or_insert("--visible");
            }
            _ => {
                let (name, inline) = match arg.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (arg, None),
                };
                if let ("--prompt", Some(value)) = (name, inline) {
                    set_prompt(&mut prompt, value)?;
                    dmenu_only.get_or_insert("--prompt");
                    continue;
                }
                if let (flag @ ("--for" | "--until"), Some(value)) = (name, inline) {
                    set_end(&mut end, if flag == "--for" { "--for" } else { "--until" }, value)?;
                    continue;
                }
                if let ("--visible", Some(value)) = (name, inline) {
                    set_visible(&mut flags, value)?;
                    dmenu_only.get_or_insert("--visible");
                    continue;
                }
                let Some(&name) = IGNORED.iter().find(|&&n| n == name) else {
                    return Err(format!("unknown option {arg:?}"));
                };
                let value = match inline {
                    Some(value) => value,
                    None => utf8(it.next().ok_or_else(|| format!("{name} needs a value"))?)?,
                };
                ignored(name, value, &mut dmenu_only)?;
            }
        }
    }
    // The end belongs to `--dnd on` alone.
    let mode = match (mode, end) {
        (mode, None) => mode,
        (Some(Invocation::Dnd(DndVerb::On, _)), Some((_, arg))) => Some(Invocation::Dnd(DndVerb::On, Some(arg))),
        (_, Some((flag, _))) => return Err(format!("{flag} only goes with --dnd on")),
    };
    match (mode, dmenu_only) {
        // A masked entry has no list to point into: refused rather than silently answered -1.
        (Some(Invocation::Dmenu { .. }), _) if flags.index && flags.password => Err("--password and --index do not go together".into()),
        (Some(Invocation::Dmenu { .. }), _) if flags.password && flags.visible.is_some() => {
            Err("--password and --visible do not go together".into())
        }
        (Some(Invocation::Dmenu { .. }), _) => Ok(Invocation::Dmenu { prompt, flags }),
        (_, Some(option)) => Err(format!("{option} only goes with --dmenu")),
        (mode, None) => Ok(mode.unwrap_or(Invocation::Toggle)),
    }
}

/// `--for D` or `--until HH:MM`: parsed now (a bad value is a usage error), one of them only.
/// Where it belongs (`--dnd on`) is checked once every argument is read.
fn set_end(slot: &mut Option<(&'static str, EndArg)>, flag: &'static str, value: &str) -> Result<(), String> {
    let arg = if flag == "--for" {
        EndArg::For(parse_duration(value).ok_or_else(|| format!("--for takes 1m to 24h, like 30m, 2h or 1h30m, not {value:?}"))?)
    } else {
        let (h, m) = parse_clock(value).ok_or_else(|| format!("--until takes a local time HH:MM, not {value:?}"))?;
        EndArg::Until(h, m)
    };
    if slot.is_some() {
        return Err("--for and --until do not go together".into());
    }
    *slot = Some((flag, arg));
    Ok(())
}

/// `<n>h`, `<n>m` or `<n>h<n>m` (digits only) in seconds, from `DND_MIN_SECS` to `DND_MAX_SECS`.
pub fn parse_duration(s: &str) -> Option<u32> {
    fn digits(s: &str) -> Option<u32> {
        if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let (hours, rest) = match s.split_once('h') {
        Some((h, rest)) => (digits(h)?, rest),
        None => (0, s),
    };
    let minutes = match (rest.strip_suffix('m'), rest.is_empty()) {
        (Some(m), _) => digits(m)?,
        (None, true) if s.contains('h') => 0,
        _ => return None,
    };
    let secs = hours.checked_mul(3600)?.checked_add(minutes.checked_mul(60)?)?;
    (DND_MIN_SECS..=DND_MAX_SECS).contains(&secs).then_some(secs)
}

/// The request an invocation sends, `now` being the Unix time (for `--until`). Only the
/// invocations that carry one: Help and Dmenu are `run`'s own.
pub fn request_of(i: Invocation, now: i64) -> Result<Request, String> {
    Ok(match i {
        Invocation::Toggle => Request::Toggle,
        Invocation::Close => Request::Close,
        Invocation::Dnd(v, None) => Request::Dnd(v),
        Invocation::Dnd(_, Some(EndArg::For(secs))) => Request::DndOn(DndEnd::For(secs)),
        Invocation::Dnd(_, Some(EndArg::Until(h, m))) => {
            let at = local_time::next_local(h, m, now).ok_or("no such local time")?;
            Request::DndOn(DndEnd::Until(u64::try_from(at).map_err(|_| "no such local time")?))
        }
        Invocation::ClearNotifications => Request::ClearNotifications,
        Invocation::Osd(v) => Request::Osd(v),
        Invocation::Quick(v) => Request::Quick(v),
        Invocation::Calc => Request::Calc,
        Invocation::Windows => Request::Windows,
        Invocation::Timer(v) => Request::Timer(v),
        Invocation::Help | Invocation::Dmenu { .. } => return Err("not a plain request".into()),
    })
}

/// `--visible N`: decimal digits only (at most 12, as the wire's numbers), given once.
fn set_visible(flags: &mut DmenuFlags, value: &str) -> Result<(), String> {
    if flags.visible.is_some() {
        return Err("--visible given twice".into());
    }
    if value.is_empty() || value.len() > 12 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("--visible takes a number, not {value:?}"));
    }
    flags.visible = value.parse().ok();
    Ok(())
}

/// fuzzel's options that `--dmenu` accepts and ignores.
const IGNORED: [&str; 3] = ["--placeholder", "--width", "--lines"];

/// fuzzel's `--placeholder TEXT`, `--width N`, `--lines N`: checked like fuzzel checks them
/// (a number for the last two), then dropped — the box keeps its own size and shows no
/// placeholder.
fn ignored(name: &'static str, value: &str, dmenu_only: &mut Option<&'static str>) -> Result<(), String> {
    if name != "--placeholder" && value.parse::<u32>().is_err() {
        return Err(format!("{name} takes a number, not {value:?}"));
    }
    dmenu_only.get_or_insert(name);
    Ok(())
}

/// A `--timer` duration as typed, in seconds: `90s`, `25m`, `1h30m` (hours, then minutes, then
/// seconds, each at most once) or a bare number of minutes; one second to a day. Anything else
/// (a fraction, a sign, another order, no unit after digits but the bare form) is None.
pub fn parse_timer_duration(s: &str) -> Option<u32> {
    let secs = if s.bytes().all(|b| b.is_ascii_digit()) { digits(s)?.checked_mul(60)? } else { units(s)? };
    u32::try_from(secs).ok().filter(|s| (TIMER_MIN_SECS..=TIMER_MAX_SECS).contains(s))
}

/// Plain decimal digits, at most six (a day is 1,440 minutes or 86,400 seconds).
fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `1h30m20s` and its parts, in that order, each at most once.
fn units(s: &str) -> Option<u64> {
    let (mut rest, mut total, mut last) = (s, 0u64, 0u8);
    while !rest.is_empty() {
        let at = rest.find(|c: char| !c.is_ascii_digit())?;
        let (n, unit) = (digits(&rest[..at])?, rest.as_bytes()[at]);
        let (rank, factor) = match unit {
            b'h' => (1, 3600),
            b'm' => (2, 60),
            b's' => (3, 1),
            _ => return None,
        };
        if rank <= last {
            return None;
        }
        last = rank;
        total = total.checked_add(n.checked_mul(factor)?)?;
        rest = &rest[at + 1..];
    }
    Some(total)
}

/// The words after `--timer`: `start DURATION`, or one of `TimerVerb::parse_word`'s.
fn timer_arg<'a>(it: &mut impl Iterator<Item = &'a OsString>) -> Result<TimerVerb, String> {
    const WORDS: &str = "start, pomodoro, stopwatch, pause, resume, toggle, stop, skip or popup";
    let word = utf8(it.next().ok_or_else(|| format!("--timer needs {WORDS}"))?)?;
    if word == "start" {
        let value = utf8(it.next().ok_or("--timer start needs a duration (90s, 25m, 1h30m or minutes)")?)?;
        return parse_timer_duration(value)
            .map(TimerVerb::Start)
            .ok_or_else(|| format!("--timer start takes 90s, 25m, 1h30m or minutes, from 1 second to 24 hours, not {value:?}"));
    }
    TimerVerb::parse_word(word).ok_or_else(|| format!("--timer takes {WORDS}, not {word:?}"))
}

/// The word after `--dnd` or `--osd`: one of `words` (as `parse` knows them), else a usage message.
fn verb_arg<'a, T>(
    it: &mut impl Iterator<Item = &'a OsString>,
    flag: &str,
    words: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<T, String> {
    let value = utf8(it.next().ok_or_else(|| format!("{flag} needs {words}"))?)?;
    parse(value).ok_or_else(|| format!("{flag} takes {words}, not {value:?}"))
}

fn utf8(arg: &OsString) -> Result<&str, String> {
    arg.to_str().ok_or_else(|| format!("argument is not valid UTF-8: {}", arg.to_string_lossy()))
}

fn choose(slot: &mut Option<Invocation>, i: Invocation) -> Result<(), String> {
    if slot.is_some() {
        return Err(
            "only one of --toggle, --close, --dmenu, --dnd, --clear-notifications, --osd, --quick, --calc, --windows, --timer, --help"
                .into(),
        );
    }
    *slot = Some(i);
    Ok(())
}

fn set_prompt(slot: &mut Option<String>, value: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err("--prompt given twice".into());
    }
    if value.len() > MAX_PROMPT {
        return Err(format!("prompt longer than {MAX_PROMPT} bytes"));
    }
    *slot = Some(value.to_string());
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// No socket, connection refused: "alpheus-demon is not running (no launcher socket at …)".
    /// `run` maps it to EXIT_UNREACHABLE.
    NotRunning(String),
    Protocol(String),
}

/// `launcher_ipc::exchange` with the wait each request has (no timeout for dmenu,
/// `CLIENT_REPLY_TIMEOUT` for most, longer for a displays trial), its errors told apart: a daemon
/// that is not running from a failed exchange.
pub fn exchange(socket: &Path, request: &Request) -> Result<Reply, ClientError> {
    exchange_waiting(socket, request, request.reply_timeout()).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => ClientError::NotRunning(e.to_string()),
        _ => ClientError::Protocol(e.to_string()),
    })
}

/// stdin → lines, refused (Err message) above the protocol's limits — before any connection.
/// With `numbered`, also each line's position on stdin (`numbered_lines`), for `--index`.
fn read_lines(stdin: &mut dyn Read, numbered: bool) -> Result<(Vec<String>, Vec<usize>), String> {
    let mut bytes = Vec::new();
    (&mut *stdin).take(MAX_DMENU_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("cannot read standard input: {e}"))?;
    let too_large = || format!("standard input is larger than {} MiB", MAX_DMENU_BYTES >> 20);
    if bytes.len() > MAX_DMENU_BYTES {
        return Err(too_large());
    }
    let (mut lines, mut positions) = (Vec::new(), Vec::new());
    for (position, line) in numbered_lines(&String::from_utf8_lossy(&bytes)) {
        lines.push(line.to_string());
        if numbered {
            positions.push(position);
        }
    }
    if lines.len() > MAX_LINES {
        return Err(format!("more than {MAX_LINES} lines on standard input"));
    }
    // U+FFFD takes 3 bytes where an invalid byte took 1: check the body as it will be sent.
    if lines.iter().map(|l| l.len() + 1).sum::<usize>().saturating_sub(1) > MAX_DMENU_BYTES {
        return Err(too_large());
    }
    Ok((lines, positions))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// One line on stderr; the exit code for a plain failure.
fn fail(stderr: &mut dyn Write, m: &str) -> i32 {
    let _ = writeln!(stderr, "alpheus-launch: {m}");
    EXIT_ERROR
}

/// A daemon that does not know `what` (an upgrade not followed by a restart): exit 3, so a caller
/// may fall back to another menu.
fn daemon_too_old(stderr: &mut dyn Write, what: &str, refusal: &str) -> i32 {
    fail(stderr, &format!("alpheus-demon does not know {what} yet (restart it after the upgrade): {refusal}"));
    EXIT_UNREACHABLE
}

/// The whole program, injectable: returns the exit code.
/// dmenu: reads stdin (at most MAX_DMENU_BYTES + 1 bytes; more → EXIT_ERROR before connecting),
/// lossy UTF-8, split_lines; Chosen(s) → prints s + '\n', EXIT_OK; Cancelled → EXIT_CANCELLED;
/// Busy → stderr "another menu is already open", EXIT_ERROR; Done for toggle/close → EXIT_OK;
/// NotRunning → stderr "… not running …", EXIT_UNREACHABLE; any other failure → EXIT_ERROR.
pub fn run(args: &[OsString], socket: &Path, stdin: &mut dyn Read, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    // `--index`: the stdin position of each line sent (empty lines are not sent but counted).
    let mut positions = Vec::new();
    let request = match parse_args(args) {
        Err(m) => return fail(stderr, &format!("{m} (see --help)")),
        Ok(Invocation::Help) => {
            return if stdout.write_all(USAGE.as_bytes()).and_then(|()| stdout.flush()).is_ok() { EXIT_OK } else { EXIT_ERROR };
        }
        Ok(
            i @ (Invocation::Toggle
            | Invocation::Close
            | Invocation::Dnd(..)
            | Invocation::ClearNotifications
            | Invocation::Osd(_)
            | Invocation::Quick(_)
            | Invocation::Calc
            | Invocation::Windows
            | Invocation::Timer(_)),
        ) => match request_of(i, unix_now()) {
            Ok(request) => request,
            Err(m) => return fail(stderr, &m),
        },
        // --password: stdin is still read (a writer is never cut off), then ignored — fuzzel
        // accepts it too; no line leaves the client.
        Ok(Invocation::Dmenu { prompt, flags }) => match read_lines(stdin, flags.index) {
            Ok((lines, p)) => {
                positions = p;
                Request::Dmenu { prompt, lines: if flags.password { Vec::new() } else { lines }, flags }
            }
            Err(m) => return fail(stderr, &m),
        },
    };
    match exchange(socket, &request) {
        Ok(reply) => reply_to_exit(&request, reply, &positions, stdout, stderr),
        Err(ClientError::NotRunning(m)) => {
            fail(stderr, &m);
            EXIT_UNREACHABLE
        }
        Err(ClientError::Protocol(m)) => fail(stderr, &m),
    }
}

/// What the daemon's `reply` to `request` means for the person: the output and the exit code.
/// `positions` are the stdin positions of the lines sent, for `--index`.
fn reply_to_exit(request: &Request, reply: Reply, positions: &[usize], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let mut print = |text: &str| if writeln!(stdout, "{text}").and_then(|()| stdout.flush()).is_ok() { EXIT_OK } else { EXIT_ERROR };
    match (request, reply) {
        (Request::Dmenu { flags, .. }, Reply::Chosen(mut line)) if !flags.index => {
            let code = print(&line);
            if flags.password {
                wipe(&mut line);
            }
            code
        }
        // fuzzel prints -1 for typed text that matched no line, and exits 0.
        (Request::Dmenu { flags, .. }, Reply::Index(None)) if flags.index => print("-1"),
        (Request::Dmenu { flags, .. }, Reply::Index(Some(i))) if flags.index => match positions.get(i) {
            Some(position) => print(&position.to_string()),
            None => fail(stderr, &format!("unexpected answer from alpheus-demon: line {i} of {}", positions.len())),
        },
        (Request::Dmenu { .. }, Reply::Cancelled) => EXIT_CANCELLED,
        (
            Request::Toggle
            | Request::Close
            | Request::Dnd(_)
            | Request::DndOn(_)
            | Request::ClearNotifications
            | Request::Osd(_)
            | Request::Quick(_)
            | Request::Calc
            | Request::Windows
            | Request::Timer(_)
            | Request::OutputsTry(_)
            | Request::OutputsKeep
            | Request::OutputsRevert,
            Reply::Done,
        ) => EXIT_OK,
        (_, Reply::Busy) => fail(stderr, "another menu is already open"),
        (_, Reply::Error(m)) => match too_old_for(request, &m) {
            Some(what) => daemon_too_old(stderr, what, &m),
            None => fail(stderr, &format!("alpheus-demon: {m}")),
        },
        // Never the chosen text itself: with --password, it is the secret.
        (_, Reply::Chosen(mut line)) => {
            wipe(&mut line);
            fail(stderr, "unexpected answer from alpheus-demon: a chosen line")
        }
        (_, other) => fail(stderr, &format!("unexpected answer from alpheus-demon: {other:?}")),
    }
}

/// What `request` asks that a daemon answering `error` is too old to know, if `error` is such a
/// refusal: the flag or word, for the message.
fn too_old_for(request: &Request, error: &str) -> Option<&'static str> {
    match request {
        Request::Dmenu { flags, .. } if refuses_flags(error, flags) => Some(if flags.visible.is_some() {
            "--visible"
        } else if flags.index {
            "--index"
        } else {
            "--password"
        }),
        _ if request.refused_as_unknown_displays_verb(error) => Some("displays"),
        Request::Calc if error == "malformed request: unknown request \"calc\"" => Some("--calc"),
        Request::Windows if error == "malformed request: unknown request \"windows\"" => Some("--windows"),
        Request::Timer(_) => OLD_TIMER.iter().find(|(refusal, _)| *refusal == error).map(|(_, what)| *what),
        // `dnd on for 60` is the unknown request "dnd" to a daemon that knows only `dnd <word>`.
        Request::DndOn(end) if error == "malformed request: unknown request \"dnd\"" => {
            Some(if matches!(end, DndEnd::For(_)) { "--dnd on --for" } else { "--dnd on --until" })
        }
        Request::Quick(verb) => {
            OLD_QUICK.iter().find(|(only, refusal, _)| refusal == &error && only.is_none_or(|v| v == *verb)).map(|(_, _, what)| *what)
        }
        _ => None,
    }
}

/// A daemon older than `--index`/`--password`/`--visible` (an upgrade not followed by a restart) decodes
/// only `dmenu <prompt> <len>`: the trailing flag words make the whole header an unknown request,
/// which `socket::describe` reports as this exact text — stable since the socket's first version.
/// Only that refusal of a flagged request reads as "this daemon cannot serve it": exit 3, so the
/// caller falls back to fuzzel with the flags intact. Never a retry without them: an index would
/// come back as a line's text, a password typed in clear. Any other error (busy, too large, a
/// current daemon refusing a word) stays a plain error, 2, as before.
const OLD_DAEMON_REFUSAL: &str = "malformed request: unknown request \"dmenu\"";

/// What a daemon that knows `--index`/`--password` but not `--visible` answers it.
const NO_VISIBLE_REFUSAL: &str = "malformed request: unknown dmenu flag \"visible\"";
/// What a daemon too old for `--quick`, or for one of its words, answers (`socket::describe` of an
/// unknown request, `parse_header`'s unknown quick word): the verb it concerns (None: any), the
/// refusal and the name for the message. Exit 3, as for the other verbs, so a key bound to it can
/// fall back. A new word adds a row, nothing else.
const OLD_QUICK: [(Option<QuickVerb>, &str, &str); 3] = [
    (None, "malformed request: unknown request \"quick\"", "--quick"),
    // Quick settings delivery 1's daemon.
    (Some(QuickVerb::Wifi), "malformed request: unknown quick word \"wifi\"", "--quick wifi"),
    // Delivery 2's daemon.
    (Some(QuickVerb::Bluetooth), "malformed request: unknown quick word \"bluetooth\"", "--quick bluetooth"),
];

/// What a daemon too old for `--timer` answers (`socket::describe` of an unknown request) and the
/// name for the message; exit 3, so a key bound to it can fall back. A word added later adds a row,
/// as `OLD_QUICK`'s.
const OLD_TIMER: [(&str, &str); 1] = [("malformed request: unknown request \"timer\"", "--timer")];

/// Whether `error` means "this daemon is older than the flags this request carries": the oldest
/// refusal for any flagged request, and the `visible` word's own for a request with `--visible`.
fn refuses_flags(error: &str, flags: &DmenuFlags) -> bool {
    let flagged = flags.index || flags.password || flags.visible.is_some();
    (flagged && error == OLD_DAEMON_REFUSAL) || (flags.visible.is_some() && error == NO_VISIBLE_REFUSAL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn accepted_invocations() {
        assert_eq!(parse_args(&args(&[])), Ok(Invocation::Toggle));
        assert_eq!(parse_args(&args(&["--toggle"])), Ok(Invocation::Toggle));
        assert_eq!(parse_args(&args(&["--close"])), Ok(Invocation::Close));
        assert_eq!(parse_args(&args(&["--dmenu"])), Ok(Invocation::Dmenu { prompt: None, flags: DmenuFlags::default() }));
        for a in [
            &["--dmenu", "--prompt", "Pick: "][..],
            &["--prompt", "Pick: ", "--dmenu"],
            &["--dmenu", "--prompt=Pick: "],
            &["-p", "Pick: ", "--dmenu"],
        ] {
            assert_eq!(
                parse_args(&args(a)),
                Ok(Invocation::Dmenu { prompt: Some("Pick: ".into()), flags: DmenuFlags::default() }),
                "{a:?}"
            );
        }
        assert_eq!(
            parse_args(&args(&["--dmenu", "--prompt", ""])),
            Ok(Invocation::Dmenu { prompt: Some(String::new()), flags: DmenuFlags::default() })
        );
        assert_eq!(
            parse_args(&args(&["--dmenu", "-p", "-x"])),
            Ok(Invocation::Dmenu { prompt: Some("-x".into()), flags: DmenuFlags::default() })
        );
        assert_eq!(parse_args(&args(&["-h"])), Ok(Invocation::Help));
        assert_eq!(parse_args(&args(&["--help"])), Ok(Invocation::Help));
        assert_eq!(parse_args(&args(&["--dnd", "on"])), Ok(Invocation::Dnd(DndVerb::On, None)));
        assert_eq!(parse_args(&args(&["--dnd", "off"])), Ok(Invocation::Dnd(DndVerb::Off, None)));
        assert_eq!(parse_args(&args(&["--dnd", "toggle"])), Ok(Invocation::Dnd(DndVerb::Toggle, None)));
        assert_eq!(parse_args(&args(&["--clear-notifications"])), Ok(Invocation::ClearNotifications));
        assert_eq!(parse_args(&args(&["--osd", "brightness"])), Ok(Invocation::Osd(OsdVerb::Brightness)));
        assert_eq!(parse_args(&args(&["--calc"])), Ok(Invocation::Calc));
        assert_eq!(parse_args(&args(&["--windows"])), Ok(Invocation::Windows));
        assert_eq!(parse_args(&args(&["--quick"])), Ok(Invocation::Quick(QuickVerb::Top)));
        assert_eq!(parse_args(&args(&["--quick", "sound"])), Ok(Invocation::Quick(QuickVerb::Sound)));
        assert_eq!(parse_args(&args(&["--quick", "wifi"])), Ok(Invocation::Quick(QuickVerb::Wifi)));
        assert_eq!(parse_args(&args(&["--quick", "bluetooth"])), Ok(Invocation::Quick(QuickVerb::Bluetooth)));
        let dmenu = |index, password| Ok(Invocation::Dmenu { prompt: None, flags: DmenuFlags { index, password, visible: None } });
        assert_eq!(parse_args(&args(&["--dmenu", "--index"])), dmenu(true, false));
        assert_eq!(parse_args(&args(&["--password", "--dmenu"])), dmenu(false, true));
        assert_eq!(parse_args(&args(&["--dmenu", "--index", "--index"])), dmenu(true, false), "repeated, as fuzzel allows");
        let visible = |index, visible| Ok(Invocation::Dmenu { prompt: None, flags: DmenuFlags { index, password: false, visible } });
        assert_eq!(parse_args(&args(&["--dmenu", "--visible", "2"])), visible(false, Some(2)));
        assert_eq!(parse_args(&args(&["--visible=0", "--dmenu"])), visible(false, Some(0)));
        assert_eq!(parse_args(&args(&["--dmenu", "--index", "--visible", "12"])), visible(true, Some(12)));
        for a in [
            &["--dmenu", "--placeholder", "Search"][..],
            &["--dmenu", "--placeholder", "--index"],
            &["--dmenu", "--placeholder="],
            &["--width", "46", "--dmenu", "--lines", "16"],
            &["--dmenu", "--width=50", "--lines=0"],
            &["--dmenu", "--lines", "0", "--lines", "3"],
        ] {
            assert_eq!(parse_args(&args(a)), dmenu(false, false), "{a:?}: accepted, ignored (`--placeholder` takes one argument)");
        }
        assert_eq!(
            parse_args(&args(&["--dmenu", "--placeholder", "x", "-p", "Go: "])),
            Ok(Invocation::Dmenu { prompt: Some("Go: ".into()), flags: DmenuFlags::default() }),
            "the placeholder is not the prompt"
        );
    }

    #[test]
    fn refused_invocations() {
        for (a, needle) in [
            (&["--prompt", "x"][..], "--prompt only goes with --dmenu"),
            (&["--close", "-p", "x"], "--prompt only goes with --dmenu"),
            (&["--index"], "--index only goes with --dmenu"),
            (&["--close", "--password"], "--password only goes with --dmenu"),
            (&["--placeholder", "x"], "--placeholder only goes with --dmenu"),
            (&["--toggle", "--width", "40"], "--width only goes with --dmenu"),
            (&["--lines=3"], "--lines only goes with --dmenu"),
            (&["--dmenu", "--placeholder"], "needs a value"),
            (&["--dmenu", "--width"], "needs a value"),
            (&["--dmenu", "--lines", "many"], "--lines takes a number"),
            (&["--dmenu", "--width=-1"], "--width takes a number"),
            (&["--dmenu", "--index", "--password"], "--password and --index do not go together"),
            (&["--visible", "2"], "--visible only goes with --dmenu"),
            (&["--dmenu", "--visible"], "--visible needs a number"),
            (&["--dmenu", "--visible", "x"], "--visible takes a number"),
            (&["--dmenu", "--visible", "-1"], "--visible takes a number"),
            (&["--dmenu", "--visible", "+2"], "--visible takes a number"),
            (&["--dmenu", "--visible", "1", "--visible", "2"], "--visible given twice"),
            (&["--dmenu", "--password", "--visible", "2"], "--password and --visible do not go together"),
            (&["--password", "--dmenu", "--index", "--index"], "--password and --index do not go together"),
            (&["--dmenu", "--password=*"], "unknown option"),
            (&["--dmenu", "-l", "3"], "unknown option"),
            (&["firefox"], "unknown option"),
            (&["--dmenu", "--dmenu"], "only one of"),
            (&["--toggle", "--close"], "only one of"),
            (&["--quick", "volume"], "unknown option"),
            (&["--quick", "network"], "unknown option"),
            (&["--quick", "wifi", "sound"], "unknown option"),
            (&["--quick", "--close"], "only one of"),
            (&["--calc", "--toggle"], "--quick, --calc, --windows, --timer, --help"),
            (&["--windows", "--toggle"], "--windows"),
            (&["--quick", "sound", "-p", "x"], "--prompt only goes with --dmenu"),
            (&["--help", "--dmenu"], "only one of"),
            (&["--dmenu", "--prompt"], "needs a value"),
            (&["--dmenu", "-p", "a", "-p", "b"], "given twice"),
            (&["--dnd"], "needs on, off or toggle"),
            (&["--dnd", "maybe"], "on, off or toggle"),
            (&["--dnd", "on", "--close"], "only one of"),
            (&["--clear-notifications", "--dnd", "on"], "only one of"),
            (&["--dnd", "on", "-p", "x"], "--prompt only goes with --dmenu"),
            (&["--clear-notifications", "now"], "unknown option"),
            (&["--osd"][..], "--osd needs brightness"),
            (&["--osd", "volume"], "--osd takes brightness"),
            (&["--osd", "brightness", "--close"], "only one of"),
            (&["--osd", "brightness", "-p", "x"], "--prompt only goes with --dmenu"),
        ] {
            let e = parse_args(&args(a)).unwrap_err();
            assert!(e.contains(needle), "{a:?}: {e}");
        }
        let long = "x".repeat(MAX_PROMPT + 1);
        assert!(parse_args(&args(&["--dmenu", "-p", &long])).unwrap_err().contains("prompt longer"));
        let bad = vec![OsString::from("--dmenu"), OsString::from("-p"), OsString::from_vec(vec![0xff])];
        assert!(parse_args(&bad).unwrap_err().contains("UTF-8"));
    }

    /// OSD spec § 4: the daemon unreachable is exit 3 for `--osd` too (the caller's fallback is to
    /// show nothing).
    #[test]
    fn osd_against_no_daemon_exits_3() {
        let socket = std::env::temp_dir().join(format!("alpheus-launch-{}-no-daemon.sock", std::process::id()));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args(&["--osd", "brightness"]), &socket, &mut std::io::empty(), &mut out, &mut err);
        assert_eq!(code, EXIT_UNREACHABLE, "{}", String::from_utf8_lossy(&err));
    }

    /// A daemon from before `--calc` refuses the word as an unknown request: exit 3, naming it.
    #[test]
    fn a_daemon_without_calc_is_too_old() {
        let refusal = "malformed request: unknown request \"calc\"";
        let mut err = Vec::new();
        assert_eq!(reply_to_exit(&Request::Calc, Reply::Error(refusal.into()), &[], &mut Vec::new(), &mut err), EXIT_UNREACHABLE);
        assert!(String::from_utf8_lossy(&err).contains("--calc"), "{}", String::from_utf8_lossy(&err));
        let mut err = Vec::new();
        assert_eq!(reply_to_exit(&Request::Calc, Reply::Error("nope".into()), &[], &mut Vec::new(), &mut err), EXIT_ERROR);
    }

    /// Timer spec § 7, plan decision 12: durations as typed, in seconds.
    #[test]
    fn timer_durations_are_read_as_typed() {
        for (text, secs) in [
            ("90s", 90),
            ("25m", 1500),
            ("1h30m", 5400),
            ("1m30s", 90),
            ("25", 1500),
            ("1h", 3600),
            ("24h", 86_400),
            ("1s", 1),
            ("1h1m1s", 3661),
            ("1440", 86_400),
        ] {
            assert_eq!(parse_timer_duration(text), Some(secs), "{text}");
        }
        for text in ["0s", "24h1s", "m", "1.5m", "30s1m", "", "1h30", "0", "1441", "-5m", "+5m", "1m1m", "5x", "h1", "1 m", "9999999"] {
            assert_eq!(parse_timer_duration(text), None, "{text:?}");
        }
    }

    /// `--timer`'s words, its duration and its usage errors.
    #[test]
    fn timer_arguments_are_parsed() {
        for (a, v) in [
            (&["--timer", "start", "25m"][..], TimerVerb::Start(1500)),
            (&["--timer", "toggle"], TimerVerb::Toggle),
            (&["--timer", "pomodoro"], TimerVerb::Pomodoro),
            (&["--timer", "stopwatch"], TimerVerb::Stopwatch),
            (&["--timer", "popup"], TimerVerb::Popup),
            (&["--timer", "skip"], TimerVerb::Skip),
        ] {
            assert_eq!(parse_args(&args(a)), Ok(Invocation::Timer(v)), "{a:?}");
            assert_eq!(request_of(Invocation::Timer(v), 0), Ok(Request::Timer(v)));
        }
        for (a, needle) in [
            (&["--timer"][..], "needs start, pomodoro"),
            (&["--timer", "start"], "needs a duration"),
            (&["--timer", "start", "0s"], "from 1 second to 24 hours"),
            (&["--timer", "start", "25h"], "from 1 second to 24 hours"),
            (&["--timer", "nap"], "--timer takes start"),
            (&["--timer", "stop", "--close"], "only one of"),
            (&["--toggle", "--timer", "stop"], "only one of"),
            (&["--timer", "stop", "-p", "x"], "--prompt only goes with --dmenu"),
            (&["--timer", "stop", "now"], "unknown option"),
        ] {
            let e = parse_args(&args(a)).unwrap_err();
            assert!(e.contains(needle), "{a:?}: {e}");
        }
    }

    /// A daemon from before `--timer` refuses the word as an unknown request: exit 3, naming it.
    #[test]
    fn a_daemon_without_timer_is_too_old() {
        let refusal = "malformed request: unknown request \"timer\"";
        let request = Request::Timer(TimerVerb::Stop);
        let mut err = Vec::new();
        assert_eq!(reply_to_exit(&request, Reply::Error(refusal.into()), &[], &mut Vec::new(), &mut err), EXIT_UNREACHABLE);
        assert!(String::from_utf8_lossy(&err).contains("--timer"), "{}", String::from_utf8_lossy(&err));
        let mut err = Vec::new();
        let other = "the timer module is not in modules";
        assert_eq!(reply_to_exit(&request, Reply::Error(other.into()), &[], &mut Vec::new(), &mut err), EXIT_ERROR);
        assert!(String::from_utf8_lossy(&err).contains(other), "{}", String::from_utf8_lossy(&err));
    }

    /// A daemon from before `--windows` refuses the word as an unknown request: exit 3, naming it.
    #[test]
    fn a_daemon_without_windows_is_too_old() {
        let refusal = "malformed request: unknown request \"windows\"";
        let mut err = Vec::new();
        assert_eq!(reply_to_exit(&Request::Windows, Reply::Error(refusal.into()), &[], &mut Vec::new(), &mut err), EXIT_UNREACHABLE);
        assert!(String::from_utf8_lossy(&err).contains("--windows"), "{}", String::from_utf8_lossy(&err));
        let mut err = Vec::new();
        assert_eq!(reply_to_exit(&Request::Windows, Reply::Error("nope".into()), &[], &mut Vec::new(), &mut err), EXIT_ERROR);
    }

    /// An upgrade not followed by a restart: a daemon that never heard of the displays verbs
    /// refuses each as an unknown request, which reads as "too old" (exit 3), never as a plain error.
    #[test]
    fn a_daemon_without_the_displays_verbs_is_too_old() {
        let socket_refusal = |word: &str| format!("malformed request: unknown request \"{word}\"");
        for (request, word) in [
            (Request::Outputs, "outputs"),
            (Request::OutputsTry(String::new()), "outputs-try"),
            (Request::OutputsKeep, "outputs-keep"),
            (Request::OutputsRevert, "outputs-revert"),
        ] {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = reply_to_exit(&request, Reply::Error(socket_refusal(word)), &[], &mut out, &mut err);
            assert_eq!(code, EXIT_UNREACHABLE, "{request:?}");
            assert!(String::from_utf8_lossy(&err).contains("displays"), "{}", String::from_utf8_lossy(&err));
            // Another refusal, or another verb's word, stays a plain error.
            for other in ["displays are off in bar.toml".to_string(), socket_refusal("quick")] {
                let mut err = Vec::new();
                assert_eq!(reply_to_exit(&request, Reply::Error(other.clone()), &[], &mut Vec::new(), &mut err), EXIT_ERROR, "{other}");
            }
        }
    }

    #[test]
    fn durations() {
        for (text, secs) in [("30m", 1800), ("2h", 7200), ("1h30m", 5400), ("1m", 60), ("24h", 86_400)] {
            assert_eq!(parse_duration(text), Some(secs), "{text}");
        }
        for text in ["0m", "24h1m", "90", "", "h", "1.5h", "-1h", "1m30s"] {
            assert_eq!(parse_duration(text), None, "{text:?}");
        }
    }

    #[test]
    fn clocks() {
        assert_eq!(parse_clock("07:30"), Some((7, 30)));
        assert_eq!(parse_clock("7:30"), Some((7, 30)));
        assert_eq!(parse_clock("23:59"), Some((23, 59)));
        assert_eq!(parse_clock("0:00"), Some((0, 0)));
        for text in ["24:00", "12:60", "1230", "12:3", "", ":30", "+1:30"] {
            assert_eq!(parse_clock(text), None, "{text:?}");
        }
    }

    #[test]
    fn timed_dnd_invocations() {
        assert_eq!(parse_args(&args(&["--dnd", "on", "--for", "30m"])), Ok(Invocation::Dnd(DndVerb::On, Some(EndArg::For(1800)))));
        assert_eq!(parse_args(&args(&["--dnd", "on", "--for=30m"])), Ok(Invocation::Dnd(DndVerb::On, Some(EndArg::For(1800)))));
        assert_eq!(parse_args(&args(&["--for", "30m", "--dnd", "on"])), Ok(Invocation::Dnd(DndVerb::On, Some(EndArg::For(1800)))));
        assert_eq!(parse_args(&args(&["--dnd", "on", "--until", "07:30"])), Ok(Invocation::Dnd(DndVerb::On, Some(EndArg::Until(7, 30)))));
        assert_eq!(parse_args(&args(&["--dnd", "on", "--until=7:30"])), Ok(Invocation::Dnd(DndVerb::On, Some(EndArg::Until(7, 30)))));
        assert_eq!(parse_args(&args(&["--dnd", "on"])), Ok(Invocation::Dnd(DndVerb::On, None)));
        let refusals: [(&[&str], &str); 8] = [
            (&["--dnd", "on", "--for", "0m"], "--for takes 1m to 24h, like 30m, 2h or 1h30m, not \"0m\""),
            (&["--dnd", "on", "--until", "25:00"], "--until takes a local time HH:MM, not \"25:00\""),
            (&["--dnd", "off", "--for", "30m"], "--for only goes with --dnd on"),
            (&["--dnd", "toggle", "--until", "07:30"], "--until only goes with --dnd on"),
            (&["--for", "30m"], "--for only goes with --dnd on"),
            (&["--until", "07:30"], "--until only goes with --dnd on"),
            (&["--dnd", "on", "--for", "30m", "--until", "07:30"], "--for and --until do not go together"),
            (&["--dnd", "on", "--for"], "--for needs a duration"),
        ];
        for (a, message) in refusals {
            assert_eq!(parse_args(&args(a)), Err(message.to_string()), "{a:?}");
        }
    }

    #[test]
    fn request_of_converts_the_end() {
        assert_eq!(request_of(Invocation::Dnd(DndVerb::On, Some(EndArg::For(60))), 0), Ok(Request::DndOn(DndEnd::For(60))));
        assert_eq!(request_of(Invocation::Dnd(DndVerb::Off, None), 0), Ok(Request::Dnd(DndVerb::Off)));
        assert_eq!(request_of(Invocation::Dnd(DndVerb::On, None), 0), Ok(Request::Dnd(DndVerb::On)));
        let now = 1_763_000_000;
        match request_of(Invocation::Dnd(DndVerb::On, Some(EndArg::Until(7, 30))), now) {
            Ok(Request::DndOn(DndEnd::Until(t))) => assert!(t as i64 > now && t as i64 <= now + 90_000, "{t}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(request_of(Invocation::Toggle, 0), Ok(Request::Toggle));
    }

    /// A daemon from before timed do-not-disturb reads `dnd on for 60` as an unknown request:
    /// exit 3, naming the flag; the same refusal of the plain verb is an ordinary error.
    #[test]
    fn a_daemon_without_timed_dnd_is_too_old() {
        let refusal = "malformed request: unknown request \"dnd\"";
        for (request, flag) in
            [(Request::DndOn(DndEnd::For(60)), "--dnd on --for"), (Request::DndOn(DndEnd::Until(1_763_229_600)), "--dnd on --until")]
        {
            let mut err = Vec::new();
            assert_eq!(reply_to_exit(&request, Reply::Error(refusal.into()), &[], &mut Vec::new(), &mut err), EXIT_UNREACHABLE);
            assert!(String::from_utf8_lossy(&err).contains(flag), "{}", String::from_utf8_lossy(&err));
        }
        let mut err = Vec::new();
        assert_eq!(reply_to_exit(&Request::Dnd(DndVerb::On), Reply::Error(refusal.into()), &[], &mut Vec::new(), &mut err), EXIT_ERROR);
        let mut err = Vec::new();
        assert_eq!(
            reply_to_exit(&Request::DndOn(DndEnd::For(60)), Reply::Error("nope".into()), &[], &mut Vec::new(), &mut err),
            EXIT_ERROR
        );
    }
}
