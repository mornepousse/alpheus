//! The launcher socket, daemon side (src/launcher/socket.rs), against real
//! clients on temporary sockets — never $XDG_RUNTIME_DIR, never the daemon.
//! In tests/ rather than in socket.rs: the guardrails reject the text
//! `UnixStream::connect` anywhere in src/ outside src/sources/ (test modules
//! included), and these tests need a connecting client.

use alpheus_common::launcher_ipc::{DmenuFlags, MAX_DMENU_BYTES, Reply, Request, decode_reply, encode_request};
use alpheus_daemon::launcher::socket::{Conn, Progress, REPLY_TIMEOUT, REQUEST_DEADLINE, Server};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

/// Bounds a socket's own blocking receive calls (`accept`, `read`) at the
/// kernel level: a mutation that drops a `set_nonblocking` call turns
/// `accept`/`read` genuinely blocking, and without this, a single such
/// syscall would never return — the polling loops below (`accepted`,
/// `until_done`) never even get a chance to check their own wall-clock
/// budget, and the whole suite hangs rather than failing. With
/// `SO_RCVTIMEO` set, the same genuinely blocking call instead returns
/// `EAGAIN`/`EWOULDBLOCK` once the timeout elapses — exactly the error a
/// correctly non-blocking socket already returns immediately — which
/// `not_blocking` below turns into a clean, bounded failure instead of a
/// silent, slow pass. It never affects a correctly non-blocking socket,
/// which already returns immediately regardless of this setting.
fn set_rcvtimeo(fd: BorrowedFd<'_>, dur: Duration) {
    let tv = libc::timeval { tv_sec: dur.as_secs() as libc::time_t, tv_usec: libc::suseconds_t::from(dur.subsec_micros()) };
    // SAFETY: setsockopt(2) reads exactly `size_of::<timeval>()` bytes from
    // `&tv`, a valid, live `timeval` owned by this frame; `fd` is open for
    // the whole call.
    let r = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&raw const tv).cast(),
            size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    assert_eq!(r, 0, "setsockopt(SO_RCVTIMEO): {}", io::Error::last_os_error());
}

/// A temporary 0700 directory, removed on drop.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let d = env::temp_dir().join(format!("alpheus-socket-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
    Dir(d)
}

fn sock(d: &Dir) -> PathBuf {
    d.0.join("launcher.sock")
}

/// Kernel-level ceiling for the two watchdogs below: a genuinely blocking
/// `accept`/`read` still returns well before `accepted`/`until_done`'s own
/// 2 s wall-clock budget runs out, so a mutation shows up as a `not_blocking`
/// failure, not as the outer "never accepted"/"never completed" one.
const WATCHDOG: Duration = Duration::from_millis(300);

/// A correctly non-blocking `accept`/`read` that has nothing to give
/// returns in well under a millisecond: no syscall wait, just `EAGAIN`.
/// Anything close to `WATCHDOG` means the call actually blocked — `set_nonblocking`
/// missing somewhere — and `SO_RCVTIMEO` (`set_rcvtimeo`, set on every fd
/// these tests touch) is the only reason it returned at all instead of
/// hanging the whole suite. The margin is generous (a fifth of `WATCHDOG`)
/// for a loaded test machine (nice -19, shared CPU), while staying far
/// under the genuinely-blocked case.
fn not_blocking<T>(call: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = call();
    assert!(t.elapsed() < WATCHDOG / 5, "a call that must be non-blocking took {:?}: a set_nonblocking call is missing", t.elapsed());
    r
}

/// The listener is non-blocking: retries until the client's connection shows up.
fn accepted(server: &Server) -> Conn {
    set_rcvtimeo(server.fd(), WATCHDOG);
    let t = Instant::now();
    loop {
        if let Some(c) = not_blocking(|| server.accept().unwrap()) {
            set_rcvtimeo(c.fd(), WATCHDOG);
            return c;
        }
        assert!(t.elapsed() < Duration::from_secs(2), "no connection accepted");
        thread::sleep(Duration::from_millis(5));
    }
}

fn until_done(conn: &mut Conn) -> Progress {
    let t = Instant::now();
    loop {
        match not_blocking(|| conn.read()) {
            Progress::Pending => {
                assert!(t.elapsed() < Duration::from_secs(2), "request never completed");
                thread::sleep(Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

fn client(path: &Path) -> UnixStream {
    UnixStream::connect(path).unwrap()
}

fn answer(mut c: UnixStream) -> Reply {
    let mut bytes = Vec::new();
    c.read_to_end(&mut bytes).unwrap();
    decode_reply(&bytes).unwrap()
}

#[test]
fn bind_makes_an_owner_only_socket_removed_on_drop() {
    let d = dir("mode");
    let server = Server::bind(&sock(&d)).unwrap();
    let meta = fs::metadata(sock(&d)).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600, "owner-only, whatever the umask");
    drop(server);
    assert!(!sock(&d).exists(), "the socket file goes with the server");
}

#[test]
fn a_stale_file_is_replaced() {
    let d = dir("stale");
    fs::write(sock(&d), "left by a SIGKILLed daemon").unwrap();
    let _server = Server::bind(&sock(&d)).unwrap();
    assert!(fs::metadata(sock(&d)).unwrap().file_type().is_socket());
}

/// The realistic case `a_stale_file_is_replaced` only approximates with a
/// plain text file: a daemon killed without running its own `Drop` (SIGKILL,
/// a crash) leaves behind a real socket inode, still bound, with nothing
/// listening on it any more. `Server::bind` must replace it just the same.
#[test]
fn a_stale_socket_left_by_a_dead_listener_is_replaced() {
    let d = dir("stale-socket");
    let path = sock(&d);
    {
        // Simulates the previous daemon's listener: a `UnixListener` whose
        // `Drop` (unlike `Server`'s) never unlinks its path.
        let _dead = UnixListener::bind(&path).unwrap();
    }
    assert!(fs::metadata(&path).unwrap().file_type().is_socket(), "setup: a real socket file is left behind");
    let server = Server::bind(&path).unwrap();
    let mut c = client(&path);
    c.write_all(&encode_request(&Request::Toggle)).unwrap();
    let mut conn = accepted(&server);
    assert!(matches!(until_done(&mut conn), Progress::Ready(Request::Toggle)), "the new listener actually serves connections");
}

#[test]
fn accept_returns_none_when_nobody_waits() {
    let d = dir("idle");
    let server = Server::bind(&sock(&d)).unwrap();
    // Nobody ever connects here: unlike every other test, there is no later
    // `accepted()` call to set this up, so it is done directly — a listener
    // left genuinely blocking (`set_nonblocking` dropped) would otherwise
    // wait forever, since nothing will ever show up to unblock it.
    set_rcvtimeo(server.fd(), WATCHDOG);
    assert!(not_blocking(|| server.accept().unwrap()).is_none(), "EAGAIN is not an error");
}

/// `Server::accept` is wired to
/// `peer_allowed` through `accept_with`, which lets a test fake a
/// mismatched peer uid without a second real uid to connect from (`peer_uid`
/// itself calls `getsockopt(SO_PEERCRED)`, unfakeable here).
#[test]
fn accept_with_refuses_a_mismatched_peer_uid() {
    let d = dir("peer-uid-mismatch");
    let server = Server::bind(&sock(&d)).unwrap();
    set_rcvtimeo(server.fd(), WATCHDOG);
    let mut c = client(&sock(&d));
    c.write_all(&encode_request(&Request::Toggle)).unwrap();
    let t = Instant::now();
    loop {
        let r = not_blocking(|| server.accept_with(|_| Ok(4242), 1000).unwrap());
        assert!(r.is_none(), "a mismatched peer uid must never be accepted");
        if t.elapsed() > Duration::from_millis(200) {
            break; // plenty of time for the kernel to have delivered the pending connection
        }
        thread::sleep(Duration::from_millis(5));
    }
    drop(c);
}

/// Same wiring, the other way: a peer uid matching `our_uid` is let through
/// (`peer_allowed`'s own logic, now proven reached through `accept_with`,
/// not just tested in isolation).
#[test]
fn accept_with_allows_a_matching_peer_uid() {
    let d = dir("peer-uid-match");
    let server = Server::bind(&sock(&d)).unwrap();
    set_rcvtimeo(server.fd(), WATCHDOG);
    let mut c = client(&sock(&d));
    c.write_all(&encode_request(&Request::Toggle)).unwrap();
    let t = Instant::now();
    let mut conn = loop {
        if let Some(conn) = not_blocking(|| server.accept_with(|_| Ok(1000), 1000).unwrap()) {
            break conn;
        }
        assert!(t.elapsed() < Duration::from_secs(2), "never accepted");
        thread::sleep(Duration::from_millis(5));
    };
    set_rcvtimeo(conn.fd(), WATCHDOG);
    assert!(matches!(until_done(&mut conn), Progress::Ready(Request::Toggle)));
}

#[test]
fn a_request_sent_byte_by_byte_arrives_whole() {
    let d = dir("bytes");
    let server = Server::bind(&sock(&d)).unwrap();
    let mut c = client(&sock(&d));
    let request = Request::Dmenu { prompt: Some("Pick: ".into()), lines: vec!["a b".into(), "é".into()], flags: DmenuFlags::default() };
    let bytes = encode_request(&request);
    let mut conn = accepted(&server);
    for (i, b) in bytes.iter().enumerate() {
        c.write_all(&[*b]).unwrap();
        if i + 1 < bytes.len() {
            assert!(matches!(not_blocking(|| conn.read()), Progress::Pending), "byte {i}: not a whole request yet");
        }
    }
    match until_done(&mut conn) {
        Progress::Ready(r) => assert_eq!(r, request),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_oversized_request_fails_and_is_answered_with_an_error() {
    let d = dir("big");
    let server = Server::bind(&sock(&d)).unwrap();
    let mut c = client(&sock(&d));
    c.write_all(format!("alpheus-launcher/1 dmenu - {}\n", MAX_DMENU_BYTES + 1).as_bytes()).unwrap();
    let mut conn = accepted(&server);
    let Progress::Failed(m) = until_done(&mut conn) else { panic!("an oversized body must fail at the header") };
    assert!(m.contains("too large"), "{m}");
    conn.reply(&Reply::Error(m));
    assert!(matches!(answer(c), Reply::Error(m) if m.contains("too large")));
}

#[test]
fn received_counts_the_bytes_read() {
    let d = dir("received");
    let server = Server::bind(&sock(&d)).unwrap();
    let mut c = client(&sock(&d));
    let mut conn = accepted(&server);
    assert_eq!(conn.received(), 0);
    c.write_all(b"alpheus-la").unwrap();
    let t = Instant::now();
    while conn.received() < 10 {
        assert!(matches!(not_blocking(|| conn.read()), Progress::Pending), "not a whole request yet");
        assert!(t.elapsed() < Duration::from_secs(2), "the bytes never arrived");
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(conn.received(), 10, "every byte read is counted, nothing more");
}

#[test]
fn a_client_closing_early_fails() {
    let d = dir("early");
    let server = Server::bind(&sock(&d)).unwrap();
    let mut c = client(&sock(&d));
    c.write_all(b"alpheus-launcher/1 dm").unwrap();
    drop(c);
    let mut conn = accepted(&server);
    let Progress::Failed(m) = until_done(&mut conn) else { panic!("EOF before a whole request") };
    assert!(m.contains("closed"), "{m}");
}

#[test]
fn a_request_never_finished_expires() {
    let d = dir("slow");
    let server = Server::bind(&sock(&d)).unwrap();
    let _c = client(&sock(&d));
    let conn = accepted(&server);
    assert!(!conn.expired(Instant::now()));
    assert!(conn.expired(Instant::now() + REQUEST_DEADLINE), "the deadline counts from the accept");
}

#[test]
fn replies_are_decodable_by_the_client() {
    let d = dir("replies");
    let server = Server::bind(&sock(&d)).unwrap();
    for r in [Reply::Done, Reply::Cancelled, Reply::Busy, Reply::Error("no".into())] {
        let mut c = client(&sock(&d));
        c.write_all(&encode_request(&Request::Toggle)).unwrap();
        let mut conn = accepted(&server);
        assert!(matches!(until_done(&mut conn), Progress::Ready(Request::Toggle)));
        conn.reply(&r);
        assert_eq!(answer(c), r);
    }
    // A dmenu answer goes through Pending.
    let mut c = client(&sock(&d));
    c.write_all(&encode_request(&Request::Dmenu { prompt: None, lines: vec!["x".into()], flags: DmenuFlags::default() })).unwrap();
    let mut conn = accepted(&server);
    assert!(matches!(until_done(&mut conn), Progress::Ready(Request::Dmenu { .. })));
    conn.into_pending().reply(&Reply::Chosen("é ok".into()));
    assert_eq!(answer(c), Reply::Chosen("é ok".into()));
}

#[test]
fn a_dmenu_client_hanging_up_is_detected() {
    let d = dir("hangup");
    let server = Server::bind(&sock(&d)).unwrap();
    let mut c = client(&sock(&d));
    c.write_all(&encode_request(&Request::Dmenu { prompt: None, lines: vec!["a".into()], flags: DmenuFlags::default() })).unwrap();
    let mut conn = accepted(&server);
    assert!(matches!(until_done(&mut conn), Progress::Ready(_)));
    let pending = conn.into_pending();
    assert!(!pending.hung_up(), "the client waits for its answer");
    c.shutdown(Shutdown::Write).unwrap();
    assert!(!pending.hung_up(), "a half-close (done sending) is not a hang-up: the client still reads");
    drop(c); // the script was killed (Ctrl-C, timeout)
    let t = Instant::now();
    while !pending.hung_up() {
        assert!(t.elapsed() < Duration::from_secs(1), "a closed client must be seen");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_reply_to_a_client_that_never_reads_gives_up_within_the_timeout() {
    let d = dir("stuck");
    let server = Server::bind(&sock(&d)).unwrap();
    let mut c = client(&sock(&d));
    c.write_all(&encode_request(&Request::Dmenu { prompt: None, lines: vec!["a".into()], flags: DmenuFlags::default() })).unwrap();
    let mut conn = accepted(&server);
    assert!(matches!(until_done(&mut conn), Progress::Ready(_)));
    let pending = conn.into_pending();
    // Far more than a socket buffer holds: every write after the first ones blocks.
    let huge = "x".repeat(MAX_DMENU_BYTES);
    let (tx, rx) = std::sync::mpsc::channel();
    let t = Instant::now();
    thread::spawn(move || {
        pending.reply(&Reply::Chosen(huge));
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(2)).expect("reply must give up, not block the event loop");
    // × 5: room for a loaded test machine (thread start, scheduling); a
    // write left without a timeout never returns at all (recv_timeout above).
    assert!(t.elapsed() < REPLY_TIMEOUT * 5, "bounded in total, not per write: {:?}", t.elapsed());
    drop(c);
}

#[test]
fn a_reply_to_a_vanished_client_is_only_logged() {
    let d = dir("vanished");
    let server = Server::bind(&sock(&d)).unwrap();
    let mut c = client(&sock(&d));
    c.write_all(&encode_request(&Request::Close)).unwrap();
    let mut conn = accepted(&server);
    assert!(matches!(until_done(&mut conn), Progress::Ready(Request::Close)));
    drop(c);
    conn.reply(&Reply::Done); // EPIPE: logged, never a panic nor a signal
}
