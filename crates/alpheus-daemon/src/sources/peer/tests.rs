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
