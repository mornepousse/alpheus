//! The one `poll(2)` wrapper: the compositor sockets, the D-Bus thread and the launcher's
//! hang-up check all wait through it, so the `unsafe` and the interrupted-call retry live once.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

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

/// A filesystem's size in blocks (the unit cancels out in the percentage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsBlocks {
    pub blocks: u64,
    pub free: u64,
    /// Free for an unprivileged user.
    pub avail: u64,
}

/// `statvfs(3)` of `path`; None when the call fails or the path has a NUL byte.
pub fn statvfs(path: &Path) -> Option<FsBlocks> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a NUL-terminated string living through the call; `st` is written in full on success.
    if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so the struct is initialised.
    let st = unsafe { st.assume_init() };
    // `fsblkcnt_t` is u32 on some targets and u64 on others: widen without a conversion clippy flags.
    #[allow(clippy::unnecessary_cast)]
    let widen = |n: libc::fsblkcnt_t| n as u64;
    Some(FsBlocks { blocks: widen(st.f_blocks), free: widen(st.f_bfree), avail: widen(st.f_bavail) })
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
        // SAFETY: installs a handler that does nothing: the signal is no longer fatal, and poll(2) is
        // never restarted by a handler (signal(7)), so it returns EINTR.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = nothing as extern "C" fn(libc::c_int) as usize;
            assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()), 0);
        }
        let (a, b) = UnixStream::pair().unwrap();
        // SAFETY: pthread_self has no preconditions.
        let me = unsafe { libc::pthread_self() };
        let helper = std::thread::spawn(move || {
            // Signals every 20 ms for 300 ms: one lands while the thread is waiting, however late it
            // reaches poll on a loaded machine.
            for _ in 0..15 {
                std::thread::sleep(Duration::from_millis(20));
                // SAFETY: the waiting thread is alive until it has read the word written below.
                unsafe { libc::pthread_kill(me, libc::SIGUSR1) };
            }
            std::io::Write::write_all(&mut &b, b"x").unwrap();
        });
        let mut fds = [watching(&a)];
        assert_eq!(poll(&mut fds, 5000).unwrap(), 1, "the signal did not end the wait");
        helper.join().unwrap();
    }

    #[test]
    fn statvfs_reads_a_real_filesystem_and_refuses_a_missing_path() {
        let fs = statvfs(&std::env::temp_dir()).expect("the temp dir's filesystem");
        assert!(fs.blocks > 0 && fs.free <= fs.blocks && fs.avail <= fs.free);
        assert_eq!(statvfs(Path::new("/nonexistent/alpheus")), None);
        assert_eq!(statvfs(Path::new("a\0b")), None);
    }
}
