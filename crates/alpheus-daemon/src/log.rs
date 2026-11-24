//! The daemon's one log line: `log::write`, used everywhere instead of `eprintln!`, and the panic
//! hook beside it.
//!
//! `eprintln!` panics when the other end of standard error is closed (EPIPE, after an interrupted
//! `journalctl -f` for instance): a resident daemon must never die over something that small, so
//! the line goes out through `writeln!` and a failed write is ignored, nothing else being able to
//! act on it. `install_panic_hook` replaces the default hook, which prints a panic's message: a
//! message can quote private text (the notepad's note, a launcher query, a password), so this one
//! prints the thread and the place only.
//!
//! Never:
//! - use `eprintln!` or `eprint!` in the daemon's production code
//!   (`eprint_forbidden_outside_tests` fails the build on one);
//! - print a panic's message: `the_panic_hook_is_installed_before_anything_else` pins both the
//!   hook's forbidden forms and its place, first in `startup::run`;
//! - carry anything but fixed words and numbers in a line: each feature has its own log guardrail
//!   (`the_shield_logs_name_nothing`, `the_vpn_logs_name_no_profile`, and the rest);
//! - format a system error's own text (`{e}`, `{e:?}`), a program, an argument list or a panic's
//!   payload: `kind_word`, `error_word` and `exec_word` give the fixed word
//!   (`no_log_line_names_a_program_an_error_text_or_a_panic_payload` reads every production line).
//!
//! See: docs/decisions.md ("Logs carry fixed words and numbers only"), docs/cli.md ("Logs"),
//! AGENTS.md ("No private data in fixtures or logs").

use alpheus_common::exec::ExecError;
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

/// An `io::ErrorKind` as a fixed word for a log line. The OS message of an error can quote a path
/// or a name, so a line carries this word instead: anything unlisted is "io".
pub fn kind_word(kind: io::ErrorKind) -> &'static str {
    use io::ErrorKind as K;
    match kind {
        K::NotFound => "not found",
        K::PermissionDenied => "permission denied",
        K::TimedOut => "timeout",
        K::AlreadyExists => "already exists",
        K::InvalidInput | K::InvalidData => "invalid",
        K::ConnectionRefused | K::ConnectionReset | K::ConnectionAborted | K::NotConnected | K::BrokenPipe => "connection",
        K::AddrInUse | K::AddrNotAvailable => "address",
        _ => "io",
    }
}

/// An error as a fixed word for a log line, never its text. An `io::Error` anywhere in its chain
/// gives its kind's word (`kind_word`); anything else, "error".
pub fn error_word(e: &(dyn std::error::Error + 'static)) -> &'static str {
    let mut at: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = at {
        if let Some(io) = err.downcast_ref::<io::Error>() {
            return kind_word(io.kind());
        }
        at = err.source();
    }
    "error"
}

/// A failed start as a fixed word: never the error's text, which may carry a path.
pub fn exec_word(e: &ExecError) -> &'static str {
    match e {
        ExecError::NotFound => "not found",
        ExecError::Timeout => "timeout",
        ExecError::Io(_) => "io",
    }
}

/// A panic as the daemon logs it: the thread and the place, never the message.
fn panic_line(thread: Option<&str>, file: &str, line: u32, column: u32) -> String {
    format!("thread '{}' panicked at {file}:{line}:{column} (message not logged)", thread.unwrap_or("<unnamed>"))
}

/// Replaces the default panic hook, which prints the panic's message to standard error (this log):
/// a message can quote private text, such as the string a failed slice was cutting (the quick
/// notes, a password prompt). This one logs the thread and the place only, then the backtrace when
/// `RUST_BACKTRACE` asks for one (frames, no message). Unwinding, `catch_unwind` and a panicking
/// thread's end are unchanged: only the printed line differs.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let (file, line, column) = info.location().map_or(("<unknown>", 0, 0), |l| (l.file(), l.line(), l.column()));
        write(&panic_line(std::thread::current().name(), file, line, column));
        let trace = std::backtrace::Backtrace::capture();
        if trace.status() == std::backtrace::BacktraceStatus::Captured {
            write(&format!("backtrace:\n{trace}"));
        }
    }));
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

    #[test]
    fn an_error_logs_as_a_word_never_its_text() {
        let quoting = io::Error::new(io::ErrorKind::NotFound, "/home/someone/secret: No such file");
        assert_eq!(error_word(&quoting), "not found");
        assert_eq!(kind_word(io::ErrorKind::PermissionDenied), "permission denied");
        assert_eq!(kind_word(io::ErrorKind::TimedOut), "timeout");
        assert_eq!(kind_word(io::ErrorKind::Unsupported), "io");
        let other = io::Error::other("/home/someone/secret");
        assert_eq!(error_word(&other), "io");
        let text = std::fmt::Error;
        assert_eq!(error_word(&text), "error");
        assert_eq!(exec_word(&ExecError::Io("/home/someone/secret".into())), "io");
        assert_eq!(exec_word(&ExecError::NotFound), "not found");
        assert_eq!(exec_word(&ExecError::Timeout), "timeout");
    }

    /// The hook's line: the thread and the place, never the payload, which can embed private text
    /// (a boundary panic quotes the string it sliced).
    #[test]
    fn the_panic_line_names_the_thread_and_the_place() {
        assert_eq!(
            panic_line(Some("calloop"), "src/notes/text.rs", 12, 5),
            "thread 'calloop' panicked at src/notes/text.rs:12:5 (message not logged)"
        );
        assert_eq!(panic_line(None, "src/a.rs", 1, 2), "thread '<unnamed>' panicked at src/a.rs:1:2 (message not logged)");
    }
}
