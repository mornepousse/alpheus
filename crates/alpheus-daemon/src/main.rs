//! The `alpheus-demon` binary: one call into `startup::run`, whose answer is the exit code.
//!
//! See: docs/cli.md (the exit codes), docs/ARCHITECTURE.md ("Entry points").

fn main() {
    std::process::exit(alpheus_daemon::startup::run());
}
