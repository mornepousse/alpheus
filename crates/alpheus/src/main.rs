//! The `alpheus` program: reads the command line (`alpheus [--tab <name>]`) and runs the control
//! center. A bad option prints a message and exits with code 2; everything else is in the library
//! (`app::run`).
//!
//! Never:
//! - do anything else: a bad option is a message and exit code 2, and everything that could fail
//!   lives in the library so the tests can reach it.
//!
//! See: docs/cli.md (the options and the exit codes), docs/features/control-center.md.

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match alpheus::app::run::parse_options(&args) {
        Ok(options) => alpheus::app::run::run(options),
        Err(m) => {
            eprintln!("alpheus: {m}");
            std::process::exit(2);
        }
    }
}
