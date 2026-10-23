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
    DmenuFlags, DndVerb, MAX_DMENU_BYTES, MAX_HEADER, MAX_LINES, MAX_PROMPT, OsdVerb, QuickVerb, Reply, Request, decode_reply,
    encode_request, numbered_lines, wipe, zero,
};
use std::ffi::OsString;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub const EXIT_OK: i32 = 0;
/// dmenu cancelled (fuzzel's contract).
pub const EXIT_CANCELLED: i32 = 1;
/// busy (another menu open), usage error, protocol error or no answer, input too large.
pub const EXIT_ERROR: i32 = 2;
/// the daemon is unreachable: no socket, connection refused (not running). The only
/// code on which callers (`shell-ctl launcher`, `tools-menu`) fall back to fuzzel.
pub const EXIT_UNREACHABLE: i32 = 3;
/// toggle/close: give up waiting for "done" after this. (Client side; the daemon's
/// own `socket::REPLY_TIMEOUT`, task 11, bounds its reply writes.)
pub const CLIENT_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

const USAGE: &str = "\
Usage: alpheus-launch [--toggle | --close | --dmenu [--prompt TEXT] [--index] [--password] [--visible N]
                      | --dnd on|off|toggle | --clear-notifications | --osd brightness | --quick [sound|wifi|bluetooth] | --help]

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
  --clear-notifications   empty the bar's notification history
  --osd brightness        show the laptop's backlight level (read once; never changed)
  --quick [sound|wifi|bluetooth]
                          open or close the bar's quick settings (at the volume with sound, on the Wi-Fi
                          with wifi, on Bluetooth with bluetooth)
  -h, --help              show this help

Exit codes: 0 done or chosen (--osd: also when there is nothing to show; --quick: also without the quick module), 1 menu cancelled,
2 error (another menu open, bad usage, no answer, the bar restarting mid-request, the notifications
module not in bar.toml), 3 alpheus-demon not running (launcher unreachable), or older than
--index/--password/--visible/--quick (or its wifi or bluetooth word) (restarted since the upgrade? until then, use another menu).

--dmenu waits for a choice with no timeout of its own: as long as the
person takes to pick a line, however long that is.
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Toggle,
    Close,
    Dmenu { prompt: Option<String>, flags: DmenuFlags },
    Dnd(DndVerb),
    ClearNotifications,
    Osd(OsdVerb),
    Quick(QuickVerb),
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
    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        let arg = utf8(arg)?;
        match arg {
            "--toggle" => choose(&mut mode, Invocation::Toggle)?,
            "--close" => choose(&mut mode, Invocation::Close)?,
            "--dmenu" => choose(&mut mode, Invocation::Dmenu { prompt: None, flags: DmenuFlags::default() })?,
            "--dnd" => {
                let value = it.next().ok_or_else(|| "--dnd needs on, off or toggle".to_string())?;
                let value = utf8(value)?;
                let verb = DndVerb::parse(value).ok_or_else(|| format!("--dnd takes on, off or toggle, not {value:?}"))?;
                choose(&mut mode, Invocation::Dnd(verb))?
            }
            "--clear-notifications" => choose(&mut mode, Invocation::ClearNotifications)?,
            "--osd" => {
                let value = it.next().ok_or_else(|| "--osd needs brightness".to_string())?;
                let value = utf8(value)?;
                let verb = OsdVerb::parse(value).ok_or_else(|| format!("--osd takes brightness, not {value:?}"))?;
                choose(&mut mode, Invocation::Osd(verb))?
            }
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

fn utf8(arg: &OsString) -> Result<&str, String> {
    arg.to_str().ok_or_else(|| format!("argument is not valid UTF-8: {}", arg.to_string_lossy()))
}

fn choose(slot: &mut Option<Invocation>, i: Invocation) -> Result<(), String> {
    if slot.is_some() {
        return Err("only one of --toggle, --close, --dmenu, --dnd, --clear-notifications, --osd, --quick, --help".into());
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

/// Connects, sends, waits for the reply (no timeout for dmenu, CLIENT_REPLY_TIMEOUT otherwise).
/// The write side is never shut down after sending: the daemon reads a
/// hang-up as "the client went away" and would close a waiting dmenu.
pub fn exchange(socket: &Path, request: &Request) -> Result<Reply, ClientError> {
    let mut stream = UnixStream::connect(socket).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
            ClientError::NotRunning(format!("alpheus-demon is not running (no launcher socket at {})", socket.display()))
        }
        _ => ClientError::Protocol(format!("cannot reach the launcher socket {}: {e}", socket.display())),
    })?;
    // dmenu waits for the person, as long as it takes; toggle/close must be answered
    // within CLIENT_REPLY_TIMEOUT, as one total deadline (not a per-read allowance
    // that a daemon trickling one byte at a time could restart forever).
    let timeout = match request {
        Request::Dmenu { .. } => None,
        Request::Toggle | Request::Close | Request::Dnd(_) | Request::ClearNotifications | Request::Osd(_) | Request::Quick(_) => {
            Some(CLIENT_REPLY_TIMEOUT)
        }
    };
    stream.set_write_timeout(timeout).map_err(|e| ClientError::Protocol(format!("socket setup failed: {e}")))?;
    let deadline = timeout.map(|t| Instant::now() + t);
    let sent = stream.write_all(&encode_request(request));
    let mut answer = Vec::new();
    let read = read_reply(&mut stream, &mut answer, deadline);
    let reply = answer_of(sent, read, &answer);
    // The reply's bytes held the typed password: overwritten before they are freed.
    if matches!(request, Request::Dmenu { flags, .. } if flags.password) {
        zero(&mut answer);
    }
    reply
}

/// The reply in `answer`, or why there is none.
fn answer_of(sent: io::Result<()>, read: io::Result<()>, answer: &[u8]) -> Result<Reply, ClientError> {
    // A daemon that refuses early (request too large) answers before we finished sending.
    if let Ok(reply) = decode_reply(answer) {
        return Ok(reply);
    }
    if let Err(e) = sent {
        return Err(ClientError::Protocol(format!("cannot send the request: {e}")));
    }
    match read {
        Err(e) if e.kind() == ErrorKind::TimedOut => {
            Err(ClientError::Protocol(format!("no answer from alpheus-demon within {} s", CLIENT_REPLY_TIMEOUT.as_secs())))
        }
        Err(e) => Err(ClientError::Protocol(format!("cannot read the answer: {e}"))),
        Ok(()) => decode_reply(answer).map_err(|e| ClientError::Protocol(format!("unexpected answer from alpheus-demon: {e}"))),
    }
}

/// Reads the reply into `answer`, bounded to `MAX_HEADER + MAX_DMENU_BYTES + 1` bytes
/// (enough to tell an oversized or malformed answer apart without ever reading an
/// unbounded stream). With a `deadline`, the whole loop is given up on as a single total
/// budget: the remaining time is recomputed before every read, so a daemon trickling
/// bytes just under a per-read timeout cannot stall the client past it. `deadline: None`
/// (dmenu) waits for the person, however long that takes.
fn read_reply(stream: &mut UnixStream, answer: &mut Vec<u8>, deadline: Option<Instant>) -> io::Result<()> {
    const BOUND: usize = MAX_HEADER + MAX_DMENU_BYTES + 1;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        if answer.len() >= BOUND {
            return Ok(());
        }
        if let Some(dl) = deadline {
            let remaining = dl.checked_duration_since(Instant::now()).filter(|d| !d.is_zero());
            let Some(remaining) = remaining else {
                return Err(io::Error::new(ErrorKind::TimedOut, "deadline exceeded"));
            };
            stream.set_read_timeout(Some(remaining))?;
        }
        let want = chunk.len().min(BOUND - answer.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Ok(()),
            Ok(n) => answer.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(io::Error::new(ErrorKind::TimedOut, "deadline exceeded"));
            }
            Err(e) => return Err(e),
        }
    }
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

/// The whole program, injectable: returns the exit code.
/// dmenu: reads stdin (at most MAX_DMENU_BYTES + 1 bytes; more → EXIT_ERROR before connecting),
/// lossy UTF-8, split_lines; Chosen(s) → prints s + '\n', EXIT_OK; Cancelled → EXIT_CANCELLED;
/// Busy → stderr "another menu is already open", EXIT_ERROR; Done for toggle/close → EXIT_OK;
/// NotRunning → stderr "… not running …", EXIT_UNREACHABLE; any other failure → EXIT_ERROR.
pub fn run(args: &[OsString], socket: &Path, stdin: &mut dyn Read, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let mut fail = |m: &str| {
        let _ = writeln!(stderr, "alpheus-launch: {m}");
        EXIT_ERROR
    };
    // `--index`: the stdin position of each line sent (empty lines are not sent but counted).
    let mut positions = Vec::new();
    let request = match parse_args(args) {
        Err(m) => return fail(&format!("{m} (see --help)")),
        Ok(Invocation::Help) => {
            return if stdout.write_all(USAGE.as_bytes()).and_then(|()| stdout.flush()).is_ok() { EXIT_OK } else { EXIT_ERROR };
        }
        Ok(Invocation::Toggle) => Request::Toggle,
        Ok(Invocation::Close) => Request::Close,
        Ok(Invocation::Dnd(v)) => Request::Dnd(v),
        Ok(Invocation::ClearNotifications) => Request::ClearNotifications,
        Ok(Invocation::Osd(v)) => Request::Osd(v),
        Ok(Invocation::Quick(v)) => Request::Quick(v),
        // --password: stdin is still read (a writer is never cut off), then ignored — fuzzel
        // accepts it too; no line leaves the client.
        Ok(Invocation::Dmenu { prompt, flags }) => match read_lines(stdin, flags.index) {
            Ok((lines, p)) => {
                positions = p;
                Request::Dmenu { prompt, lines: if flags.password { Vec::new() } else { lines }, flags }
            }
            Err(m) => return fail(&m),
        },
    };
    let reply = match exchange(socket, &request) {
        Ok(r) => r,
        Err(ClientError::NotRunning(m)) => {
            fail(&m);
            return EXIT_UNREACHABLE;
        }
        Err(ClientError::Protocol(m)) => return fail(&m),
    };
    let mut print = |text: &str| if writeln!(stdout, "{text}").and_then(|()| stdout.flush()).is_ok() { EXIT_OK } else { EXIT_ERROR };
    match (&request, reply) {
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
            None => fail(&format!("unexpected answer from alpheus-demon: line {i} of {}", positions.len())),
        },
        (Request::Dmenu { .. }, Reply::Cancelled) => EXIT_CANCELLED,
        (
            Request::Toggle | Request::Close | Request::Dnd(_) | Request::ClearNotifications | Request::Osd(_) | Request::Quick(_),
            Reply::Done,
        ) => EXIT_OK,
        (_, Reply::Busy) => fail("another menu is already open"),
        (Request::Dmenu { flags, .. }, Reply::Error(m)) if refuses_flags(&m, flags) => {
            let flag = if flags.visible.is_some() {
                "--visible"
            } else if flags.index {
                "--index"
            } else {
                "--password"
            };
            fail(&format!("alpheus-demon does not know {flag} yet (restart it after the upgrade): {m}"));
            EXIT_UNREACHABLE
        }
        (Request::Quick(_), Reply::Error(m)) if m == OLD_QUICK_REFUSAL => {
            fail(&format!("alpheus-demon does not know --quick yet (restart it after the upgrade): {m}"));
            EXIT_UNREACHABLE
        }
        (Request::Quick(QuickVerb::Wifi), Reply::Error(m)) if m == OLD_QUICK_WIFI_REFUSAL => {
            fail(&format!("alpheus-demon does not know --quick wifi yet (restart it after the upgrade): {m}"));
            EXIT_UNREACHABLE
        }
        (Request::Quick(QuickVerb::Bluetooth), Reply::Error(m)) if m == OLD_QUICK_BLUETOOTH_REFUSAL => {
            fail(&format!("alpheus-demon does not know --quick bluetooth yet (restart it after the upgrade): {m}"));
            EXIT_UNREACHABLE
        }
        (_, Reply::Error(m)) => fail(&format!("alpheus-demon: {m}")),
        // Never the chosen text itself: with --password, it is the secret.
        (_, Reply::Chosen(mut line)) => {
            wipe(&mut line);
            fail("unexpected answer from alpheus-demon: a chosen line")
        }
        (_, other) => fail(&format!("unexpected answer from alpheus-demon: {other:?}")),
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
/// What a daemon older than `--quick` answers it (`socket::describe` of an unknown request): exit 3,
/// as for the other verbs, so a key bound to it can fall back.
const OLD_QUICK_REFUSAL: &str = "malformed request: unknown request \"quick\"";
/// What a daemon of quick settings delivery 1 answers `--quick wifi` (`parse_header`'s unknown quick
/// word): exit 3, so a key bound to it can fall back.
const OLD_QUICK_WIFI_REFUSAL: &str = "malformed request: unknown quick word \"wifi\"";
/// What a daemon of quick settings delivery 2 answers `--quick bluetooth`: exit 3, as for `wifi`.
const OLD_QUICK_BLUETOOTH_REFUSAL: &str = "malformed request: unknown quick word \"bluetooth\"";

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
        assert_eq!(parse_args(&args(&["--dnd", "on"])), Ok(Invocation::Dnd(DndVerb::On)));
        assert_eq!(parse_args(&args(&["--dnd", "off"])), Ok(Invocation::Dnd(DndVerb::Off)));
        assert_eq!(parse_args(&args(&["--dnd", "toggle"])), Ok(Invocation::Dnd(DndVerb::Toggle)));
        assert_eq!(parse_args(&args(&["--clear-notifications"])), Ok(Invocation::ClearNotifications));
        assert_eq!(parse_args(&args(&["--osd", "brightness"])), Ok(Invocation::Osd(OsdVerb::Brightness)));
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
}
