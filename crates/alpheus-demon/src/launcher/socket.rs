//! The daemon's side of the launcher socket (`launcher_ipc::socket_path`,
//! in $XDG_RUNTIME_DIR): owner-only (0600 and the peer's uid checked), read
//! without ever blocking, every request bounded by `launcher_ipc`'s limits
//! and by `REQUEST_DEADLINE`. Pure of calloop: launch.rs registers the file
//! descriptors and calls `accept`/`read`/`reply`; host.rs keeps a dmenu
//! client's `Pending` until the answer.
//!
//! Replies are written blocking but bounded: `REPLY_TIMEOUT` for the whole
//! reply, not per write call. A client gone before its reply makes `write`
//! fail with EPIPE: the Rust runtime ignores SIGPIPE in every binary, so it
//! is a logged error, never a dead daemon.
//!
//! Every reply path (`Conn::reply`, `Pending::reply`) shuts the connection
//! down right after sending, instead of leaving it to whichever caller
//! happens to drop the value last: `alpheus-launch` reads until EOF, and a
//! `--dmenu` client waits with no timeout of its own, so a reply that did
//! not also close the connection would hang a script forever if anything
//! ever kept another handle to the same fd open (`finish`'s unit test below
//! proves this with a duplicated fd, not only by relying on `Drop`).

use crate::log::write as log;
use alpheus_commun::launcher_ipc::{IpcError, Reply, Request, RequestDecoder, encode_reply};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const REQUEST_DEADLINE: Duration = Duration::from_secs(2);
pub const REPLY_TIMEOUT: Duration = Duration::from_millis(200);
pub const MAX_CONNECTIONS: usize = 8;
/// Bytes asked of the kernel per read; the decoder enforces the real limits.
const CHUNK: usize = 16 * 1024;

/// The socket is the user's: only their own processes may talk to it (not
/// even root, which has other ways in anyway).
pub fn peer_allowed(peer_uid: u32, our_uid: u32) -> bool {
    peer_uid == our_uid
}

fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt(2) writes at most `len` bytes into `cred`, a valid
    // `ucred` owned by this frame; the fd is open for the whole call.
    let r = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len)
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

fn describe(e: &IpcError) -> String {
    match e {
        IpcError::TooLarge(what) => format!("request too large ({what})"),
        IpcError::Malformed(m) => format!("malformed request: {m}"),
        IpcError::Truncated => "truncated request".to_string(),
    }
}

/// Writes all of `bytes` within `limit` IN ALL (the write timeout is
/// re-armed with what is left before each call), blocking.
fn send_within(stream: &UnixStream, bytes: &[u8], limit: Duration) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let deadline = Instant::now() + limit;
    let mut rest = bytes;
    while !rest.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "reply timed out"));
        }
        stream.set_write_timeout(Some(left))?;
        match (&*stream).write(rest) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => rest = &rest[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Sends the reply (bounded), then shuts the connection down both ways: the
/// client must see EOF now, not whenever the caller happens to drop its
/// last handle to this fd (see the module doc).
fn finish(stream: UnixStream, r: &Reply) {
    if let Err(e) = send_within(&stream, &encode_reply(r), REPLY_TIMEOUT) {
        log(&format!("launcher socket: reply not delivered: {e}"));
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// Listening socket; Drop unlinks the file.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    /// Removes a stale file (the instance lock guarantees no other daemon), binds, chmod 0600, non-blocking.
    pub fn bind(path: &Path) -> io::Result<Server> {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(path)?;
        // Built before the chmod: if it fails, Drop still unlinks the file.
        let server = Server { listener, path: path.to_path_buf() };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        server.listener.set_nonblocking(true)?;
        Ok(server)
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }

    /// Ok(None): nothing pending (EAGAIN). A peer with another uid is dropped (logged) → Ok(None).
    pub fn accept(&self) -> io::Result<Option<Conn>> {
        let stream = match self.listener.accept() {
            Ok((s, _)) => s,
            // ECONNABORTED: that one client gave up before the accept; the
            // listener is fine (the next pass takes the others).
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted) => {
                return Ok(None);
            }
            // Anything else (EMFILE, ENFILE, ENOBUFS…) stays pending: the
            // caller pauses the listener (launch.rs, `drain_listener`).
            Err(e) => return Err(e),
        };
        // SAFETY: getuid(2) cannot fail.
        let ours = unsafe { libc::getuid() };
        match peer_uid(&stream) {
            Ok(uid) if peer_allowed(uid, ours) => {}
            Ok(uid) => {
                log(&format!("launcher socket: peer uid {uid} refused"));
                return Ok(None);
            }
            Err(e) => {
                log(&format!("launcher socket: peer credentials unreadable: {e}"));
                return Ok(None);
            }
        }
        // accept4(2) does not pass O_NONBLOCK on to the new socket.
        stream.set_nonblocking(true)?;
        Ok(Some(Conn { stream, decoder: RequestDecoder::new(), accepted_at: Instant::now(), received: 0 }))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One accepted connection being read.
pub struct Conn {
    stream: UnixStream,
    decoder: RequestDecoder,
    accepted_at: Instant,
    received: usize,
}

#[derive(Debug)]
pub enum Progress {
    Pending,
    Ready(Request),
    Failed(String),
}

impl Conn {
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    /// Reads what is available without blocking; EOF before a whole request → Failed.
    pub fn read(&mut self) -> Progress {
        let mut buf = [0u8; CHUNK];
        loop {
            match (&self.stream).read(&mut buf) {
                Ok(0) => return Progress::Failed("connection closed before a whole request".to_string()),
                Ok(n) => {
                    self.received += n;
                    match self.decoder.feed(&buf[..n]) {
                        Ok(Some(r)) => return Progress::Ready(r),
                        Ok(None) => {}
                        Err(e) => return Progress::Failed(describe(&e)),
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Progress::Pending,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Progress::Failed(format!("read: {e}")),
            }
        }
    }

    pub fn received(&self) -> usize {
        self.received
    }

    pub fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.accepted_at) >= REQUEST_DEADLINE
    }

    /// Blocking write with REPLY_TIMEOUT, then shutdown; errors logged.
    pub fn reply(self, r: &Reply) {
        finish(self.stream, r);
    }

    /// Keeps the connection for a dmenu answer.
    pub fn into_pending(self) -> Pending {
        Pending { stream: self.stream }
    }
}

/// A dmenu client waiting for its answer.
pub struct Pending {
    stream: UnixStream,
}

impl Pending {
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    /// True once the client CLOSED its end: POLLHUP (both directions shut),
    /// POLLERR or POLLNVAL. A half-close (`shutdown(SHUT_WR)` after sending,
    /// which only raises POLLRDHUP and makes reads return 0) is NOT a
    /// hang-up: that client still waits for its answer.
    pub fn hung_up(&self) -> bool {
        let mut p = libc::pollfd { fd: self.stream.as_raw_fd(), events: 0, revents: 0 };
        // SAFETY: poll(2) on one valid pollfd owned by this frame, timeout 0
        // (never blocks); the fd stays open for the whole call.
        let r = unsafe { libc::poll(&mut p, 1, 0) };
        r > 0 && p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    }

    pub fn reply(self, r: &Reply) {
        finish(self.stream, r);
    }

    /// A Pending and the client's end, for unit tests elsewhere in the crate
    /// (surface.rs): no listening socket, no `connect`.
    #[cfg(test)]
    pub(crate) fn pair() -> (Pending, UnixStream) {
        let (ours, theirs) = UnixStream::pair().expect("socketpair");
        (Pending { stream: ours }, theirs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn only_the_same_uid_is_allowed() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(0, 1000), "not even root: the socket is the user's");
        assert!(!peer_allowed(1001, 1000));
    }

    #[test]
    fn pair_gives_a_pending_whose_peer_reads_the_reply() {
        let (pending, mut peer) = Pending::pair();
        pending.reply(&Reply::Cancelled);
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).unwrap();
        assert_eq!(alpheus_commun::launcher_ipc::decode_reply(&bytes).unwrap(), Reply::Cancelled);
    }

    /// The contract task 2's review relied on: a reply closes the
    /// connection right there, not whenever the last handle to the fd
    /// happens to drop. Proven with a genuinely independent duplicate of
    /// the server's fd kept alive past the call: with `close()`/`Drop`
    /// alone (no reference count at zero), the peer would never see EOF
    /// and this read would time out instead of completing.
    #[test]
    fn a_reply_closes_the_connection_even_if_another_handle_to_the_fd_is_still_open() {
        let (pending, mut peer) = Pending::pair();
        let _dup = pending.stream.try_clone().expect("dup the server's fd");
        pending.reply(&Reply::Done);
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).expect("the client must see EOF (and so the full reply) despite the open duplicate");
        assert_eq!(alpheus_commun::launcher_ipc::decode_reply(&bytes).unwrap(), Reply::Done);
    }
}
