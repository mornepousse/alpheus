//! Tests of `peer_pid`, on a socket this test listens on itself: never the session's compositor.

use super::*;
use crate::launcher::testing::TempDir;
use std::os::unix::net::UnixListener;

#[test]
fn the_pid_is_the_peer_of_the_socket() {
    let dir = TempDir::new("peer-pid-socket");
    let path = dir.join("wm.sock");
    let _listener = UnixListener::bind(&path).unwrap();
    // This process listens, so the peer's credentials are ours.
    assert_eq!(peer_pid(&path), Some(std::process::id() as i32));
}

#[test]
fn a_socket_nobody_listens_on_gives_nothing() {
    let dir = TempDir::new("peer-pid-absent");
    assert_eq!(peer_pid(&dir.join("absent.sock")), None);
}

/// A listening socket that nobody accepts from and whose backlog is already full: a blocking
/// `connect` waits for room, a non-blocking one answers at once.
fn wedged_listener(path: &Path) -> OwnedFd {
    // SAFETY: plain socket(2)/bind(2)/listen(2)/connect(2) calls on a descriptor and a sockaddr this
    // function owns for their whole duration; the fd is wrapped in an `OwnedFd` right away.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0);
        let owned = OwnedFd::from_raw_fd(fd);
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (i, b) in path.as_os_str().as_bytes().iter().enumerate() {
            addr.sun_path[i] = *b as libc::c_char;
        }
        let len = size_of::<libc::sockaddr_un>() as libc::socklen_t;
        assert_eq!(libc::bind(owned.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len), 0);
        assert_eq!(libc::listen(owned.as_raw_fd(), 0), 0);
        // Fill the queue without blocking: the first connections are accepted into it, then it is full.
        for _ in 0..4 {
            let c = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
            libc::connect(c, (&addr as *const libc::sockaddr_un).cast(), len);
            // Left open on purpose: closing would not free the slot for this test's purpose.
            std::mem::forget(OwnedFd::from_raw_fd(c));
        }
        owned
    }
}

#[test]
fn a_wedged_compositor_never_blocks_the_caller() {
    let dir = TempDir::new("peer-pid-wedged");
    let path = dir.join("wedged.sock");
    let _listener = wedged_listener(&path);
    let (tx, rx) = std::sync::mpsc::channel();
    let probe = path.clone();
    // On its own thread so that a blocking connect fails this test instead of hanging it.
    std::thread::spawn(move || {
        let _ = tx.send(peer_pid(&probe));
    });
    let answer = rx.recv_timeout(std::time::Duration::from_secs(2)).expect("peer_pid waited on a full backlog");
    assert_eq!(answer, None, "a socket that takes no connection gives no pid");
}
