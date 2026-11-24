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
//! - keep the connection: it lives inside one call;
//! - log the pid or the path: nothing here logs.
//!
//! See: docs/features/tools-menu.md ("Kill a process"), docs/decisions.md ("Only the user's own
//! processes are offered, and a reused pid is never signalled").

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// The pid of the program listening on `socket`; None when nothing answers or the kernel gives no
/// usable pid (0, or 1 which would be init).
pub fn peer_pid(socket: &Path) -> Option<i32> {
    let stream = UnixStream::connect(socket).ok()?;
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
