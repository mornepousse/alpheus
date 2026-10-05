//! `alpheus-launch` against a fake daemon: a `UnixListener` on a temporary
//! socket that decodes and records the request, then answers a scripted
//! reply. Never the session's real socket.

use alpheus_commun::launcher_ipc::{MAX_DMENU_BYTES, MAX_LINES, Reply, Request, RequestDecoder, encode_reply, socket_path};
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
                    let _ = s.write_all(&encode_reply(&Reply::Busy));
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
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: Some("Pick: ".into()), lines });
}

#[test]
fn free_text_with_empty_stdin_prints_what_was_typed() {
    let (socket, daemon) = answering("free-text", Reply::Chosen("192.0.2.7".into()));
    assert_eq!(launch(&["--dmenu", "-p", "Target: "], &socket, b""), (EXIT_OK, "192.0.2.7\n".into(), String::new()));
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: Some("Target: ".into()), lines: vec![] });
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
    assert_eq!(daemon.join().unwrap(), Request::Dmenu { prompt: None, lines: vec!["caf\u{FFFD}".into(), "ok".into()] });
}

#[test]
fn usage_errors_exit_2_without_connecting() {
    let (socket, connections, stop, daemon) = counting_daemon("usage");
    for args in [&["--dmenu", "--width", "40"][..], &["--prompt", "x"], &["--toggle", "--close"]] {
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
}
