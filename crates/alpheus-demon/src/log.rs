//! The daemon's log: a single function, used everywhere instead of
//! `eprintln!`. `eprintln!` panics if the other end of standard error is
//! closed (EPIPE — an interrupted `journalctl -f`, e.g.): a resident daemon
//! must never die over something this small. `writeln!` returns a plain
//! error that we ignore (nothing else could act on it anyway).

use std::io::{self, Write};

/// Testable part, separate from `io::stderr()`: a test can hand it a pipe
/// end already closed for reading (EPIPE guaranteed on write) without
/// touching the test process's real standard error, shared by every test
/// thread running at the same time.
fn write_to(out: &mut impl Write, m: &str) {
    let _ = writeln!(out, "alpheus-demon: {m}");
}

pub fn write(m: &str) {
    write_to(&mut io::stderr(), m);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::FromRawFd;

    #[test]
    fn write_survives_a_real_epipe() {
        // A real pipe, its read end closed right away: the next write on
        // the write end returns EPIPE, for real (not a simulation) —
        // without touching the test process's real fd 2 (see `write_to`).
        let mut fds = [0i32; 2];
        // SAFETY: pipe(2), a valid array of two `i32`, correct size.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2) failed");
        let (read, write_end) = (fds[0], fds[1]);
        // SAFETY: `read` just came back from `pipe(2)` a moment ago, never
        // yet closed nor owned by anyone else.
        unsafe { libc::close(read) };
        // SAFETY: `write_end` just came back from `pipe(2)` a moment ago;
        // `File` becomes its sole owner (closed by its `Drop`).
        let mut broken_pipe = unsafe { File::from_raw_fd(write_end) };
        write_to(&mut broken_pipe, "this message must never make the daemon panic");
        // Nothing more to check: if `write_to` had used `writeln!` directly
        // (without `let _ =`), the line above would already have panicked.
    }
}
