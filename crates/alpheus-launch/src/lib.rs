//! alpheus-launch — the bridge to the launcher that lives inside
//! `alpheus-demon` (spec docs/specs/2026-10-05-alpheus-launcher-design.md,
//! § 1). No Wayland, no fonts: it sends one request on
//! `$XDG_RUNTIME_DIR/alpheus-launcher.sock` and waits for the answer.
//!
//! `--dmenu` keeps `fuzzel --dmenu`'s contract, so scripts switch without
//! other changes: the chosen line on stdout and exit 0; Escape → nothing,
//! exit 1; anything wrong (another menu open, bad usage, no answer) → a
//! message on stderr, exit 2. One code of its own: 3 when the daemon is
//! unreachable (not running), the only case where a caller may fall back
//! to another menu.

use alpheus_commun::launcher_ipc::{
    DndVerb, MAX_DMENU_BYTES, MAX_HEADER, MAX_LINES, MAX_PROMPT, Reply, Request, decode_reply, encode_request, split_lines,
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
Usage: alpheus-launch [--toggle | --close | --dmenu [--prompt TEXT] | --dnd on|off|toggle
                      | --clear-notifications | --help]

  (no option), --toggle   open the launcher, or close it if it is open
  --close                 close the launcher
  --dmenu                 show the lines read on standard input, print the chosen one
  -p, --prompt TEXT       with --dmenu: text shown before the input
  --dnd on|off|toggle     the bar's do-not-disturb (notifications module)
  --clear-notifications   empty the bar's notification history
  -h, --help              show this help

Exit codes: 0 done or chosen, 1 menu cancelled, 2 error (another menu open,
bad usage, no answer, the bar restarting mid-request, the notifications
module not in bar.toml), 3 alpheus-demon not running (launcher unreachable).

--dmenu waits for a choice with no timeout of its own: as long as the
person takes to pick a line, however long that is.
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Toggle,
    Close,
    Dmenu { prompt: Option<String> },
    Dnd(DndVerb),
    ClearNotifications,
    Help,
}

/// No argument or --toggle → Toggle; --close; --dmenu [--prompt TEXT | --prompt=TEXT | -p TEXT];
/// -h/--help. --prompt without --dmenu, unknown options, repeats → Err(usage message).
/// `args` excludes the program name.
pub fn parse_args(args: &[OsString]) -> Result<Invocation, String> {
    let mut mode: Option<Invocation> = None;
    let mut prompt: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let arg = utf8(arg)?;
        match arg {
            "--toggle" => choose(&mut mode, Invocation::Toggle)?,
            "--close" => choose(&mut mode, Invocation::Close)?,
            "--dmenu" => choose(&mut mode, Invocation::Dmenu { prompt: None })?,
            "--dnd" => {
                let value = it.next().ok_or_else(|| "--dnd needs on, off or toggle".to_string())?;
                let value = utf8(value)?;
                let verb = DndVerb::parse(value).ok_or_else(|| format!("--dnd takes on, off or toggle, not {value:?}"))?;
                choose(&mut mode, Invocation::Dnd(verb))?
            }
            "--clear-notifications" => choose(&mut mode, Invocation::ClearNotifications)?,
            "-h" | "--help" => choose(&mut mode, Invocation::Help)?,
            "-p" | "--prompt" => {
                let value = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
                set_prompt(&mut prompt, utf8(value)?)?;
            }
            _ => match arg.strip_prefix("--prompt=") {
                Some(value) => set_prompt(&mut prompt, value)?,
                None => return Err(format!("unknown option {arg:?}")),
            },
        }
    }
    match (mode, prompt) {
        (Some(Invocation::Dmenu { .. }), prompt) => Ok(Invocation::Dmenu { prompt }),
        (_, Some(_)) => Err("--prompt only goes with --dmenu".into()),
        (mode, None) => Ok(mode.unwrap_or(Invocation::Toggle)),
    }
}

fn utf8(arg: &OsString) -> Result<&str, String> {
    arg.to_str().ok_or_else(|| format!("argument is not valid UTF-8: {}", arg.to_string_lossy()))
}

fn choose(slot: &mut Option<Invocation>, i: Invocation) -> Result<(), String> {
    if slot.is_some() {
        return Err("only one of --toggle, --close, --dmenu, --dnd, --clear-notifications, --help".into());
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
        Request::Toggle | Request::Close | Request::Dnd(_) | Request::ClearNotifications => Some(CLIENT_REPLY_TIMEOUT),
    };
    stream.set_write_timeout(timeout).map_err(|e| ClientError::Protocol(format!("socket setup failed: {e}")))?;
    let deadline = timeout.map(|t| Instant::now() + t);
    let sent = stream.write_all(&encode_request(request));
    let mut answer = Vec::new();
    let read = read_reply(&mut stream, &mut answer, deadline);
    // A daemon that refuses early (request too large) answers before we finished sending.
    if let Ok(reply) = decode_reply(&answer) {
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
        Ok(()) => decode_reply(&answer).map_err(|e| ClientError::Protocol(format!("unexpected answer from alpheus-demon: {e}"))),
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
fn read_lines(stdin: &mut dyn Read) -> Result<Vec<String>, String> {
    let mut bytes = Vec::new();
    (&mut *stdin).take(MAX_DMENU_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| format!("cannot read standard input: {e}"))?;
    let too_large = || format!("standard input is larger than {} MiB", MAX_DMENU_BYTES >> 20);
    if bytes.len() > MAX_DMENU_BYTES {
        return Err(too_large());
    }
    let lines = split_lines(&String::from_utf8_lossy(&bytes));
    if lines.len() > MAX_LINES {
        return Err(format!("more than {MAX_LINES} lines on standard input"));
    }
    // U+FFFD takes 3 bytes where an invalid byte took 1: check the body as it will be sent.
    if lines.iter().map(|l| l.len() + 1).sum::<usize>().saturating_sub(1) > MAX_DMENU_BYTES {
        return Err(too_large());
    }
    Ok(lines)
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
    let request = match parse_args(args) {
        Err(m) => return fail(&format!("{m} (see --help)")),
        Ok(Invocation::Help) => {
            return if stdout.write_all(USAGE.as_bytes()).and_then(|()| stdout.flush()).is_ok() { EXIT_OK } else { EXIT_ERROR };
        }
        Ok(Invocation::Toggle) => Request::Toggle,
        Ok(Invocation::Close) => Request::Close,
        Ok(Invocation::Dnd(v)) => Request::Dnd(v),
        Ok(Invocation::ClearNotifications) => Request::ClearNotifications,
        Ok(Invocation::Dmenu { prompt }) => match read_lines(stdin) {
            Ok(lines) => Request::Dmenu { prompt, lines },
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
    match (&request, reply) {
        (Request::Dmenu { .. }, Reply::Chosen(line)) => {
            if writeln!(stdout, "{line}").and_then(|()| stdout.flush()).is_ok() {
                EXIT_OK
            } else {
                EXIT_ERROR
            }
        }
        (Request::Dmenu { .. }, Reply::Cancelled) => EXIT_CANCELLED,
        (Request::Toggle | Request::Close | Request::Dnd(_) | Request::ClearNotifications, Reply::Done) => EXIT_OK,
        (_, Reply::Busy) => fail("another menu is already open"),
        (_, Reply::Error(m)) => fail(&format!("alpheus-demon: {m}")),
        (_, other) => fail(&format!("unexpected answer from alpheus-demon: {other:?}")),
    }
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
        assert_eq!(parse_args(&args(&["--dmenu"])), Ok(Invocation::Dmenu { prompt: None }));
        for a in [
            &["--dmenu", "--prompt", "Pick: "][..],
            &["--prompt", "Pick: ", "--dmenu"],
            &["--dmenu", "--prompt=Pick: "],
            &["-p", "Pick: ", "--dmenu"],
        ] {
            assert_eq!(parse_args(&args(a)), Ok(Invocation::Dmenu { prompt: Some("Pick: ".into()) }), "{a:?}");
        }
        assert_eq!(parse_args(&args(&["--dmenu", "--prompt", ""])), Ok(Invocation::Dmenu { prompt: Some(String::new()) }));
        assert_eq!(parse_args(&args(&["--dmenu", "-p", "-x"])), Ok(Invocation::Dmenu { prompt: Some("-x".into()) }));
        assert_eq!(parse_args(&args(&["-h"])), Ok(Invocation::Help));
        assert_eq!(parse_args(&args(&["--help"])), Ok(Invocation::Help));
        assert_eq!(parse_args(&args(&["--dnd", "on"])), Ok(Invocation::Dnd(DndVerb::On)));
        assert_eq!(parse_args(&args(&["--dnd", "off"])), Ok(Invocation::Dnd(DndVerb::Off)));
        assert_eq!(parse_args(&args(&["--dnd", "toggle"])), Ok(Invocation::Dnd(DndVerb::Toggle)));
        assert_eq!(parse_args(&args(&["--clear-notifications"])), Ok(Invocation::ClearNotifications));
    }

    #[test]
    fn refused_invocations() {
        for (a, needle) in [
            (&["--prompt", "x"][..], "--prompt only goes with --dmenu"),
            (&["--close", "-p", "x"], "--prompt only goes with --dmenu"),
            (&["--dmenu", "--width", "40"], "unknown option"),
            (&["--dmenu", "--lines", "10"], "unknown option"),
            (&["--dmenu", "--index"], "unknown option"),
            (&["firefox"], "unknown option"),
            (&["--dmenu", "--dmenu"], "only one of"),
            (&["--toggle", "--close"], "only one of"),
            (&["--help", "--dmenu"], "only one of"),
            (&["--dmenu", "--prompt"], "needs a value"),
            (&["--dmenu", "-p", "a", "-p", "b"], "given twice"),
            (&["--dnd"], "needs on, off or toggle"),
            (&["--dnd", "maybe"], "on, off or toggle"),
            (&["--dnd", "on", "--close"], "only one of"),
            (&["--clear-notifications", "--dnd", "on"], "only one of"),
            (&["--dnd", "on", "-p", "x"], "--prompt only goes with --dmenu"),
            (&["--clear-notifications", "now"], "unknown option"),
        ] {
            let e = parse_args(&args(a)).unwrap_err();
            assert!(e.contains(needle), "{a:?}: {e}");
        }
        let long = "x".repeat(MAX_PROMPT + 1);
        assert!(parse_args(&args(&["--dmenu", "-p", &long])).unwrap_err().contains("prompt longer"));
        let bad = vec![OsString::from("--dmenu"), OsString::from("-p"), OsString::from_vec(vec![0xff])];
        assert!(parse_args(&bad).unwrap_err().contains("UTF-8"));
    }
}
