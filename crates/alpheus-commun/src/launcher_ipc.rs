//! The launcher's wire protocol, shared by `alpheus-launch` (client) and
//! `alpheus-demon` (server) over `$XDG_RUNTIME_DIR/alpheus-launcher.sock`.
//! Std only, no serde: one header line, then (dmenu only) two
//! length-prefixed blobs. Every size is bounded and checked as soon as it is
//! known, so a hostile or broken client can never make the daemon buffer
//! more than `MAX_HEADER + MAX_PROMPT + MAX_DMENU_BYTES` bytes.

use crate::displays::wire::MAX_OUTPUTS_BODY;
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SOCKET_NAME: &str = "alpheus-launcher.sock";
pub const MAGIC: &str = "alpheus-launcher/1";
/// Header line, magic included, '\n' excluded.
pub const MAX_HEADER: usize = 256;
pub const MAX_PROMPT: usize = 1024;
/// Whole dmenu body (lines joined by '\n').
pub const MAX_DMENU_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_LINES: usize = 100_000;
/// `error` reply text, one line.
pub const MAX_ERROR: usize = 512;

/// How long a client waits for the answer to every request but dmenu (which waits for the person).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// `outputs-try` is answered once the compositor has answered the configuration (the daemon's
/// watchdog is 5 s).
pub const OUTPUTS_TRY_TIMEOUT: Duration = Duration::from_secs(7);
/// `shield status` runs the blocked-devices read (its own bound is 2 s) before it answers.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(4);
/// The `shield status` text, in bytes.
pub const MAX_SHIELD_BODY: usize = 16 * 1024;

pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SOCKET_NAME)
}

/// The shortest and longest timed do-not-disturb, in seconds (1 minute, 24 hours).
pub const DND_MIN_SECS: u32 = 60;
pub const DND_MAX_SECS: u32 = 86_400;

/// When a timed do-not-disturb ends: after this many seconds, or at this Unix time (the client
/// resolves a local `HH:MM` itself, so the daemon needs no time zone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndEnd {
    For(u32),
    Until(u64),
}

/// `--dnd`'s word: the bar's do-not-disturb (notifications spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndVerb {
    On,
    Off,
    Toggle,
}

impl DndVerb {
    pub const ALL: [DndVerb; 3] = [DndVerb::On, DndVerb::Off, DndVerb::Toggle];

    pub fn word(self) -> &'static str {
        match self {
            DndVerb::On => "on",
            DndVerb::Off => "off",
            DndVerb::Toggle => "toggle",
        }
    }

    pub fn parse(s: &str) -> Option<DndVerb> {
        DndVerb::ALL.into_iter().find(|v| v.word() == s)
    }
}

/// `--osd`'s word: what the on-screen display reads, then shows (OSD spec § 2). Volume and the
/// microphone have none: the bar follows them on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsdVerb {
    Brightness,
}

impl OsdVerb {
    pub const ALL: [OsdVerb; 1] = [OsdVerb::Brightness];

    pub fn word(self) -> &'static str {
        match self {
            OsdVerb::Brightness => "brightness",
        }
    }

    pub fn parse(s: &str) -> Option<OsdVerb> {
        OsdVerb::ALL.into_iter().find(|v| v.word() == s)
    }
}

/// `--shield`'s word (shield spec § 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShieldVerb {
    On,
    Off,
    Toggle,
    /// Prints the state, and the blocked USB devices while on.
    Status,
    /// Opens the picker of the blocked USB devices in the launcher's box.
    Allow,
}

impl ShieldVerb {
    pub const ALL: [ShieldVerb; 5] = [ShieldVerb::On, ShieldVerb::Off, ShieldVerb::Toggle, ShieldVerb::Status, ShieldVerb::Allow];

    pub fn word(self) -> &'static str {
        match self {
            ShieldVerb::On => "on",
            ShieldVerb::Off => "off",
            ShieldVerb::Toggle => "toggle",
            ShieldVerb::Status => "status",
            ShieldVerb::Allow => "allow",
        }
    }

    pub fn parse(s: &str) -> Option<ShieldVerb> {
        ShieldVerb::ALL.into_iter().find(|v| v.word() == s)
    }
}

/// `--quick`'s optional word (quick spec § 1, quick Wi-Fi spec § 1, quick Bluetooth spec § 1): where
/// the panel opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickVerb {
    /// `--quick`: the first row.
    Top,
    /// `--quick sound`: the volume row.
    Sound,
    /// `--quick wifi`: the Wi-Fi detail, once the Wi-Fi state is read.
    Wifi,
    /// `--quick bluetooth`: the Bluetooth detail.
    Bluetooth,
    /// `--quick vpn`: the VPN detail.
    Vpn,
}

impl QuickVerb {
    pub const ALL: [QuickVerb; 5] = [QuickVerb::Top, QuickVerb::Sound, QuickVerb::Wifi, QuickVerb::Bluetooth, QuickVerb::Vpn];

    /// The word after `quick` (wire) and `--quick` (command line); None for the top.
    pub fn word(self) -> Option<&'static str> {
        match self {
            QuickVerb::Top => None,
            QuickVerb::Sound => Some("sound"),
            QuickVerb::Wifi => Some("wifi"),
            QuickVerb::Bluetooth => Some("bluetooth"),
            QuickVerb::Vpn => Some("vpn"),
        }
    }

    /// A word after `quick`: "sound", "wifi", "bluetooth" or "vpn".
    pub fn parse(s: &str) -> Option<QuickVerb> {
        QuickVerb::ALL.into_iter().find(|v| v.word() == Some(s))
    }
}

/// The shortest and the longest countdown `--timer start` asks for, in seconds.
pub const TIMER_MIN_SECS: u32 = 1;
pub const TIMER_MAX_SECS: u32 = 86_400;

/// `--timer`'s verb (timer spec § 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerVerb {
    /// A countdown of this many seconds (`TIMER_MIN_SECS..=TIMER_MAX_SECS`).
    Start(u32),
    Pomodoro,
    Stopwatch,
    Pause,
    Resume,
    Toggle,
    Stop,
    Skip,
    /// Opens or closes the popup.
    Popup,
}

impl TimerVerb {
    /// The word after `timer` (wire) and `--timer` (command line); `start` for a countdown.
    pub fn word(self) -> &'static str {
        match self {
            TimerVerb::Start(_) => "start",
            TimerVerb::Pomodoro => "pomodoro",
            TimerVerb::Stopwatch => "stopwatch",
            TimerVerb::Pause => "pause",
            TimerVerb::Resume => "resume",
            TimerVerb::Toggle => "toggle",
            TimerVerb::Stop => "stop",
            TimerVerb::Skip => "skip",
            TimerVerb::Popup => "popup",
        }
    }

    /// Every word but `start`, which carries a duration.
    pub fn parse_word(s: &str) -> Option<TimerVerb> {
        [
            TimerVerb::Pomodoro,
            TimerVerb::Stopwatch,
            TimerVerb::Pause,
            TimerVerb::Resume,
            TimerVerb::Toggle,
            TimerVerb::Stop,
            TimerVerb::Skip,
            TimerVerb::Popup,
        ]
        .into_iter()
        .find(|v| v.word() == s)
    }
}

/// `--dmenu`'s options that change what the daemon does (the dmenu inventory, 2026-10-09):
/// optional trailing words of the dmenu header, so a request without them reads as before.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DmenuFlags {
    /// `--index`: answer the chosen line's position on standard input (`Reply::Index`), not its text.
    pub index: bool,
    /// `--password`: the input drawn as one `•` per character, no list, the text never logged
    /// nor kept.
    pub password: bool,
    /// `--visible N`: with the query empty, only the first N lines are listed; typing searches
    /// them all (sources spec § 4). None: every line, as before.
    pub visible: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Toggle,
    Close,
    /// Opens the launcher as a calculator with no scan (a second one, or a toggle, closes it).
    Calc,
    /// Opens the window switcher (the previous window selected), or steps to the next one when it is open.
    Windows,
    /// Toggles the quick notepad (quick notes spec): answered Done (the pad is no module: it is
    /// always available). An older daemon refuses it as an unknown request.
    Notes,
    /// Opens the clipboard history in the launcher (clipboard spec). An older daemon refuses it as
    /// an unknown request.
    Clipboard,
    /// Empties the clipboard history, pins kept (clipboard spec): answered Done, or an error when
    /// the history is off.
    ClipboardClear,
    Dmenu {
        prompt: Option<String>,
        lines: Vec<String>,
        flags: DmenuFlags,
    },
    /// The notifications module's do-not-disturb (notifications spec): answered Done.
    Dnd(DndVerb),
    /// Do-not-disturb on, ending at `DndEnd` (small features spec): answered Done. An older daemon
    /// refuses it as an unknown `dnd` request.
    DndOn(DndEnd),
    /// Empties the notification history (notifications spec): answered Done.
    ClearNotifications,
    /// Reads the failed-unit counts again now (diagnostics spec § 6): answered Done, also with the
    /// module off (nothing read). An older daemon refuses it as an unknown request.
    FailedRefresh,
    /// The OSD (OSD spec § 2): read the backlight once and show it. Answered Done, also with the
    /// module off or no backlight (nothing shown).
    Osd(OsdVerb),
    /// The quick settings panel (quick spec § 1): toggled, at the volume row with `sound`. Answered
    /// Done, also with the module off (nothing opens).
    Quick(QuickVerb),
    /// The focus timer (timer spec § 7): answered Done, or an error when the module is not in `modules`.
    /// An older daemon refuses it as an unknown `timer` request.
    Timer(TimerVerb),
    /// The Displays tab (displays protocol spec): the snapshot of the heads, answered
    /// `Reply::Outputs`.
    Outputs,
    /// Tries a desired state (`displays::wire::encode_desired`): answered Done once applied, Error
    /// when refused.
    OutputsTry(String),
    /// Keeps the trial that is open: answered Done.
    OutputsKeep,
    /// Reverts the trial that is open: answered Done.
    OutputsRevert,
    /// Shield mode (shield spec § 9): answered Done, `Reply::Shield` for `status`, Cancelled for an
    /// escaped picker. An older daemon refuses it as an unknown request.
    Shield(ShieldVerb),
}

impl Request {
    /// The word a daemon older than the displays verbs refuses this request by, if it is one of
    /// them: the header decoder of such a daemon reports the verb as an unknown request.
    pub fn displays_word(&self) -> Option<&'static str> {
        match self {
            Request::Outputs => Some("outputs"),
            Request::OutputsTry(_) => Some("outputs-try"),
            Request::OutputsKeep => Some("outputs-keep"),
            Request::OutputsRevert => Some("outputs-revert"),
            _ => None,
        }
    }

    /// Whether `error` is what a daemon from before the displays verbs answers this request:
    /// "the daemon is too old for displays".
    pub fn refused_as_unknown_displays_verb(&self, error: &str) -> bool {
        self.displays_word().is_some_and(|w| error == format!("malformed request: unknown request \"{w}\""))
    }

    /// How long a client waits for the reply: None for dmenu, which waits for the person.
    pub fn reply_timeout(&self) -> Option<Duration> {
        match self {
            Request::Dmenu { .. } => None,
            Request::OutputsTry(_) => Some(OUTPUTS_TRY_TIMEOUT),
            // The picker waits for the person, as dmenu does.
            Request::Shield(ShieldVerb::Allow) => None,
            Request::Shield(ShieldVerb::Status) => Some(STATUS_TIMEOUT),
            _ => Some(REPLY_TIMEOUT),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// toggle / close / dnd / clear-notifications / osd / quick handled.
    Done,
    /// dmenu: the chosen line, or the typed text when nothing matched.
    Chosen(String),
    /// dmenu with `index`: the chosen line's 0-based position in the order the client sent the
    /// lines; None for typed text that matched no line (the client prints -1, as fuzzel does).
    Index(Option<usize>),
    /// dmenu: Escape, toggle, close, focus lost, output gone.
    Cancelled,
    /// dmenu refused: another dmenu is already open.
    Busy,
    Error(String),
    /// `outputs`: the encoded snapshot (`displays::wire::encode_snapshot`).
    Outputs(String),
    /// `shield status`: the text to print (`MAX_SHIELD_BODY` bytes at most).
    Shield(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcError {
    TooLarge(&'static str),
    Malformed(String),
    /// The peer closed before a whole message arrived.
    Truncated,
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::TooLarge(what) => write!(f, "{what} too large"),
            IpcError::Malformed(m) => write!(f, "malformed message: {m}"),
            IpcError::Truncated => write!(f, "message cut short"),
        }
    }
}

/// Wire format (all text UTF-8):
///   "alpheus-launcher/1 toggle\n" | "alpheus-launcher/1 close\n"
///   "alpheus-launcher/1 dnd on|off|toggle\n" | "alpheus-launcher/1 clear-notifications\n"
///   "alpheus-launcher/1 failed-refresh\n"
///   "alpheus-launcher/1 osd brightness\n"
///   "alpheus-launcher/1 quick\n" | "alpheus-launcher/1 quick sound\n"
///   "alpheus-launcher/1 timer start <secs>\n" | "alpheus-launcher/1 timer pomodoro|stopwatch|pause|resume|toggle|stop|skip|popup\n"
///   "alpheus-launcher/1 clipboard\n" | "alpheus-launcher/1 clipboard-clear\n"
///   "alpheus-launcher/1 shield on|off|toggle|status|allow\n"
///   "alpheus-launcher/1 outputs\n" | "alpheus-launcher/1 outputs-keep\n" | "alpheus-launcher/1 outputs-revert\n"
///   "alpheus-launcher/1 outputs-try <body_len>\n" <body bytes>
///   "alpheus-launcher/1 dmenu <prompt_len|-> <body_len>[ index][ password][ visible <N>]\n" <prompt bytes> <body bytes>
/// body = lines joined by '\n' (no trailing '\n'). The flag words (`DmenuFlags`) are optional,
/// each at most once and in this order: a client that sends none is read as before, so the
/// header stays `alpheus-launcher/1`; a daemon older than the flags refuses them as malformed
/// (an `error` reply, exit 2), never misreads them. Lines are expected not to
/// contain '\n' themselves (`split_lines` guarantees it), but this is not
/// enforced here: a line that does is split consistently with decoding, not
/// refused, because the wire format has no other way to tell "one line
/// holding a newline" from "two lines" — the same rule the decoder already
/// applies (and the decoder also drops the empty lines this can produce,
/// the dmenu rule). Sizes are the caller's to check against the MAX_*
/// limits (the decoder refuses what exceeds them).
pub fn encode_request(r: &Request) -> Vec<u8> {
    match r {
        Request::Toggle => format!("{MAGIC} toggle\n").into_bytes(),
        Request::Close => format!("{MAGIC} close\n").into_bytes(),
        Request::Calc => format!("{MAGIC} calc\n").into_bytes(),
        Request::Windows => format!("{MAGIC} windows\n").into_bytes(),
        Request::Notes => format!("{MAGIC} notes\n").into_bytes(),
        Request::Clipboard => format!("{MAGIC} clipboard\n").into_bytes(),
        Request::ClipboardClear => format!("{MAGIC} clipboard-clear\n").into_bytes(),
        Request::Dnd(v) => format!("{MAGIC} dnd {}\n", v.word()).into_bytes(),
        Request::DndOn(DndEnd::For(secs)) => format!("{MAGIC} dnd on for {secs}\n").into_bytes(),
        Request::DndOn(DndEnd::Until(at)) => format!("{MAGIC} dnd on until {at}\n").into_bytes(),
        Request::ClearNotifications => format!("{MAGIC} clear-notifications\n").into_bytes(),
        Request::FailedRefresh => format!("{MAGIC} failed-refresh\n").into_bytes(),
        Request::Osd(v) => format!("{MAGIC} osd {}\n", v.word()).into_bytes(),
        Request::Quick(v) => match v.word() {
            Some(w) => format!("{MAGIC} quick {w}\n").into_bytes(),
            None => format!("{MAGIC} quick\n").into_bytes(),
        },
        Request::Timer(TimerVerb::Start(secs)) => format!("{MAGIC} timer start {secs}\n").into_bytes(),
        Request::Timer(v) => format!("{MAGIC} timer {}\n", v.word()).into_bytes(),
        Request::Shield(v) => format!("{MAGIC} shield {}\n", v.word()).into_bytes(),
        Request::Outputs => format!("{MAGIC} outputs\n").into_bytes(),
        Request::OutputsKeep => format!("{MAGIC} outputs-keep\n").into_bytes(),
        Request::OutputsRevert => format!("{MAGIC} outputs-revert\n").into_bytes(),
        Request::OutputsTry(body) => {
            let mut out = format!("{MAGIC} outputs-try {}\n", body.len()).into_bytes();
            out.extend_from_slice(body.as_bytes());
            out
        }
        Request::Dmenu { prompt, lines, flags } => {
            let body = lines.join("\n");
            let prompt_len = prompt.as_ref().map_or_else(|| "-".to_string(), |p| p.len().to_string());
            let index = if flags.index { " index" } else { "" };
            let password = if flags.password { " password" } else { "" };
            let visible = flags.visible.map_or_else(String::new, |n| format!(" visible {n}"));
            let mut out = format!("{MAGIC} dmenu {prompt_len} {}{index}{password}{visible}\n", body.len()).into_bytes();
            if let Some(p) = prompt {
                out.extend_from_slice(p.as_bytes());
            }
            out.extend_from_slice(body.as_bytes());
            out
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Header {
    Toggle,
    Close,
    Calc,
    Windows,
    Notes,
    Clipboard,
    ClipboardClear,
    Dmenu { prompt: Option<usize>, body: usize, flags: DmenuFlags },
    Dnd(DndVerb),
    DndOn(DndEnd),
    ClearNotifications,
    FailedRefresh,
    Osd(OsdVerb),
    Quick(QuickVerb),
    Timer(TimerVerb),
    Outputs,
    OutputsTry { body: usize },
    OutputsKeep,
    OutputsRevert,
    Shield(ShieldVerb),
}

/// Incremental decoder (the daemon reads without blocking).
#[derive(Debug, Default)]
pub struct RequestDecoder {
    buf: Vec<u8>,
    header: Option<Header>,
    /// '\n' seen in the dmenu body so far (lines = newlines + 1).
    newlines: usize,
    done: bool,
    /// Set to the first error `feed` ever returned; once set, every later
    /// `feed` call returns it again without touching any other state.
    poisoned: Option<IpcError>,
}

impl RequestDecoder {
    pub fn new() -> RequestDecoder {
        RequestDecoder::default()
    }

    /// Ok(Some) once a whole request arrived (extra bytes after it → Malformed);
    /// Ok(None): need more. Limits checked as soon as the header is known; the
    /// line count, which only the body reveals, as the body arrives.
    ///
    /// **After an `Err`, the decoder is poisoned and must be discarded** (or
    /// treated as permanently failed): every later `feed` call returns the
    /// same error again and never yields a request, even if the bytes that
    /// follow would otherwise complete a valid one. A connection that sends
    /// something malformed does not get a second chance mid-stream.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Option<Request>, IpcError> {
        if let Some(e) = &self.poisoned {
            return Err(e.clone());
        }
        match self.feed_inner(bytes) {
            Err(e) => {
                self.poisoned = Some(e.clone());
                Err(e)
            }
            ok => ok,
        }
    }

    fn feed_inner(&mut self, bytes: &[u8]) -> Result<Option<Request>, IpcError> {
        if self.done {
            return if bytes.is_empty() { Ok(None) } else { Err(IpcError::Malformed("bytes after the request".into())) };
        }
        let mut bytes = bytes;
        let header = match self.header {
            Some(h) => h,
            None => {
                // Never buffer more than one header line, '\n' included.
                let room = (MAX_HEADER + 1).saturating_sub(self.buf.len());
                let window = &bytes[..bytes.len().min(room)];
                let Some(end) = window.iter().position(|&b| b == b'\n') else {
                    self.buf.extend_from_slice(window);
                    return if self.buf.len() > MAX_HEADER { Err(IpcError::TooLarge("header")) } else { Ok(None) };
                };
                self.buf.extend_from_slice(&window[..end]);
                let h = parse_header(&self.buf)?;
                self.buf = Vec::new();
                if let Header::Dmenu { prompt, body, .. } = h {
                    // Bounded by the header check; the pages are only touched as the body arrives.
                    self.buf.reserve_exact(prompt.unwrap_or(0) + body);
                }
                if let Header::OutputsTry { body } = h {
                    self.buf.reserve_exact(body);
                }
                self.header = Some(h);
                bytes = &bytes[end + 1..];
                h
            }
        };
        match header {
            Header::Dmenu { prompt, body, flags } => {
                let prompt_len = prompt.unwrap_or(0);
                let total = prompt_len + body;
                if self.buf.len() + bytes.len() > total {
                    return Err(IpcError::Malformed("bytes after the request".into()));
                }
                let start = self.buf.len();
                self.newlines += bytes.iter().enumerate().filter(|&(i, &b)| b == b'\n' && start + i >= prompt_len).count();
                if self.newlines >= MAX_LINES {
                    return Err(IpcError::TooLarge("lines"));
                }
                self.buf.extend_from_slice(bytes);
                if self.buf.len() < total {
                    return Ok(None);
                }
                self.done = true;
                // One buffer of exactly `total` bytes: the prompt is copied out (≤ MAX_PROMPT),
                // the body becomes the String in place (no second 4 MiB copy).
                let mut data = std::mem::take(&mut self.buf);
                let prompt = match prompt {
                    Some(n) => Some(text(data[..n].to_vec(), "prompt")?),
                    None => None,
                };
                data.drain(..prompt_len);
                let body = text(data, "dmenu input")?;
                // The dmenu rule: a blank line carries nothing, so it is dropped
                // rather than turned into an empty entry (matches fuzzel/wofi).
                let lines = body.split('\n').filter(|l| !l.is_empty()).map(str::to_string).collect();
                Ok(Some(Request::Dmenu { prompt, lines, flags }))
            }
            Header::OutputsTry { body } => {
                if self.buf.len() + bytes.len() > body {
                    return Err(IpcError::Malformed("bytes after the request".into()));
                }
                self.buf.extend_from_slice(bytes);
                if self.buf.len() < body {
                    return Ok(None);
                }
                self.done = true;
                let data = std::mem::take(&mut self.buf);
                Ok(Some(Request::OutputsTry(text(data, "outputs body")?)))
            }
            bare => {
                if !bytes.is_empty() {
                    return Err(IpcError::Malformed("bytes after the request".into()));
                }
                self.done = true;
                Ok(bare_request(bare))
            }
        }
    }

    /// Bytes buffered so far: test-only (the memory-guard tests below),
    /// never read by the daemon itself.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

/// The request a header with no body stands for; None for dmenu, which has one.
fn bare_request(h: Header) -> Option<Request> {
    match h {
        Header::Toggle => Some(Request::Toggle),
        Header::Close => Some(Request::Close),
        Header::Calc => Some(Request::Calc),
        Header::Windows => Some(Request::Windows),
        Header::Notes => Some(Request::Notes),
        Header::Clipboard => Some(Request::Clipboard),
        Header::ClipboardClear => Some(Request::ClipboardClear),
        Header::Dnd(v) => Some(Request::Dnd(v)),
        Header::DndOn(end) => Some(Request::DndOn(end)),
        Header::ClearNotifications => Some(Request::ClearNotifications),
        Header::FailedRefresh => Some(Request::FailedRefresh),
        Header::Osd(v) => Some(Request::Osd(v)),
        Header::Quick(v) => Some(Request::Quick(v)),
        Header::Timer(v) => Some(Request::Timer(v)),
        Header::Outputs => Some(Request::Outputs),
        Header::OutputsKeep => Some(Request::OutputsKeep),
        Header::OutputsRevert => Some(Request::OutputsRevert),
        Header::Shield(v) => Some(Request::Shield(v)),
        Header::Dmenu { .. } | Header::OutputsTry { .. } => None,
    }
}

fn parse_header(line: &[u8]) -> Result<Header, IpcError> {
    let line = std::str::from_utf8(line).map_err(|_| IpcError::Malformed("header is not UTF-8".into()))?;
    let mut words = line.split(' ');
    if words.next() != Some(MAGIC) {
        return Err(IpcError::Malformed(format!("not an alpheus launcher request: {:?}", cut(line))));
    }
    match words.collect::<Vec<_>>().as_slice() {
        ["toggle"] => Ok(Header::Toggle),
        ["close"] => Ok(Header::Close),
        ["calc"] => Ok(Header::Calc),
        ["windows"] => Ok(Header::Windows),
        ["notes"] => Ok(Header::Notes),
        ["clipboard"] => Ok(Header::Clipboard),
        ["clipboard-clear"] => Ok(Header::ClipboardClear),
        ["dnd", word] => {
            DndVerb::parse(word).map(Header::Dnd).ok_or_else(|| IpcError::Malformed(format!("unknown do-not-disturb word {:?}", cut(word))))
        }
        ["dnd", "on", "for", secs] => {
            let secs = u32::try_from(number(secs, "do-not-disturb seconds")?)
                .map_err(|_| IpcError::Malformed(format!("do-not-disturb seconds out of range: {:?}", cut(secs))))?;
            Ok(Header::DndOn(DndEnd::For(secs)))
        }
        ["dnd", "on", "until", at] => Ok(Header::DndOn(DndEnd::Until(number(at, "do-not-disturb end")? as u64))),
        ["clear-notifications"] => Ok(Header::ClearNotifications),
        ["failed-refresh"] => Ok(Header::FailedRefresh),
        ["osd", word] => {
            OsdVerb::parse(word).map(Header::Osd).ok_or_else(|| IpcError::Malformed(format!("unknown osd word {:?}", cut(word))))
        }
        ["quick"] => Ok(Header::Quick(QuickVerb::Top)),
        ["quick", word] => {
            QuickVerb::parse(word).map(Header::Quick).ok_or_else(|| IpcError::Malformed(format!("unknown quick word {:?}", cut(word))))
        }
        ["timer", "start", secs] => {
            let secs = number(secs, "timer seconds")?;
            u32::try_from(secs)
                .ok()
                .filter(|s| (TIMER_MIN_SECS..=TIMER_MAX_SECS).contains(s))
                .map(|s| Header::Timer(TimerVerb::Start(s)))
                .ok_or_else(|| IpcError::Malformed(format!("timer seconds out of range: {secs}")))
        }
        ["timer", word] => {
            TimerVerb::parse_word(word).map(Header::Timer).ok_or_else(|| IpcError::Malformed(format!("unknown timer word {:?}", cut(word))))
        }
        ["shield", word] => {
            ShieldVerb::parse(word).map(Header::Shield).ok_or_else(|| IpcError::Malformed(format!("unknown shield word {:?}", cut(word))))
        }
        ["outputs"] => Ok(Header::Outputs),
        ["outputs-keep"] => Ok(Header::OutputsKeep),
        ["outputs-revert"] => Ok(Header::OutputsRevert),
        ["outputs-try", len] => {
            let body = number(len, "outputs body length")?;
            if body > MAX_OUTPUTS_BODY {
                return Err(IpcError::TooLarge("outputs body"));
            }
            Ok(Header::OutputsTry { body })
        }
        ["dmenu", prompt, body, flags @ ..] => {
            let prompt = if *prompt == "-" { None } else { Some(number(prompt, "prompt length")?) };
            let body = number(body, "dmenu length")?;
            if prompt.is_some_and(|n| n > MAX_PROMPT) {
                return Err(IpcError::TooLarge("prompt"));
            }
            if body > MAX_DMENU_BYTES {
                return Err(IpcError::TooLarge("dmenu input"));
            }
            Ok(Header::Dmenu { prompt, body, flags: dmenu_flags(flags)? })
        }
        [verb, ..] => Err(IpcError::Malformed(format!("unknown request {:?}", cut(verb)))),
        [] => Err(IpcError::Malformed("empty request".into())),
    }
}

/// The dmenu header's optional flag words: each at most once, in the documented order.
fn dmenu_flags(words: &[&str]) -> Result<DmenuFlags, IpcError> {
    let mut flags = DmenuFlags::default();
    let mut rest = words;
    if let ["index", tail @ ..] = rest {
        flags.index = true;
        rest = tail;
    }
    if let ["password", tail @ ..] = rest {
        flags.password = true;
        rest = tail;
    }
    if let ["visible", n, tail @ ..] = rest {
        flags.visible = Some(number(n, "visible count")?);
        rest = tail;
    }
    match rest {
        [] => Ok(flags),
        [word, ..] => Err(IpcError::Malformed(format!("unknown dmenu flag {:?}", cut(word)))),
    }
}

fn number(s: &str, what: &str) -> Result<usize, IpcError> {
    if s.is_empty() || s.len() > 12 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(IpcError::Malformed(format!("{what} is not a number: {:?}", cut(s))));
    }
    s.parse().map_err(|_| IpcError::Malformed(format!("{what} is not a number: {:?}", cut(s))))
}

fn text(bytes: Vec<u8>, what: &str) -> Result<String, IpcError> {
    String::from_utf8(bytes).map_err(|_| IpcError::Malformed(format!("{what} is not UTF-8")))
}

/// At most 64 characters of peer-supplied text in an error message.
fn cut(s: &str) -> String {
    s.chars().take(64).collect()
}

/// "done\n" | "chosen <len>\n<bytes>" | "outputs <len>\n<bytes>" | "shield <len>\n<bytes>" | "index <n>\n" | "index -\n" | "cancelled\n" | "busy\n" | "error <text>\n"
pub fn encode_reply(r: &Reply) -> Vec<u8> {
    match r {
        Reply::Done => b"done\n".to_vec(),
        Reply::Chosen(s) => {
            let mut out = format!("chosen {}\n", s.len()).into_bytes();
            out.extend_from_slice(s.as_bytes());
            out
        }
        Reply::Cancelled => b"cancelled\n".to_vec(),
        Reply::Busy => b"busy\n".to_vec(),
        Reply::Index(Some(i)) => format!("index {i}\n").into_bytes(),
        Reply::Index(None) => b"index -\n".to_vec(),
        Reply::Error(t) => format!("error {}\n", one_line(t)).into_bytes(),
        Reply::Outputs(s) => {
            let mut out = format!("outputs {}\n", s.len()).into_bytes();
            out.extend_from_slice(s.as_bytes());
            out
        }
        Reply::Shield(s) => {
            let mut out = format!("shield {}\n", s.len()).into_bytes();
            out.extend_from_slice(s.as_bytes());
            out
        }
    }
}

/// Control characters become spaces; cut to MAX_ERROR bytes on a char boundary.
fn one_line(t: &str) -> String {
    let mut out = String::new();
    for c in t.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        if out.len() + c.len_utf8() > MAX_ERROR {
            break;
        }
        out.push(c);
    }
    out
}

pub fn decode_reply(bytes: &[u8]) -> Result<Reply, IpcError> {
    let Some(end) = bytes.iter().position(|&b| b == b'\n') else {
        return Err(IpcError::Truncated);
    };
    let line = std::str::from_utf8(&bytes[..end]).map_err(|_| IpcError::Malformed("reply is not UTF-8".into()))?;
    let rest = &bytes[end + 1..];
    let alone = |r: Reply| if rest.is_empty() { Ok(r) } else { Err(IpcError::Malformed("bytes after the reply".into())) };
    match line.split_once(' ') {
        None => match line {
            "done" => alone(Reply::Done),
            "cancelled" => alone(Reply::Cancelled),
            "busy" => alone(Reply::Busy),
            other => Err(IpcError::Malformed(format!("unknown reply {:?}", cut(other)))),
        },
        Some(("error", t)) => {
            if t.len() > MAX_ERROR {
                return Err(IpcError::TooLarge("error text"));
            }
            alone(Reply::Error(t.to_string()))
        }
        Some(("index", "-")) => alone(Reply::Index(None)),
        Some(("index", n)) => {
            let n = number(n, "index")?;
            if n >= MAX_LINES {
                return Err(IpcError::TooLarge("index"));
            }
            alone(Reply::Index(Some(n)))
        }
        Some(("outputs", len)) => {
            let n = number(len, "outputs length")?;
            if n > MAX_OUTPUTS_BODY {
                return Err(IpcError::TooLarge("outputs body"));
            }
            match rest.len().cmp(&n) {
                std::cmp::Ordering::Less => Err(IpcError::Truncated),
                std::cmp::Ordering::Greater => Err(IpcError::Malformed("bytes after the reply".into())),
                std::cmp::Ordering::Equal => Ok(Reply::Outputs(text(rest.to_vec(), "outputs body")?)),
            }
        }
        Some(("shield", len)) => {
            let n = number(len, "shield length")?;
            if n > MAX_SHIELD_BODY {
                return Err(IpcError::TooLarge("shield body"));
            }
            match rest.len().cmp(&n) {
                std::cmp::Ordering::Less => Err(IpcError::Truncated),
                std::cmp::Ordering::Greater => Err(IpcError::Malformed("bytes after the reply".into())),
                std::cmp::Ordering::Equal => Ok(Reply::Shield(text(rest.to_vec(), "shield body")?)),
            }
        }
        Some(("chosen", len)) => {
            let n = number(len, "chosen length")?;
            if n > MAX_DMENU_BYTES {
                return Err(IpcError::TooLarge("chosen line"));
            }
            match rest.len().cmp(&n) {
                std::cmp::Ordering::Less => Err(IpcError::Truncated),
                std::cmp::Ordering::Greater => Err(IpcError::Malformed("bytes after the reply".into())),
                std::cmp::Ordering::Equal => Ok(Reply::Chosen(text(rest.to_vec(), "chosen line")?)),
            }
        }
        Some((verb, _)) => Err(IpcError::Malformed(format!("unknown reply {:?}", cut(verb)))),
    }
}

/// Connects, sends `request`, waits for the reply: one total deadline of `timeout`. A missing
/// socket or a refused connection keeps its kind (`NotFound`, `ConnectionRefused`: the daemon is
/// not running), a silent daemon is `TimedOut`, anything unreadable `InvalidData`; the message of
/// each is the one the client prints.
pub fn exchange(socket: &Path, request: &Request, timeout: Duration) -> io::Result<Reply> {
    exchange_waiting(socket, request, Some(timeout))
}

/// `exchange` with the wait chosen by the caller: None waits for the person, as long as it takes
/// (`Request::reply_timeout`: dmenu). The write side is never shut down after sending: the daemon
/// reads a hang-up as "the client went away" and would close a waiting dmenu. The total deadline is
/// not a per-read allowance that a daemon trickling one byte at a time could restart forever.
pub fn exchange_waiting(socket: &Path, request: &Request, timeout: Option<Duration>) -> io::Result<Reply> {
    let mut stream = UnixStream::connect(socket).map_err(|e| match e.kind() {
        kind @ (ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            io::Error::new(kind, format!("alpheus-demon is not running (no launcher socket at {})", socket.display()))
        }
        kind => io::Error::new(kind, format!("cannot reach the launcher socket {}: {e}", socket.display())),
    })?;
    stream.set_write_timeout(timeout).map_err(|e| io::Error::new(e.kind(), format!("socket setup failed: {e}")))?;
    let deadline = timeout.map(|t| Instant::now() + t);
    let sent = stream.write_all(&encode_request(request));
    let mut answer = Vec::new();
    let read = read_reply(&mut stream, &mut answer, deadline);
    let reply = answer_of(sent, read, &answer, timeout);
    // The reply's bytes held the typed password: overwritten before they are freed.
    if matches!(request, Request::Dmenu { flags, .. } if flags.password) {
        zero(&mut answer);
    }
    reply
}

/// The reply in `answer`, or why there is none.
fn answer_of(sent: io::Result<()>, read: io::Result<()>, answer: &[u8], timeout: Option<Duration>) -> io::Result<Reply> {
    // A daemon that refuses early (request too large) answers before we finished sending.
    if let Ok(reply) = decode_reply(answer) {
        return Ok(reply);
    }
    if let Err(e) = sent {
        return Err(io::Error::new(e.kind(), format!("cannot send the request: {e}")));
    }
    match read {
        Err(e) if e.kind() == ErrorKind::TimedOut => {
            let secs = timeout.map_or(0, |t| t.as_secs());
            Err(io::Error::new(ErrorKind::TimedOut, format!("no answer from alpheus-demon within {secs} s")))
        }
        Err(e) => Err(io::Error::new(e.kind(), format!("cannot read the answer: {e}"))),
        Ok(()) => {
            decode_reply(answer).map_err(|e| io::Error::new(ErrorKind::InvalidData, format!("unexpected answer from alpheus-demon: {e}")))
        }
    }
}

/// Reads the reply into `answer`, bounded to `MAX_HEADER + MAX_DMENU_BYTES + 1` bytes (enough to
/// tell an oversized or malformed answer apart without ever reading an unbounded stream). With a
/// `deadline`, the whole loop is given up on as a single total budget: the remaining time is
/// recomputed before every read.
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

/// Writes zeros over all of `v`'s allocation, its spare capacity included (`len` becomes the
/// capacity), so a secret's bytes do not outlive it in freed memory. Safe code only, no
/// dependency: `resize` writes the spare capacity, `black_box` keeps the writes from being
/// elided before the free.
pub fn zero(v: &mut Vec<u8>) {
    v.fill(0);
    let capacity = v.capacity();
    v.resize(capacity, 0);
    std::hint::black_box(&mut *v);
}

/// Empties `s` after overwriting its bytes (`zero`): `--password`'s text, where practical.
pub fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    zero(&mut bytes);
}

/// dmenu stdin → lines: split on '\n', one trailing '\r' stripped per line,
/// empty lines dropped, invalid UTF-8 replaced (U+FFFD) by the caller before.
pub fn split_lines(input: &str) -> Vec<String> {
    numbered_lines(input).map(|(_, l)| l.to_string()).collect()
}

/// `split_lines`'s lines, each with its position among the input's lines (from 0), the dropped
/// empty ones counted: fuzzel keeps empty lines as entries, so its `--index` counts them.
pub fn numbered_lines(input: &str) -> impl Iterator<Item = (usize, &str)> {
    input.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).enumerate().filter(|(_, l)| !l.is_empty())
}

#[cfg(test)]
mod tests;
