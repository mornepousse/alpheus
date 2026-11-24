//! The daemon's only executor: a wrapper that refuses the one call that blocks.
//!
//! `Executor::run` waits for the command it starts, up to two seconds. On the event loop that
//! would freeze every bar on every screen, and nothing in the daemon needs it: a click `spawn`s,
//! the sound source `stream`s on its own thread. So this wrapper answers an error without even
//! trying, and logs which program asked. `capture` blocks too, up to its own deadline, and
//! `run_silent` waits: both are delegated, because the only callers are the launcher's scan thread
//! and the Shield's worker, each on a thread of its own.
//!
//! Never:
//! - be bypassed: `the_only_system_executor_is_wrapped_in_non_blocking` checks that the one system
//!   executor the daemon builds is built inside `NonBlocking(…)`;
//! - let a second command stream: `the_only_stream_is_pw_dump_monitor` pins the one `stream` call,
//!   the sound source's `pw-dump`;
//! - let anything but those two runners capture, or either of them through a shell
//!   (`only_the_runners_capture_and_never_through_a_shell`);
//! - start a command with an environment anywhere but the hooks
//!   (`spawn_with_env_only_runs_hooks`).
//!
//! See: docs/ARCHITECTURE.md ("The foundation and the client"), docs/features/hooks.md,
//! docs/features/launcher.md (the scan thread), docs/features/shield.md (the worker).

use crate::log::write as log;
use alpheus_common::exec::{CaptureLimits, Captured, ExecError, Executor, Output, Stream};

pub const REFUSED: &str = "Executor::run refused: it blocks, and the daemon must never block its event loop";

pub struct NonBlocking<E: Executor>(pub E);

impl<E: Executor> Executor for NonBlocking<E> {
    fn run(&self, _prog: &str, _args: &[&str]) -> Result<Output, ExecError> {
        log(REFUSED);
        Err(ExecError::Io(REFUSED.into()))
    }

    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        self.0.spawn(prog, args)
    }

    fn spawn_with_input(&self, prog: &str, args: &[&str], input: &[&[u8]]) -> Result<(), ExecError> {
        self.0.spawn_with_input(prog, args, input)
    }

    fn spawn_with_env(&self, prog: &str, args: &[&str], env: &[(&str, &str)], unset: &[&str]) -> Result<(), ExecError> {
        self.0.spawn_with_env(prog, args, env, unset)
    }

    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError> {
        self.0.stream(prog, args)
    }

    fn capture(&self, prog: &str, args: &[&str], limits: CaptureLimits) -> Result<Captured, ExecError> {
        self.0.capture(prog, args, limits)
    }

    fn run_silent(&self, prog: &str, args: &[&str]) -> Result<Option<i32>, ExecError> {
        self.0.run_silent(prog, args)
    }

    fn stop_running(&self) {
        self.0.stop_running();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpheus_common::exec::CaptureEnd;
    use alpheus_common::exec::fake::FakeExecutor;
    use std::time::Duration;

    #[test]
    fn run_is_refused_and_never_reaches_the_executor() {
        let fake = FakeExecutor::new();
        fake.ok("wpctl status", "ok");
        let e = NonBlocking(fake);
        assert_eq!(e.run("wpctl", &["status"]), Err(ExecError::Io(REFUSED.into())));
        assert!(e.0.calls().is_empty(), "run must not even be attempted: {:?}", e.0.calls());
    }

    #[test]
    fn spawn_and_stream_are_delegated() {
        let fake = FakeExecutor::new();
        fake.ok("alpheus-toggle sound", "");
        fake.stream_ok("pw-dump -m -N -R", b"[]");
        let e = NonBlocking(fake);
        assert_eq!(e.spawn("alpheus-toggle", &["sound"]), Ok(()));
        assert!(e.stream("pw-dump", &["-m", "-N", "-R"]).is_ok());
        assert_eq!(e.0.calls(), ["alpheus-toggle sound", "pw-dump -m -N -R"]);
    }

    #[test]
    fn capture_is_delegated_to_the_executor() {
        let fake = FakeExecutor::new();
        let c = Captured { stdout: b"a\n".to_vec(), end: CaptureEnd::Exited(Some(0)) };
        fake.capture_ok("fake-list", c.clone());
        let e = NonBlocking(fake);
        let l = CaptureLimits { deadline: Duration::from_secs(1), max_bytes: 10, max_lines: 10 };
        assert_eq!(e.capture("fake-list", &[], l), Ok(c));
    }

    #[test]
    fn spawn_with_input_is_delegated() {
        let fake = FakeExecutor::new();
        fake.ok("nmcli --ask device wifi connect X", "");
        let e = NonBlocking(fake);
        assert_eq!(e.spawn_with_input("nmcli", &["--ask", "device", "wifi", "connect", "X"], &[b"fake-pass", b"\n"]), Ok(()));
        assert_eq!(e.0.inputs(), [("nmcli --ask device wifi connect X".to_string(), b"fake-pass\n".to_vec())]);
    }

    #[test]
    fn spawn_with_env_is_delegated() {
        let fake = FakeExecutor::new();
        fake.ok("hook-x a", "");
        let e = NonBlocking(fake);
        assert_eq!(e.spawn_with_env("hook-x", &["a"], &[("ALPHEUS_EVENT", "dnd")], &["ALPHEUS_DND"]), Ok(()));
        assert_eq!(e.0.unsets(), [vec!["ALPHEUS_DND".to_string()]]);
        assert_eq!(e.0.envs(), [("hook-x a".to_string(), vec![("ALPHEUS_EVENT".to_string(), "dnd".to_string())])]);
    }
}
