//! `alpheus-launch` against a fake daemon: a `UnixListener` on a temporary
//! socket that decodes and records the request, then answers a scripted
//! reply. Never the session's real socket.

use alpheus_common::launcher_ipc::{
    DmenuFlags, DndEnd, DndVerb, MAX_DMENU_BYTES, MAX_LINES, QuickVerb, Reply, Request, RequestDecoder, ShieldVerb, TimerVerb,
    encode_reply, socket_path,
};
use alpheus_launch::{CLIENT_REPLY_TIMEOUT, EXIT_CANCELLED, EXIT_ERROR, EXIT_OK, EXIT_UNREACHABLE, run};
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A socket path in its own temporary directory, removed when dropped.
struct TempSocket {
    dir: PathBuf,
    path: PathBuf,
}

impl std::ops::Deref for TempSocket {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn temp_socket(name: &str) -> TempSocket {
    let dir = std::env::temp_dir().join(format!("alpheus-launch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempSocket { path: socket_path(&dir), dir }
}

fn read_request(s: &mut UnixStream) -> Request {
    let mut decoder = RequestDecoder::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = s.read(&mut buf).expect("read");
        assert!(n > 0, "the client closed before a whole request");
        if let Some(r) = decoder.feed(&buf[..n]).expect("a valid request") {
            return r;
        }
    }
}

/// The client must keep its end open while it waits: the daemon reads a
/// hang-up as "the client went away" and closes a dmenu.
fn assert_still_open(s: &UnixStream) {
    s.set_nonblocking(true).unwrap();
    let mut reader = s;
    match reader.read(&mut [0u8; 1]) {
        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
        other => panic!("the client hung up (or sent more) after its request: {other:?}"),
    }
    s.set_nonblocking(false).unwrap();
}

/// Accepts one connection, records its request, waits `delay` then answers
/// `reply` — or, with None, keeps the connection open without a word for
/// CLIENT_REPLY_TIMEOUT + 1 s.
fn fake_daemon(name: &str, reply: Option<Reply>, delay: Duration) -> (TempSocket, JoinHandle<Request>) {
    let path = temp_socket(name);
    let listener = UnixListener::bind(&*path).unwrap();
    let handle = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let request = read_request(&mut s);
        assert_still_open(&s);
        match reply {
            Some(r) => {
                thread::sleep(delay);
                s.write_all(&encode_reply(&r)).unwrap();
                let _ = s.shutdown(Shutdown::Both);
            }
            None => thread::sleep(CLIENT_REPLY_TIMEOUT + Duration::from_secs(1)),
        }
        request
    });
    (path, handle)
}

fn answering(name: &str, reply: Reply) -> (TempSocket, JoinHandle<Request>) {
    fake_daemon(name, Some(reply), Duration::ZERO)
}

/// Accepts one connection, reads the request, then writes `b'x'` in chunks
/// forever, stopping only once a write fails (the client closed its end):
/// proves the client bounds how much of a reply it reads instead of reading
/// an unbounded stream.
fn streaming_daemon(name: &str) -> (TempSocket, JoinHandle<Request>) {
    let path = temp_socket(name);
    let listener = UnixListener::bind(&*path).unwrap();
    let handle = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let request = read_request(&mut s);
        let chunk = [b'x'; 64 * 1024];
        while s.write_all(&chunk).is_ok() {}
        request
    });
    (path, handle)
}

/// Accepts one connection, reads only a few bytes of what is otherwise a
/// multi-megabyte request (never draining the rest), then answers at once
/// and closes both directions. The client's own `write_all` of the request
/// is expected to later fail (broken pipe, the socket buffer having filled
/// up): proves the daemon's own early reply is still reported, rather than
/// being replaced by a generic "cannot send" error.
fn early_reply_daemon(name: &str, reply: Reply) -> (TempSocket, JoinHandle<()>) {
    let path = temp_socket(name);
    let listener = UnixListener::bind(&*path).unwrap();
    let handle = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut head = [0u8; 4096];
        let _ = s.read(&mut head);
        s.write_all(&encode_reply(&reply)).unwrap();
        let _ = s.shutdown(Shutdown::Both);
    });
    (path, handle)
}

/// Accepts one connection, records its request, then sends `reply`'s bytes
/// one at a time with `gap` between them, stopping early if a write fails
/// (the client gave up and closed its end). Each byte comfortably beats
/// CLIENT_REPLY_TIMEOUT, but the whole reply does not: proves the client's
/// deadline is a total budget, not a per-read allowance a trickling daemon
/// could keep restarting forever.
fn trickling_daemon(name: &str, reply: Reply, gap: Duration) -> (TempSocket, JoinHandle<Request>) {
    let path = temp_socket(name);
    let listener = UnixListener::bind(&*path).unwrap();
    let handle = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let request = read_request(&mut s);
        assert_still_open(&s);
        for byte in encode_reply(&reply) {
            thread::sleep(gap);
            if s.write_all(&[byte]).is_err() {
                break;
            }
        }
        request
    });
    (path, handle)
}

fn launch(args: &[&str], socket: &Path, stdin: &[u8]) -> (i32, String, String) {
    let args: Vec<OsString> = args.iter().map(|s| OsString::from(*s)).collect();
    let (mut input, mut out, mut err) = (stdin, Vec::new(), Vec::new());
    let code = run(&args, socket, &mut input, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

/// A daemon that answers every connection `busy` at once, without reading
/// it, and counts them until `stop` is set: a client that should not have
/// connected ends with "another menu is already open" instead of hanging.
fn counting_daemon(name: &str) -> (TempSocket, Arc<AtomicUsize>, Arc<AtomicBool>, JoinHandle<()>) {
    replying_daemon(name, Reply::Busy)
}

/// `counting_daemon` with another answer.
fn replying_daemon(name: &str, reply: Reply) -> (TempSocket, Arc<AtomicUsize>, Arc<AtomicBool>, JoinHandle<()>) {
    let path = temp_socket(name);
    let listener = UnixListener::bind(&*path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (connections, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
    let (count, stopped) = (connections.clone(), stop.clone());
    let handle = thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = s.write_all(&encode_reply(&reply));
                }
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
    });
    (path, connections, stop, handle)
}

fn assert_never_contacted(connections: Arc<AtomicUsize>, stop: Arc<AtomicBool>, daemon: JoinHandle<()>) {
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 0, "the daemon was contacted");
}

#[test]
fn toggle_and_close_send_their_request_and_exit_0() {
    for (i, (args, expected)) in
        [(&[] as &[&str], Request::Toggle), (&["--toggle"], Request::Toggle), (&["--close"], Request::Close)].into_iter().enumerate()
    {
        let (socket, daemon) = answering(&format!("toggle-{i}"), Reply::Done);
        assert_eq!(launch(args, &socket, b""), (EXIT_OK, String::new(), String::new()), "{args:?}");
        assert_eq!(daemon.join().unwrap(), expected);
    }
}

#[test]
fn dmenu_sends_prompt_and_lines_and_prints_the_choice() {
    let (socket, daemon) = answering("dmenu", Reply::Chosen("beta gamma".into()));
    let (code, out, err) = launch(&["--dmenu", "--prompt", "Pick: "], &socket, b"alpha\nbeta gamma\r\n\ndelta");
    assert_eq!((code, out.as_str(), err.as_str()), (EXIT_OK, "beta gamma\n", ""));
    let lines = vec!["alpha".to_string(), "beta gamma".into(), "delta".into()];
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: Some("Pick: ".into()), lines, flags: DmenuFlags::default() });
}

/// `--index` (audio-switcher, bzmenu): the chosen line's position on stdin, as fuzzel prints it.
#[test]
fn index_sends_the_flag_and_prints_the_position() {
    let (socket, daemon) = answering("index", Reply::Index(Some(2)));
    assert_eq!(launch(&["--dmenu", "--index"], &socket, b"a\nb\nc\n"), (EXIT_OK, "2\n".into(), String::new()));
    let flags = DmenuFlags { index: true, password: false, visible: None };
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into(), "c".into()], flags });
}

/// fuzzel 1.14.1 keeps empty lines as entries (`dmenu.c`), so `--index` counts stdin lines, empty
/// ones included; alpheus drops them from the list and maps the daemon's position back.
#[test]
fn index_counts_empty_lines_like_fuzzel() {
    let (socket, daemon) = answering("index-blank", Reply::Index(Some(1)));
    assert_eq!(launch(&["--dmenu", "--index"], &socket, b"a\n\nb"), (EXIT_OK, "2\n".into(), String::new()));
    let flags = DmenuFlags { index: true, password: false, visible: None };
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: None, lines: vec!["a".into(), "b".into()], flags });
    let (socket, daemon) = answering("index-blanks", Reply::Index(Some(2)));
    assert_eq!(launch(&["--dmenu", "--index"], &socket, b"\n\r\nx\n\ny\n\n\nz\n"), (EXIT_OK, "7\n".into(), String::new()));
    daemon.join().unwrap();
    let (socket, daemon) = answering("index-blank-none", Reply::Index(None));
    assert_eq!(launch(&["--dmenu", "--index"], &socket, b"\na\n"), (EXIT_OK, "-1\n".into(), String::new()), "-1 stays -1");
    daemon.join().unwrap();
}

/// A position past the lines sent is the daemon's mistake: an error, never a made-up number.
#[test]
fn an_index_past_the_lines_exits_2() {
    let (socket, daemon) = answering("index-past", Reply::Index(Some(2)));
    let (code, out, err) = launch(&["--dmenu", "--index"], &socket, b"a\n\nb\n");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("unexpected answer"), "{err}");
    daemon.join().unwrap();
}

/// Typed text that matched no line: fuzzel 1.14 prints -1 and exits 0 (`execute_selected`,
/// wayland.c), and so does alpheus.
#[test]
fn index_of_typed_text_is_minus_one() {
    let (socket, daemon) = answering("index-none", Reply::Index(None));
    assert_eq!(launch(&["--dmenu", "--index"], &socket, b"a\n"), (EXIT_OK, "-1\n".into(), String::new()));
    daemon.join().unwrap();
}

/// A reply of the other kind is a protocol error, and never echoes a chosen text (it may be a
/// password) to stderr.
#[test]
fn a_reply_of_the_wrong_kind_exits_2_without_its_text() {
    let (socket, daemon) = answering("index-chosen", Reply::Chosen("hunter2".into()));
    let (code, out, err) = launch(&["--dmenu", "--index"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("unexpected answer") && !err.contains("hunter2"), "{err}");
    daemon.join().unwrap();
    let (socket, daemon) = answering("chosen-index", Reply::Index(Some(0)));
    let (code, out, err) = launch(&["--dmenu"], &socket, b"a\n");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("unexpected answer"), "{err}");
    daemon.join().unwrap();
}

/// `--password` (networkmanager_dmenu's Wi-Fi key): stdin is read, then ignored (fuzzel accepts
/// it too): no line leaves the client; the typed text comes back as is.
#[test]
fn password_sends_no_lines_and_prints_the_typed_text() {
    let (socket, daemon) = answering("password", Reply::Chosen(" pass word ".into()));
    assert_eq!(
        launch(&["--dmenu", "--password", "-p", "Key: "], &socket, b"saved\nlines\n"),
        (EXIT_OK, " pass word \n".into(), String::new())
    );
    let flags = DmenuFlags { index: false, password: true, visible: None };
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: Some("Key: ".into()), lines: vec![], flags });
}

/// fuzzel's `--placeholder`, `--width` and `--lines` are accepted and change nothing: nothing of
/// them goes over the wire.
#[test]
fn placeholder_width_and_lines_are_ignored() {
    let (socket, daemon) = answering("ignored", Reply::Chosen("a".into()));
    let args = ["--dmenu", "--placeholder", "Search…", "--width", "50", "--lines=0", "--width=46", "--lines", "16", "--placeholder=x"];
    assert_eq!(launch(&args, &socket, b"a\n"), (EXIT_OK, "a\n".into(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: None, lines: vec!["a".into()], flags: DmenuFlags::default() });
}

#[test]
fn free_text_with_empty_stdin_prints_what_was_typed() {
    let (socket, daemon) = answering("free-text", Reply::Chosen("192.0.2.7".into()));
    assert_eq!(launch(&["--dmenu", "-p", "Target: "], &socket, b""), (EXIT_OK, "192.0.2.7\n".into(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: Some("Target: ".into()), lines: vec![], flags: DmenuFlags::default() });
}

#[test]
fn dmenu_cancelled_exits_1_with_empty_stdout() {
    let (socket, daemon) = answering("cancelled", Reply::Cancelled);
    assert_eq!(launch(&["--dmenu"], &socket, b"a\nb\n"), (EXIT_CANCELLED, String::new(), String::new()));
    daemon.join().unwrap();
}

#[test]
fn busy_exits_2_with_the_message() {
    let (socket, daemon) = answering("busy", Reply::Busy);
    let (code, out, err) = launch(&["--dmenu"], &socket, b"a\n");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("another menu is already open"), "{err}");
    daemon.join().unwrap();
}

#[test]
fn daemon_errors_and_odd_answers_exit_2() {
    let (socket, daemon) = answering("error", Reply::Error("launcher unavailable".into()));
    let (code, _, err) = launch(&["--toggle"], &socket, b"");
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("launcher unavailable"), "{err}");
    daemon.join().unwrap();
    let (socket, daemon) = answering("odd", Reply::Done);
    let (code, out, err) = launch(&["--dmenu"], &socket, b"a\n");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("unexpected answer"), "{err}");
    daemon.join().unwrap();
}

/// 3, not 2: the only code on which callers fall back to fuzzel (a busy
/// launcher or an error must never open a second menu beside alpheus's).
#[test]
fn an_unreachable_daemon_exits_3_not_running() {
    let socket = temp_socket("absent");
    let (code, out, err) = launch(&[], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""));
    assert!(err.contains("not running") && err.contains("alpheus-launcher.sock"), "{err}");
    // A stale socket file left by a crashed daemon: connection refused, same answer.
    drop(UnixListener::bind(&*socket).unwrap());
    let (code, _, err) = launch(&["--dmenu"], &socket, b"a\n");
    assert_eq!(code, EXIT_UNREACHABLE);
    assert!(err.contains("not running"), "{err}");
    // A plain file where the socket should be: refused too.
    std::fs::remove_file(&*socket).unwrap();
    std::fs::write(&*socket, "").unwrap();
    assert_eq!(launch(&["--close"], &socket, b"").0, EXIT_UNREACHABLE);
    // The runtime directory itself is missing (no session yet).
    assert_eq!(launch(&[], Path::new("/nonexistent/alpheus-launcher.sock"), b"").0, EXIT_UNREACHABLE);
}

#[test]
fn a_daemon_that_never_answers_a_toggle_is_given_up_on() {
    let (socket, daemon) = fake_daemon("silent", None, Duration::ZERO);
    let start = Instant::now();
    let (code, _, err) = launch(&["--toggle"], &socket, b"");
    let took = start.elapsed();
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("no answer"), "{err}");
    assert!(took >= CLIENT_REPLY_TIMEOUT - Duration::from_millis(50) && took < CLIENT_REPLY_TIMEOUT + Duration::from_secs(1), "{took:?}");
    assert_eq!(daemon.join().unwrap(), Request::Toggle);
}

#[test]
fn dmenu_waits_for_the_person_beyond_the_toggle_timeout() {
    let (socket, daemon) = fake_daemon("slow", Some(Reply::Chosen("late".into())), CLIENT_REPLY_TIMEOUT + Duration::from_millis(500));
    assert_eq!(launch(&["--dmenu"], &socket, b"late\n"), (EXIT_OK, "late\n".into(), String::new()));
    daemon.join().unwrap();
}

/// A daemon that keeps streaming garbage forever instead of a bounded reply:
/// the client must cut its read off (MAX_HEADER + MAX_DMENU_BYTES + 1 bytes)
/// and report an unexpected answer, rather than reading an unbounded stream.
/// Run on its own thread with a watchdog: if the client's read is ever
/// unbounded again, this must fail fast instead of hanging the test suite.
#[test]
fn an_endless_reply_is_cut_off_and_rejected() {
    let (socket, daemon) = streaming_daemon("endless-reply");
    let path = socket.path.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(launch(&["--toggle"], &path, b""));
    });
    let (code, out, err) =
        rx.recv_timeout(Duration::from_secs(10)).expect("alpheus-launch must bound its reply read instead of reading forever");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("unexpected answer"), "{err}");
    assert_eq!(daemon.join().unwrap(), Request::Toggle);
    drop(socket);
}

/// The daemon never finishes reading the (multi-megabyte) request, but
/// answers right away with its own error and closes: the client's own send
/// is expected to later fail (the socket buffer fills up with nobody
/// draining it), yet the daemon's early reply must still be the one
/// reported — not a generic "cannot send the request".
#[test]
fn an_early_error_reply_is_reported_even_if_sending_then_fails() {
    let (socket, daemon) = early_reply_daemon("early-error", Reply::Error("dmenu input too large".into()));
    let big = vec![b'a'; MAX_DMENU_BYTES];
    let (code, out, err) = launch(&["--dmenu"], &socket, &big);
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("alpheus-demon: dmenu input too large"), "{err}");
    assert!(!err.contains("cannot send the request"), "{err}");
    daemon.join().unwrap();
}

/// Each byte of the reply arrives well inside CLIENT_REPLY_TIMEOUT, so a
/// per-read timeout alone would never fire; only a true total deadline
/// gives up on this toggle, instead of letting the trickle restart the
/// clock forever.
#[test]
fn a_trickling_reply_is_still_given_up_on_within_the_deadline() {
    let gap = CLIENT_REPLY_TIMEOUT / 3;
    let (socket, daemon) = trickling_daemon("trickle", Reply::Done, gap);
    let start = Instant::now();
    let (code, out, err) = launch(&["--toggle"], &socket, b"");
    let took = start.elapsed();
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
    assert!(err.contains("no answer"), "{err}");
    assert!(took >= CLIENT_REPLY_TIMEOUT - Duration::from_millis(100) && took < CLIENT_REPLY_TIMEOUT + Duration::from_secs(1), "{took:?}");
    assert_eq!(daemon.join().unwrap(), Request::Toggle);
}

#[test]
fn oversized_stdin_is_refused_before_connecting() {
    let (socket, connections, stop, daemon) = counting_daemon("oversized");
    for (stdin, needle) in [
        (vec![b'a'; MAX_DMENU_BYTES + 1], "larger than 4 MiB"),
        ("x\n".repeat(MAX_LINES + 1).into_bytes(), "lines"),
        // Lossy replacement makes it grow: 2 MiB of invalid bytes become 6 MiB of U+FFFD.
        (vec![0xff; MAX_DMENU_BYTES / 2], "larger than 4 MiB"),
    ] {
        let (code, out, err) = launch(&["--dmenu"], &socket, &stdin);
        assert_eq!((code, out.as_str()), (EXIT_ERROR, ""));
        assert!(err.contains(needle), "{err}");
    }
    assert_never_contacted(connections, stop, daemon);
}

#[test]
fn stdin_at_the_limit_is_sent() {
    let (socket, daemon) = answering("at-limit", Reply::Cancelled);
    assert_eq!(launch(&["--dmenu"], &socket, &vec![b'a'; MAX_DMENU_BYTES]).0, EXIT_CANCELLED);
    let Request::Dmenu { lines, .. } = daemon.join().unwrap() else { panic!("not a dmenu request") };
    assert_eq!((lines.len(), lines[0].len()), (1, MAX_DMENU_BYTES));

    // Exactly MAX_LINES lines: still accepted, not refused as "too many".
    let (socket, daemon) = answering("at-limit-lines", Reply::Cancelled);
    assert_eq!(launch(&["--dmenu"], &socket, "x\n".repeat(MAX_LINES).as_bytes()).0, EXIT_CANCELLED);
    let Request::Dmenu { lines, .. } = daemon.join().unwrap() else { panic!("not a dmenu request") };
    assert_eq!(lines.len(), MAX_LINES);
}

#[test]
fn non_utf8_lines_are_replaced() {
    let (socket, daemon) = answering("lossy", Reply::Chosen("caf\u{FFFD}".into()));
    assert_eq!(launch(&["--dmenu"], &socket, b"caf\xe9\nok\n"), (EXIT_OK, "caf\u{FFFD}\n".into(), String::new()));
    assert_eq!(
        daemon.join().unwrap(),
        Request::Dmenu { prompt: None, lines: vec!["caf\u{FFFD}".into(), "ok".into()], flags: DmenuFlags::default() }
    );
}

#[test]
fn usage_errors_exit_2_without_connecting() {
    let (socket, connections, stop, daemon) = counting_daemon("usage");
    for args in [
        &["--dmenu", "--width", "wide"][..],
        &["--prompt", "x"],
        &["--toggle", "--close"],
        &["--dmenu", "-l", "3"],
        &["--dmenu", "--password", "--index"],
    ] {
        let (code, out, err) = launch(args, &socket, b"a\n");
        assert_eq!((code, out.as_str()), (EXIT_ERROR, ""), "{args:?}");
        assert!(err.starts_with("alpheus-launch: ") && err.contains("--help"), "{args:?}: {err}");
    }
    assert_never_contacted(connections, stop, daemon);
}

#[test]
fn help_prints_usage_and_exits_0() {
    let (code, out, err) = launch(&["--help"], Path::new("/nonexistent/alpheus-launcher.sock"), b"");
    assert_eq!((code, err.as_str()), (EXIT_OK, ""));
    assert!(out.starts_with("Usage: alpheus-launch") && out.contains("Exit codes"), "{out}");
    for option in [
        "--index",
        "--password",
        "--visible N",
        "--placeholder TEXT",
        "--width N",
        "--lines N",
        "--quick [sound|wifi|bluetooth|vpn]",
        "(or its wifi, bluetooth or vpn word)",
        "--failed-refresh",
    ] {
        assert!(out.contains(option), "{option} undocumented: {out}");
    }
}

#[test]
fn dnd_and_clear_send_their_request_and_exit_0() {
    let cases = [
        (&["--dnd", "on"][..], Request::Dnd(DndVerb::On)),
        (&["--dnd", "off"], Request::Dnd(DndVerb::Off)),
        (&["--dnd", "toggle"], Request::Dnd(DndVerb::Toggle)),
        (&["--dnd", "on", "--for", "30m"], Request::DndOn(DndEnd::For(1800))),
        (&["--dnd", "on", "--for=1h30m"], Request::DndOn(DndEnd::For(5400))),
        (&["--clear-notifications"], Request::ClearNotifications),
        (&["--failed-refresh"], Request::FailedRefresh),
    ];
    for (i, (args, expected)) in cases.into_iter().enumerate() {
        let (socket, daemon) = answering(&format!("notify-{i}"), Reply::Done);
        assert_eq!(launch(args, &socket, b""), (EXIT_OK, String::new(), String::new()), "{args:?}");
        assert_eq!(daemon.join().unwrap(), expected);
    }
}

/// Quick spec § 1: `--quick [sound]` sends its word and exits 0 on Done.
#[test]
fn quick_sends_its_word_and_exits_0() {
    for (i, (args, expected)) in [
        (&["--quick"][..], Request::Quick(QuickVerb::Top)),
        (&["--quick", "sound"], Request::Quick(QuickVerb::Sound)),
        (&["--quick", "wifi"], Request::Quick(QuickVerb::Wifi)),
        (&["--quick", "bluetooth"], Request::Quick(QuickVerb::Bluetooth)),
        (&["--quick", "vpn"], Request::Quick(QuickVerb::Vpn)),
    ]
    .into_iter()
    .enumerate()
    {
        let (socket, daemon) = answering(&format!("quick-{i}"), Reply::Done);
        assert_eq!(launch(args, &socket, b""), (EXIT_OK, String::new(), String::new()), "{args:?}");
        assert_eq!(daemon.join().unwrap(), expected);
    }
}

/// Small features spec: `--calc` sends its request and exits 0 on Done; a daemon too old for it
/// answers the unknown request: exit 3, one request.
#[test]
fn calc_sends_its_request_and_an_old_daemon_exits_3() {
    let (socket, daemon) = answering("calc", Reply::Done);
    assert_eq!(launch(&["--calc"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Calc);
    let (socket, connections, stop, daemon) =
        replying_daemon("calc-old", Reply::Error("malformed request: unknown request \"calc\"".into()));
    let (code, out, err) = launch(&["--calc"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--calc"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

/// Timer spec § 7: `--timer` sends its request and exits 0 on Done; a daemon too old for it answers
/// the unknown request: exit 3, one request; any other error is exit 2 with its text.
#[test]
fn timer_sends_its_request_and_an_old_daemon_exits_3() {
    let (socket, daemon) = answering("timer", Reply::Done);
    assert_eq!(launch(&["--timer", "start", "25m"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Timer(TimerVerb::Start(1500)));
    let (socket, connections, stop, daemon) =
        replying_daemon("timer-old", Reply::Error("malformed request: unknown request \"timer\"".into()));
    let (code, out, err) = launch(&["--timer", "popup"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--timer"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    let (socket, _connections, stop, daemon) = replying_daemon("timer-off", Reply::Error("the timer module is not in modules".into()));
    let (code, out, err) = launch(&["--timer", "stop"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""), "{err}");
    assert!(err.contains("the timer module is not in modules"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
}

/// Launcher 2: `--windows` sends its request and exits 0 on Done; a daemon too old for it answers
/// the unknown request: exit 3, one request.
#[test]
fn windows_sends_its_request_and_an_old_daemon_exits_3() {
    let (socket, daemon) = answering("windows", Reply::Done);
    assert_eq!(launch(&["--windows"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Windows);
    let (socket, connections, stop, daemon) =
        replying_daemon("windows-old", Reply::Error("malformed request: unknown request \"windows\"".into()));
    let (code, out, err) = launch(&["--windows"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--windows"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

/// Quick notes: `--notes` sends its request and exits 0 on Done; a daemon too old for it answers
/// the unknown request: exit 3, one request.
#[test]
fn notes_sends_its_request_and_an_old_daemon_exits_3() {
    let (socket, daemon) = answering("notes", Reply::Done);
    assert_eq!(launch(&["--notes"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Notes);
    let (socket, connections, stop, daemon) =
        replying_daemon("notes-old", Reply::Error("malformed request: unknown request \"notes\"".into()));
    let (code, out, err) = launch(&["--notes"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--notes"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

/// Spec § 1: a daemon older than `--quick` refuses the word as an unknown request: exit 3, as for
/// the other verbs, one request, no retry.
#[test]
fn an_old_daemon_refusing_quick_exits_3() {
    let (socket, connections, stop, daemon) =
        replying_daemon("quick-old", Reply::Error("malformed request: unknown request \"quick\"".into()));
    let (code, out, err) = launch(&["--quick"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--quick"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    let socket = temp_socket("quick-absent");
    assert_eq!(launch(&["--quick", "sound"], &socket, b"").0, EXIT_UNREACHABLE, "no daemon: 3");
}

/// Quick Wi-Fi spec § 1: a daemon of delivery 1 knows `quick` but not its `wifi` word: exit 3, as for
/// any verb an older daemon refuses, one request, no retry.
#[test]
fn a_daemon_without_the_wifi_word_exits_3() {
    let (socket, connections, stop, daemon) =
        replying_daemon("quick-wifi-old", Reply::Error("malformed request: unknown quick word \"wifi\"".into()));
    let (code, out, err) = launch(&["--quick", "wifi"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--quick wifi"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

/// Quick Bluetooth spec § 1: a daemon of delivery 2 knows `quick` but not its `bluetooth` word: exit
/// 3, as for any verb an older daemon refuses, one request, no retry.
#[test]
fn a_daemon_without_the_bluetooth_word_exits_3() {
    let (socket, connections, stop, daemon) =
        replying_daemon("quick-bt-old", Reply::Error("malformed request: unknown quick word \"bluetooth\"".into()));
    let (code, out, err) = launch(&["--quick", "bluetooth"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--quick bluetooth"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

/// Quick VPN spec § 1: a daemon of delivery 3 knows `quick` but not its `vpn` word: exit 3, one
/// request, no retry.
#[test]
fn a_daemon_without_the_vpn_word_exits_3() {
    let (socket, connections, stop, daemon) =
        replying_daemon("quick-vpn-old", Reply::Error("malformed request: unknown quick word \"vpn\"".into()));
    let (code, out, err) = launch(&["--quick", "vpn"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--quick vpn"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

/// Review Focus: `--dnd toggle` with no daemon exits 3, like every verb (tools-menu falls back on 3).
#[test]
fn dnd_toggle_with_no_daemon_exits_3() {
    let socket = temp_socket("dnd-absent");
    let (code, out, err) = launch(&["--dnd", "toggle"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""));
    assert!(err.contains("not running"), "{err}");
    assert_eq!(launch(&["--clear-notifications"], &socket, b"").0, EXIT_UNREACHABLE);
    assert_eq!(launch(&["--failed-refresh"], &socket, b"").0, EXIT_UNREACHABLE);
}

#[test]
fn a_refused_verb_exits_2_with_the_daemons_word() {
    let (socket, daemon) = answering("dnd-refused", Reply::Error("the notifications module is not in modules".into()));
    let (code, _, err) = launch(&["--dnd", "on"], &socket, b"");
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("notifications module"), "{err}");
    daemon.join().unwrap();
    let (socket, daemon) = answering("dnd-odd", Reply::Chosen("x".into()));
    let (code, _, err) = launch(&["--clear-notifications"], &socket, b"");
    assert_eq!(code, EXIT_ERROR);
    assert!(err.contains("unexpected answer"), "{err}");
    daemon.join().unwrap();
}

/// What a daemon from before `--index`/`--password` answers a request that carries them: its
/// header decoder knew `dmenu <p> <len>` only, so the extra words fall to "unknown request", and
/// `socket::describe` prefixes "malformed request: ".
const OLD_DAEMON_ERROR: &str = "malformed request: unknown request \"dmenu\"";

/// A new client in front of a daemon still running the old binary (an upgrade before a re-login):
/// the menu it asked for cannot be shown with its flags, so the daemon counts as unusable — exit 3,
/// on which nixos-config falls back to fuzzel with `--index`/`--password` intact. One connection
/// only: never retried without the flags (an index printed as text, a password shown in clear).
#[test]
fn an_old_daemon_refusing_the_flags_exits_3_without_a_retry() {
    for (i, flag) in ["--index", "--password"].into_iter().enumerate() {
        let (socket, connections, stop, daemon) = replying_daemon(&format!("old-{i}"), Reply::Error(OLD_DAEMON_ERROR.into()));
        let (code, out, err) = launch(&["--dmenu", flag], &socket, b"a\n");
        assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{flag}: {err}");
        assert!(err.contains(flag), "{flag}: {err}");
        stop.store(true, Ordering::SeqCst);
        daemon.join().unwrap();
        assert_eq!(connections.load(Ordering::SeqCst), 1, "{flag}: one request, no retry");
    }
}

/// `--visible N` (tools-menu's home menu): the count goes on the request, the choice comes back as usual.
#[test]
fn visible_sends_its_count() {
    let (socket, daemon) = answering("visible", Reply::Chosen("Captures · Zone".into()));
    let input = b"Captures\nAffichage\nCaptures \xc2\xb7 Zone\n";
    assert_eq!(launch(&["--dmenu", "--visible", "2"], &socket, input), (EXIT_OK, "Captures · Zone\n".into(), String::new()));
    let lines = vec!["Captures".into(), "Affichage".into(), "Captures · Zone".into()];
    let flags = DmenuFlags { index: false, password: false, visible: Some(2) };
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: None, lines, flags });
}

/// Review Focus 5: `--index` with `--visible` still answers the line's position on stdin, empty lines counted.
#[test]
fn visible_with_index_counts_empty_lines() {
    let (socket, daemon) = answering("visible-index", Reply::Index(Some(2)));
    assert_eq!(launch(&["--dmenu", "--index", "--visible", "1"], &socket, b"a\n\nb\nc"), (EXIT_OK, "3\n".into(), String::new()));
    daemon.join().unwrap();
}

/// A daemon that knows `--index`/`--password` but not `--visible` refuses the word: like an older
/// daemon, exit 3, one connection, so tools-menu falls back to fuzzel.
#[test]
fn a_daemon_without_visible_exits_3_without_a_retry() {
    for (i, error) in [OLD_DAEMON_ERROR, "malformed request: unknown dmenu flag \"visible\""].into_iter().enumerate() {
        let (socket, connections, stop, daemon) = replying_daemon(&format!("no-visible-{i}"), Reply::Error(error.into()));
        let (code, out, err) = launch(&["--dmenu", "--visible", "2"], &socket, b"a\n");
        assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{error}: {err}");
        assert!(err.contains("--visible"), "{err}");
        stop.store(true, Ordering::SeqCst);
        daemon.join().unwrap();
        assert_eq!(connections.load(Ordering::SeqCst), 1, "one request, no retry");
    }
}

/// Only that refusal, only for a flagged request: anything else stays an error (2).
#[test]
fn other_refusals_of_a_dmenu_stay_errors() {
    let cases = [
        (&["--dmenu"][..], OLD_DAEMON_ERROR),
        (&["--dmenu", "--index"], "launcher unavailable"),
        (&["--dmenu", "--password"], "malformed request: unknown dmenu flag \"x\""),
        (&["--dmenu", "--index"], "malformed request: unknown dmenu flag \"visible\""),
        (&["--dmenu", "--index"], "request too large (dmenu input)"),
    ];
    for (i, (args, error)) in cases.into_iter().enumerate() {
        let (socket, daemon) = answering(&format!("refused-{i}"), Reply::Error(error.into()));
        let (code, _, err) = launch(args, &socket, b"a\n");
        assert_eq!(code, EXIT_ERROR, "{args:?} / {error}: {err}");
        daemon.join().unwrap();
    }
}

/// Clipboard spec: both verbs send their request and exit 0 on Done; a daemon too old for one
/// answers the unknown request: exit 3, one request; an error reply is exit 2 with its text.
#[test]
fn clipboard_clear_sends_its_request_and_an_old_daemon_exits_3() {
    let (socket, daemon) = answering("clipboard-clear", Reply::Done);
    assert_eq!(launch(&["--clipboard-clear"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::ClipboardClear);
    let (socket, daemon) = answering("clipboard", Reply::Done);
    assert_eq!(launch(&["--clipboard"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Clipboard);
    let (socket, connections, stop, daemon) =
        replying_daemon("clipboard-old", Reply::Error("malformed request: unknown request \"clipboard-clear\"".into()));
    let (code, out, err) = launch(&["--clipboard-clear"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--clipboard-clear"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    let (socket, _connections, stop, daemon) =
        replying_daemon("clipboard-off", Reply::Error("the clipboard history is off ([clipboard] enabled in bar.toml)".into()));
    let (code, out, err) = launch(&["--clipboard"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""), "{err}");
    assert!(err.contains("the clipboard history is off"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
}

/// Tools spec § 1, § 5: both verbs send their request and exit 0 on Done; a daemon too old for one
/// answers the unknown request: exit 3, one request; the power menu's refusal is exit 2 with its text.
#[test]
fn tools_and_power_send_their_requests_and_an_old_daemon_exits_3() {
    let (socket, daemon) = answering("tools", Reply::Done);
    assert_eq!(launch(&["--tools"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Tools);
    let (socket, daemon) = answering("power", Reply::Done);
    assert_eq!(launch(&["--power"], &socket, b""), (EXIT_OK, String::new(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Power);
    for (flag, word) in [("--tools", "tools"), ("--power", "power")] {
        let (socket, connections, stop, daemon) =
            replying_daemon(&format!("{word}-old"), Reply::Error(format!("malformed request: unknown request \"{word}\"")));
        let (code, out, err) = launch(&[flag], &socket, b"");
        assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
        assert!(err.contains(flag), "{err}");
        stop.store(true, Ordering::SeqCst);
        daemon.join().unwrap();
        assert_eq!(connections.load(Ordering::SeqCst), 1);
    }
    let refusal = "no session command is set ([quick] lock, logout, reboot, poweroff)";
    let (socket, _connections, stop, daemon) = replying_daemon("power-refused", Reply::Error(refusal.into()));
    let (code, out, err) = launch(&["--power"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""), "{err}");
    assert!(err.contains(refusal), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
}

/// Shield spec § 9: every word sends its request; `status` prints the body (exit 0, 2 when the
/// state is unknown); `allow` exits 1 when the picker is escaped; an old daemon exits 3, an
/// unconfigured one 2 with its text.
#[test]
fn shield_verbs_send_their_request_and_map_the_answers() {
    for v in [ShieldVerb::On, ShieldVerb::Off, ShieldVerb::Toggle, ShieldVerb::Allow] {
        let (socket, daemon) = answering("shield-done", Reply::Done);
        assert_eq!(launch(&["--shield", v.word()], &socket, b""), (EXIT_OK, String::new(), String::new()), "{v:?}");
        assert_eq!(daemon.join().unwrap(), Request::Shield(v));
    }
    let body = "Shield: on\nBlocked USB devices:\n  Test Stick (1234:5678)";
    let (socket, daemon) = answering("shield-status", Reply::Shield(body.into()));
    assert_eq!(launch(&["--shield", "status"], &socket, b""), (EXIT_OK, format!("{body}\n"), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Shield(ShieldVerb::Status));
    let (socket, daemon) = answering("shield-unknown", Reply::Shield("Shield: unknown".into()));
    assert_eq!(launch(&["--shield", "status"], &socket, b""), (EXIT_ERROR, "Shield: unknown\n".into(), String::new()));
    daemon.join().unwrap();
    let (socket, daemon) = answering("shield-escape", Reply::Cancelled);
    assert_eq!(launch(&["--shield", "allow"], &socket, b""), (EXIT_CANCELLED, String::new(), String::new()));
    daemon.join().unwrap();
    let (socket, _connections, stop, daemon) =
        replying_daemon("shield-old", Reply::Error("malformed request: unknown request \"shield\"".into()));
    let (code, out, err) = launch(&["--shield", "status"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_UNREACHABLE, ""), "{err}");
    assert!(err.contains("--shield"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    let (socket, _connections, stop, daemon) = replying_daemon("shield-off", Reply::Error("shield is not configured in bar.toml".into()));
    let (code, out, err) = launch(&["--shield", "on"], &socket, b"");
    assert_eq!((code, out.as_str()), (EXIT_ERROR, ""), "{err}");
    assert!(err.contains("shield is not configured"), "{err}");
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
}
