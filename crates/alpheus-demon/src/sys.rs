//! The one `poll(2)` wrapper: the compositor sockets, the D-Bus thread and the launcher's
//! hang-up check all wait through it, so the `unsafe` and the interrupted-call retry live once.

use std::io;

/// `poll(2)` on `fds`, in milliseconds (`-1`: no timeout, `0`: a look), retried when a signal
/// interrupts it. The number of descriptors with a non-zero `revents`; the events are left in
/// `fds`.
pub fn poll(fds: &mut [libc::pollfd], timeout_ms: i32) -> io::Result<usize> {
    loop {
        // SAFETY: `fds` is a live slice of `pollfd` for the whole call, its length given with it.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if r >= 0 {
            return Ok(r as usize);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    extern "C" fn nothing(_: libc::c_int) {}

    fn watching(s: &UnixStream) -> libc::pollfd {
        libc::pollfd { fd: s.as_raw_fd(), events: libc::POLLIN, revents: 0 }
    }

    #[test]
    fn a_zero_timeout_only_looks() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut fds = [watching(&a)];
        assert_eq!(poll(&mut fds, 0).unwrap(), 0, "nothing to read yet");
        std::io::Write::write_all(&mut &b, b"x").unwrap();
        assert_eq!(poll(&mut fds, 0).unwrap(), 1);
        assert_ne!(fds[0].revents & libc::POLLIN, 0, "the events are left in the slice");
    }

    /// A signal arriving during the wait does not end it: the call is retried until the data comes.
    #[test]
    fn an_interrupted_wait_is_retried() {
        // SAFETY: installs a handler that does nothing, without SA_RESTART, so poll(2) returns EINTR.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = nothing as extern "C" fn(libc::c_int) as usize;
            assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()), 0);
        }
        let (a, b) = UnixStream::pair().unwrap();
        // SAFETY: pthread_self has no preconditions.
        let me = unsafe { libc::pthread_self() };
        let helper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            // SAFETY: the waiting thread is alive until it has read the word written below.
            unsafe { libc::pthread_kill(me, libc::SIGUSR1) };
            std::thread::sleep(Duration::from_millis(50));
            std::io::Write::write_all(&mut &b, b"x").unwrap();
        });
        let mut fds = [watching(&a)];
        assert_eq!(poll(&mut fds, 5000).unwrap(), 1, "the signal did not end the wait");
        helper.join().unwrap();
    }
}
