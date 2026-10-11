//! The launcher's wire protocol, shared by `alpheus-launch` (client) and
//! `alpheus-demon` (server) over `$XDG_RUNTIME_DIR/alpheus-launcher.sock`.
//! Std only, no serde: one header line, then (dmenu only) two
//! length-prefixed blobs. Every size is bounded and checked as soon as it is
//! known, so a hostile or broken client can never make the daemon buffer
//! more than `MAX_HEADER + MAX_PROMPT + MAX_DMENU_BYTES` bytes.

use std::fmt;
use std::path::{Path, PathBuf};

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

pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SOCKET_NAME)
}

/// `--dnd`'s word: the bar's do-not-disturb (notifications task 5).
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

/// `--quick`'s optional word (quick spec § 1): where the panel opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickVerb {
    /// `--quick`: the first row.
    Top,
    /// `--quick sound`: the volume row.
    Sound,
}

impl QuickVerb {
    pub const ALL: [QuickVerb; 2] = [QuickVerb::Top, QuickVerb::Sound];

    /// The word after `quick` (wire) and `--quick` (command line); None for the top.
    pub fn word(self) -> Option<&'static str> {
        match self {
            QuickVerb::Top => None,
            QuickVerb::Sound => Some("sound"),
        }
    }

    /// A word after `quick`: only "sound".
    pub fn parse(s: &str) -> Option<QuickVerb> {
        QuickVerb::ALL.into_iter().find(|v| v.word() == Some(s))
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
    Dmenu {
        prompt: Option<String>,
        lines: Vec<String>,
        flags: DmenuFlags,
    },
    /// The notifications module's do-not-disturb (notifications task 5): answered Done.
    Dnd(DndVerb),
    /// Empties the notification history (notifications task 5): answered Done.
    ClearNotifications,
    /// The OSD (OSD spec § 2): read the backlight once and show it. Answered Done, also with the
    /// module off or no backlight (nothing shown).
    Osd(OsdVerb),
    /// The quick settings panel (quick spec § 1): toggled, at the volume row with `sound`. Answered
    /// Done, also with the module off (nothing opens).
    Quick(QuickVerb),
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
///   "alpheus-launcher/1 osd brightness\n"
///   "alpheus-launcher/1 quick\n" | "alpheus-launcher/1 quick sound\n"
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
        Request::Dnd(v) => format!("{MAGIC} dnd {}\n", v.word()).into_bytes(),
        Request::ClearNotifications => format!("{MAGIC} clear-notifications\n").into_bytes(),
        Request::Osd(v) => format!("{MAGIC} osd {}\n", v.word()).into_bytes(),
        Request::Quick(v) => match v.word() {
            Some(w) => format!("{MAGIC} quick {w}\n").into_bytes(),
            None => format!("{MAGIC} quick\n").into_bytes(),
        },
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
    Dmenu { prompt: Option<usize>, body: usize, flags: DmenuFlags },
    Dnd(DndVerb),
    ClearNotifications,
    Osd(OsdVerb),
    Quick(QuickVerb),
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
    /// never read by the daemon itself (review item M5, final review
    /// slice 1).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

/// The request a header with no body stands for; None for dmenu, which has one.
fn bare_request(h: Header) -> Option<Request> {
    match h {
        Header::Toggle => Some(Request::Toggle),
        Header::Close => Some(Request::Close),
        Header::Dnd(v) => Some(Request::Dnd(v)),
        Header::ClearNotifications => Some(Request::ClearNotifications),
        Header::Osd(v) => Some(Request::Osd(v)),
        Header::Quick(v) => Some(Request::Quick(v)),
        Header::Dmenu { .. } => None,
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
        ["dnd", word] => {
            DndVerb::parse(word).map(Header::Dnd).ok_or_else(|| IpcError::Malformed(format!("unknown do-not-disturb word {:?}", cut(word))))
        }
        ["clear-notifications"] => Ok(Header::ClearNotifications),
        ["osd", word] => {
            OsdVerb::parse(word).map(Header::Osd).ok_or_else(|| IpcError::Malformed(format!("unknown osd word {:?}", cut(word))))
        }
        ["quick"] => Ok(Header::Quick(QuickVerb::Top)),
        ["quick", word] => {
            QuickVerb::parse(word).map(Header::Quick).ok_or_else(|| IpcError::Malformed(format!("unknown quick word {:?}", cut(word))))
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

/// "done\n" | "chosen <len>\n<bytes>" | "index <n>\n" | "index -\n" | "cancelled\n" | "busy\n" | "error <text>\n"
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
mod tests {
    use super::*;

    fn requests() -> Vec<Request> {
        vec![
            Request::Toggle,
            Request::Close,
            Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() },
            Request::Dmenu { prompt: Some(String::new()), lines: vec!["one".into()], flags: DmenuFlags::default() },
            Request::Dmenu {
                prompt: Some("Target (IP / host): ".into()),
                lines: vec!["a b c".into(), "tab\there".into(), "cr\rinside".into(), "café ☕ 日本".into(), "  padded  ".into()],
                flags: DmenuFlags::default(),
            },
            Request::Dmenu {
                prompt: None,
                lines: vec!["a".into(), "b".into()],
                flags: DmenuFlags { index: true, password: false, visible: None },
            },
            Request::Dmenu {
                prompt: Some("Key: ".into()),
                lines: vec![],
                flags: DmenuFlags { index: false, password: true, visible: None },
            },
            Request::Dmenu {
                prompt: Some("x".into()),
                lines: vec!["l".into()],
                flags: DmenuFlags { index: true, password: true, visible: None },
            },
            Request::Dmenu {
                prompt: None,
                lines: vec!["a".into(), "b".into(), "c".into()],
                flags: DmenuFlags { index: false, password: false, visible: Some(2) },
            },
            Request::Dmenu {
                prompt: Some("Go".into()),
                lines: vec!["a".into()],
                flags: DmenuFlags { index: true, password: false, visible: Some(0) },
            },
            Request::Dnd(DndVerb::On),
            Request::Dnd(DndVerb::Off),
            Request::Dnd(DndVerb::Toggle),
            Request::ClearNotifications,
            Request::Osd(OsdVerb::Brightness),
            Request::Quick(QuickVerb::Top),
            Request::Quick(QuickVerb::Sound),
        ]
    }

    fn decode(bytes: &[u8]) -> Result<Option<Request>, IpcError> {
        RequestDecoder::new().feed(bytes)
    }

    #[test]
    fn every_request_round_trips() {
        for r in requests() {
            assert_eq!(decode(&encode_request(&r)), Ok(Some(r.clone())), "{r:?}");
        }
    }

    #[test]
    fn byte_by_byte_gives_the_same_request() {
        for r in requests() {
            let bytes = encode_request(&r);
            let mut d = RequestDecoder::new();
            for (i, b) in bytes.iter().enumerate() {
                let got = d.feed(std::slice::from_ref(b)).unwrap();
                if i + 1 < bytes.len() {
                    assert_eq!(got, None, "{r:?} complete too early, at byte {i}");
                } else {
                    assert_eq!(got, Some(r.clone()));
                }
            }
        }
    }

    #[test]
    fn the_wire_format_is_the_documented_one() {
        assert_eq!(encode_request(&Request::Toggle), b"alpheus-launcher/1 toggle\n");
        assert_eq!(encode_request(&Request::Close), b"alpheus-launcher/1 close\n");
        assert_eq!(encode_request(&Request::Dnd(DndVerb::On)), b"alpheus-launcher/1 dnd on\n");
        assert_eq!(encode_request(&Request::Dnd(DndVerb::Off)), b"alpheus-launcher/1 dnd off\n");
        assert_eq!(encode_request(&Request::Dnd(DndVerb::Toggle)), b"alpheus-launcher/1 dnd toggle\n");
        assert_eq!(encode_request(&Request::ClearNotifications), b"alpheus-launcher/1 clear-notifications\n");
        assert_eq!(encode_request(&Request::Osd(OsdVerb::Brightness)), b"alpheus-launcher/1 osd brightness\n");
        assert_eq!(encode_request(&Request::Quick(QuickVerb::Top)), b"alpheus-launcher/1 quick\n");
        assert_eq!(encode_request(&Request::Quick(QuickVerb::Sound)), b"alpheus-launcher/1 quick sound\n");
        assert_eq!(
            encode_request(&Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }),
            b"alpheus-launcher/1 dmenu - 0\n"
        );
        let dmenu = Request::Dmenu { prompt: Some("Go: ".into()), lines: vec!["a".into(), "bé".into()], flags: DmenuFlags::default() };
        assert_eq!(encode_request(&dmenu), "alpheus-launcher/1 dmenu 4 5\nGo: a\nbé".as_bytes());
        let flagged = |index, password| Request::Dmenu {
            prompt: None,
            lines: vec!["a".into()],
            flags: DmenuFlags { index, password, visible: None },
        };
        assert_eq!(encode_request(&flagged(true, false)), b"alpheus-launcher/1 dmenu - 1 index\na");
        assert_eq!(encode_request(&flagged(false, true)), b"alpheus-launcher/1 dmenu - 1 password\na");
        assert_eq!(encode_request(&flagged(true, true)), b"alpheus-launcher/1 dmenu - 1 index password\na");
        let visible = |index, visible| Request::Dmenu {
            prompt: None,
            lines: vec!["a".into()],
            flags: DmenuFlags { index, password: false, visible },
        };
        assert_eq!(encode_request(&visible(false, Some(3))), b"alpheus-launcher/1 dmenu - 1 visible 3\na");
        assert_eq!(encode_request(&visible(true, Some(0))), b"alpheus-launcher/1 dmenu - 1 index visible 0\na");
        assert_eq!(encode_reply(&Reply::Index(Some(2))), b"index 2\n");
        assert_eq!(encode_reply(&Reply::Index(None)), b"index -\n");
        assert_eq!(encode_reply(&Reply::Done), b"done\n");
        assert_eq!(encode_reply(&Reply::Chosen("bé".into())), "chosen 3\nbé".as_bytes());
        assert_eq!(encode_reply(&Reply::Cancelled), b"cancelled\n");
        assert_eq!(encode_reply(&Reply::Busy), b"busy\n");
        assert_eq!(encode_reply(&Reply::Error("no".into())), b"error no\n");
    }

    #[test]
    fn malformed_headers_are_refused() {
        for h in [
            &b"toggle\n"[..],
            b"alpheus-launcher/2 toggle\n",
            b"alpheus-launcher/1 open\n",
            b"alpheus-launcher/1\n",
            b"alpheus-launcher/1 toggle now\n",
            b"alpheus-launcher/1  toggle\n",
            b"alpheus-launcher/1 dmenu x 3\n",
            b"alpheus-launcher/1 dmenu - -1\n",
            b"alpheus-launcher/1 dmenu - +3\n",
            b"alpheus-launcher/1 dmenu -\n",
            b"alpheus-launcher/1 dmenu - 1 2\n",
            b"\n",
            b"alpheus-launcher/1 \xff\n",
            b"alpheus-launcher/1 dnd\n",
            b"alpheus-launcher/1 dnd maybe\n",
            b"alpheus-launcher/1 dnd on now\n",
            b"alpheus-launcher/1 dnd  on\n",
            b"alpheus-launcher/1 clear-notifications now\n",
            b"alpheus-launcher/1 osd\n",
            b"alpheus-launcher/1 osd volume\n",
            b"alpheus-launcher/1 osd brightness now\n",
            b"alpheus-launcher/1 osd  brightness\n",
            b"alpheus-launcher/1 quick volume\n",
            b"alpheus-launcher/1 quick sound now\n",
            b"alpheus-launcher/1 quick Sound\n",
            b"alpheus-launcher/1 dmenu - 0 indexes\n",
            b"alpheus-launcher/1 dmenu - 0 index index\n",
            b"alpheus-launcher/1 dmenu - 0 visible\n",
            b"alpheus-launcher/1 dmenu - 0 visible x\n",
            b"alpheus-launcher/1 dmenu - 0 visible -1\n",
            b"alpheus-launcher/1 dmenu - 0 visible 1234567890123\n",
            b"alpheus-launcher/1 dmenu - 0 visible 1 visible 2\n",
            b"alpheus-launcher/1 dmenu - 0 visible 1 index\n",
            b"alpheus-launcher/1 dmenu - 0 password password\n",
            b"alpheus-launcher/1 dmenu - 0 password index\n",
            b"alpheus-launcher/1 dmenu - 0  index\n",
            b"alpheus-launcher/1 dmenu - 0 index \n",
            b"alpheus-launcher/1 dmenu - 0 Index\n",
        ] {
            assert!(matches!(decode(h), Err(IpcError::Malformed(_))), "{:?} → {:?}", String::from_utf8_lossy(h), decode(h));
        }
    }

    /// A request from a client that knows no flags (the trailing words are optional) still reads,
    /// as no flag: the header stays `alpheus-launcher/1`.
    #[test]
    fn a_dmenu_header_without_flags_still_decodes() {
        assert_eq!(
            decode(b"alpheus-launcher/1 dmenu - 3\na\nb"),
            Ok(Some(Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into()], flags: DmenuFlags::default() }))
        );
    }

    #[test]
    fn bytes_after_a_whole_request_are_malformed() {
        assert!(matches!(decode(b"alpheus-launcher/1 toggle\nx"), Err(IpcError::Malformed(_))));
        assert!(matches!(decode(b"alpheus-launcher/1 dmenu - 1\nab"), Err(IpcError::Malformed(_))));
        assert!(matches!(decode(b"alpheus-launcher/1 dnd on\nx"), Err(IpcError::Malformed(_))));
        assert!(matches!(decode(b"alpheus-launcher/1 clear-notifications\nx"), Err(IpcError::Malformed(_))));
        assert!(matches!(decode(b"alpheus-launcher/1 osd brightness\nx"), Err(IpcError::Malformed(_))));
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(b"alpheus-launcher/1 close\n"), Ok(Some(Request::Close)));
        assert!(matches!(d.feed(b"x"), Err(IpcError::Malformed(_))), "a second message on the same connection");
    }

    #[test]
    fn every_dnd_word_parses_back() {
        for v in DndVerb::ALL {
            assert_eq!(DndVerb::parse(v.word()), Some(v));
        }
        assert_eq!(DndVerb::parse("On"), None, "the words are exact");
        assert_eq!(DndVerb::parse(""), None);
    }

    #[test]
    fn every_osd_word_parses_back() {
        for v in OsdVerb::ALL {
            assert_eq!(OsdVerb::parse(v.word()), Some(v));
        }
        assert_eq!(OsdVerb::parse("Brightness"), None, "the words are exact");
        assert_eq!(OsdVerb::parse("volume"), None, "volume is passive: no verb");
    }

    #[test]
    fn every_quick_word_parses_back() {
        for v in QuickVerb::ALL {
            if let Some(w) = v.word() {
                assert_eq!(QuickVerb::parse(w), Some(v));
            }
        }
        assert_eq!(QuickVerb::Top.word(), None, "the bare verb");
        assert_eq!(QuickVerb::parse("Sound"), None, "the words are exact");
        assert_eq!(QuickVerb::parse("network"), None, "deliveries 2 and 3 add their sections");
    }

    #[test]
    fn a_header_without_end_is_refused_at_max_header() {
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(&[b'a'; MAX_HEADER]), Ok(None), "MAX_HEADER bytes may still be followed by '\\n'");
        assert_eq!(d.feed(b"a"), Err(IpcError::TooLarge("header")));
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(&vec![b'a'; 1 << 20]), Err(IpcError::TooLarge("header")));
        assert!(d.buffered() <= MAX_HEADER + 1, "{} bytes buffered", d.buffered());
    }

    #[test]
    fn prompt_and_body_limits_are_checked_on_the_header() {
        let mut chunk = format!("{MAGIC} dmenu {} 0\n", MAX_PROMPT + 1).into_bytes();
        chunk.extend(vec![b'p'; 4096]);
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(&chunk), Err(IpcError::TooLarge("prompt")));
        assert!(d.buffered() <= MAX_HEADER, "{} bytes buffered", d.buffered());
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(format!("{MAGIC} dmenu - {}\n", MAX_DMENU_BYTES + 1).as_bytes()), Err(IpcError::TooLarge("dmenu input")));
        assert!(d.buffered() <= MAX_HEADER, "{} bytes buffered", d.buffered());
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(format!("{MAGIC} dmenu {MAX_PROMPT} {MAX_DMENU_BYTES}\n").as_bytes()), Ok(None), "the limits themselves pass");
    }

    #[test]
    fn too_many_lines_are_refused_as_they_arrive() {
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(format!("{MAGIC} dmenu - {MAX_DMENU_BYTES}\n").as_bytes()), Ok(None));
        let chunk = b"x\n".repeat(1000);
        let mut result = Ok(None);
        for _ in 0..MAX_LINES / 1000 {
            result = d.feed(&chunk);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(IpcError::TooLarge("lines")));
        assert!(d.buffered() < 2 * MAX_LINES, "refused long before the 4 MiB body: {} bytes buffered", d.buffered());
    }

    #[test]
    fn the_body_is_buffered_once_at_its_announced_size() {
        let (prompt, body) = ("Go: ", "x".repeat(1_000_000));
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(format!("{MAGIC} dmenu {} {}\n", prompt.len(), body.len()).as_bytes()), Ok(None));
        let mut rest = format!("{prompt}{body}").into_bytes();
        let last = rest.split_off(rest.len() - 1);
        for chunk in rest.chunks(64 * 1024) {
            assert_eq!(d.feed(chunk), Ok(None));
        }
        assert_eq!(d.buf.capacity(), prompt.len() + body.len(), "reserved once, never doubled");
        assert_eq!(
            d.feed(&last),
            Ok(Some(Request::Dmenu { prompt: Some(prompt.into()), lines: vec![body], flags: DmenuFlags::default() }))
        );
    }

    #[test]
    fn exactly_max_lines_is_accepted() {
        let r =
            Request::Dmenu { prompt: None, lines: (0..MAX_LINES).map(|i| (i % 10).to_string()).collect(), flags: DmenuFlags::default() };
        assert_eq!(decode(&encode_request(&r)), Ok(Some(r)));
    }

    #[test]
    fn non_utf8_prompt_or_body_is_malformed() {
        let mut p = format!("{MAGIC} dmenu 1 1\n").into_bytes();
        p.extend([0xff, b'a']);
        assert!(matches!(decode(&p), Err(IpcError::Malformed(_))));
        let mut b = format!("{MAGIC} dmenu - 1\n").into_bytes();
        b.push(0xff);
        assert!(matches!(decode(&b), Err(IpcError::Malformed(_))));
    }

    #[test]
    fn every_reply_round_trips() {
        for r in [
            Reply::Done,
            Reply::Cancelled,
            Reply::Busy,
            Reply::Chosen(String::new()),
            Reply::Chosen("ssh  host-1 → café".into()),
            Reply::Chosen("two\nlines".into()),
            Reply::Error("launcher closed".into()),
            Reply::Error(String::new()),
            Reply::Index(Some(0)),
            Reply::Index(Some(MAX_LINES - 1)),
            Reply::Index(None),
        ] {
            assert_eq!(decode_reply(&encode_reply(&r)), Ok(r.clone()), "{r:?}");
        }
    }

    #[test]
    fn error_text_is_one_bounded_line() {
        assert_eq!(encode_reply(&Reply::Error("bad\nthing\r".into())), b"error bad thing \n");
        let Ok(Reply::Error(t)) = decode_reply(&encode_reply(&Reply::Error("é".repeat(MAX_ERROR)))) else { panic!("not an error reply") };
        assert!(t.len() <= MAX_ERROR && t.len() >= MAX_ERROR - 1 && t.chars().all(|c| c == 'é'), "{} bytes", t.len());
    }

    #[test]
    fn cut_or_odd_replies_are_refused() {
        assert_eq!(decode_reply(b""), Err(IpcError::Truncated));
        assert_eq!(decode_reply(b"done"), Err(IpcError::Truncated));
        assert_eq!(decode_reply(b"chosen 5\nab"), Err(IpcError::Truncated));
        for bad in [
            &b"chosen 1\nab"[..],
            b"done\nx",
            b"chosen x\n",
            b"hello\n",
            b"chosen\n",
            b"\xff\n",
            b"index\n",
            b"index x\n",
            b"index -1\n",
            b"index 1\nx",
            b"index  1\n",
        ] {
            assert!(matches!(decode_reply(bad), Err(IpcError::Malformed(_))), "{:?}", String::from_utf8_lossy(bad));
        }
        assert_eq!(decode_reply(format!("chosen {}\n", MAX_DMENU_BYTES + 1).as_bytes()), Err(IpcError::TooLarge("chosen line")));
        assert_eq!(decode_reply(format!("index {MAX_LINES}\n").as_bytes()), Err(IpcError::TooLarge("index")), "past the last line");
    }

    #[test]
    fn split_lines_follows_fuzzel() {
        assert_eq!(split_lines("a\nb\n"), ["a", "b"]);
        assert_eq!(split_lines("a\r\nb\r\n"), ["a", "b"]);
        assert_eq!(split_lines("\n\n a \n\r\n"), [" a "]);
        assert_eq!(split_lines("x\r\r\n"), ["x\r"], "one '\\r' stripped, not all");
        assert!(split_lines("").is_empty());
        assert_eq!(numbered_lines("a\n\nb").collect::<Vec<_>>(), [(0, "a"), (2, "b")]);
        assert_eq!(numbered_lines("\r\n\nx\r\n").collect::<Vec<_>>(), [(2, "x")], "\"\\r\" alone is an empty line too");
    }

    /// A password's bytes are overwritten before they are freed, spare capacity included (what a
    /// Backspace popped, what a push left behind).
    #[test]
    fn zeroed_overwrites_the_whole_allocation() {
        let mut v = b"hunter2".to_vec();
        v.reserve(9);
        v.truncate(3);
        let capacity = v.capacity();
        zero(&mut v);
        assert_eq!(v.len(), capacity, "the spare capacity is written too");
        assert!(v.iter().all(|&b| b == 0), "{v:?}");
        let mut s = String::from("hunter2");
        s.pop();
        wipe(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn socket_path_joins_the_name() {
        assert_eq!(socket_path(Path::new("/run/user/1000")), Path::new("/run/user/1000/alpheus-launcher.sock"));
    }

    #[test]
    fn the_decoder_is_poisoned_after_a_malformed_header() {
        let mut d = RequestDecoder::new();
        assert!(matches!(d.feed(b"alpheus-launcher/1\n"), Err(IpcError::Malformed(_))));
        // The second call completes what would otherwise be a valid "toggle" request
        // if the decoder kept going; it must not, because the connection already
        // sent something malformed.
        assert!(!matches!(d.feed(b" toggle\n"), Ok(Some(Request::Toggle))), "poisoned decoder must not yield a request");
        assert!(d.feed(b" toggle\n").is_err(), "poisoned decoder keeps refusing");
    }

    #[test]
    fn the_decoder_is_poisoned_after_a_dmenu_overflow() {
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(format!("{MAGIC} dmenu - 1\n").as_bytes()), Ok(None));
        assert!(matches!(d.feed(b"ab"), Err(IpcError::Malformed(_))), "bytes after the request");
        assert!(d.feed(b"c").is_err(), "poisoned decoder keeps refusing");
        assert!(d.feed(&[]).is_err(), "poisoned decoder refuses even an empty feed");
    }

    #[test]
    fn empty_dmenu_lines_are_dropped() {
        // The dmenu rule: a line holding nothing is not a line.
        let r = Request::Dmenu { prompt: None, lines: vec![String::new()], flags: DmenuFlags::default() };
        assert_eq!(
            decode(&encode_request(&r)),
            Ok(Some(Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() })),
            "vec![\"\"] round-trips to vec![]"
        );

        // A hostile body with blank lines in the middle: fed directly, bypassing encode_request.
        let body = "a\n\nb\n";
        let mut d = RequestDecoder::new();
        assert_eq!(d.feed(format!("{MAGIC} dmenu - {}\n", body.len()).as_bytes()), Ok(None));
        assert_eq!(
            d.feed(body.as_bytes()),
            Ok(Some(Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into()], flags: DmenuFlags::default() }))
        );
    }
}
