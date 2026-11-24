//! The `alpheus-launch` binary: the arguments parsed, one request sent, its answer the exit code.
//!
//! See: docs/cli.md (the verbs and the exit codes).
//! `alpheus-launch`: see lib.rs.

use alpheus_common::launcher_ipc::socket_path;
use alpheus_common::paths::runtime_dir;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let code = alpheus_launch::run(
        &args,
        &socket_path(&runtime_dir()),
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::exit(code);
}
