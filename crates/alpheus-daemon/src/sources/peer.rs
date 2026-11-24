//! Whose program listens on the compositor's socket: the pid the kernel reports for the peer of a
//! connection made and dropped at once.
//!
//! The kill list uses it to know the compositor's pid (a name is not enough: a Nix-wrapped
//! compositor runs under another name). It lives here, with the compositor's other sockets, because
//! `compositor_sockets_opened_only_in_sources` allows a connection nowhere else.
//!
//! Never:
//! - write a byte to the socket, or read from it: only the kernel's peer credentials are asked
//!   (`SO_PEERCRED`), then the connection is dropped;
//! - wait: the connection is non-blocking, and a socket that takes no connection at once gives None;
//! - keep the connection: it lives inside one call;
//! - log the pid or the path: nothing here logs.
//!
//! See: docs/features/tools-menu.md ("Kill a process"), docs/decisions.md ("Only the user's own
//! processes are offered, and a reused pid is never signalled").

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// A non-blocking connection to the socket at `path`, or None. It never waits: a compositor that
/// has stopped accepting (its queue full) is answered "no" at once, where a blocking `connect`
/// would hold the event loop, and so the whole bar, until it came back.
fn connect_without_waiting(path: &Path) -> Option<OwnedFd> {
    // SAFETY: socket(2) takes no pointer; its result is checked and wrapped in an `OwnedFd` at once.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is a descriptor just created and owned by nothing else.
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: an all-zero `sockaddr_un` is a valid value (integers and a byte array).
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    // One byte stays zero: the path's end.
    if bytes.is_empty() || bytes.len() >= address.sun_path.len() {
        return None;
    }
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length = size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: `address` is a valid `sockaddr_un` of `length` bytes and `socket` is open, both
    // alive for the whole call.
    let r = unsafe { libc::connect(socket.as_raw_fd(), (&address as *const libc::sockaddr_un).cast(), length) };
    // Done at once, or refused or "try again" (EAGAIN: the queue is full): never waited for.
    if r == 0 { Some(socket) } else { None }
}

/// The pid of the program listening on `socket`; None when nothing answers at once or the kernel
/// gives no usable pid (0, or 1 which would be init).
pub fn peer_pid(socket: &Path) -> Option<i32> {
    let stream = connect_without_waiting(socket)?;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt(2) writes at most `len` bytes into `cred`, a valid `ucred` owned by this
    // frame; the descriptor stays open until `stream` drops, after the call.
    let r = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len)
    };
    if r == 0 && cred.pid > 1 { Some(cred.pid) } else { None }
}

#[cfg(test)]
mod tests;
