//! The daemon's only executor. `Executor::run` waits for its command (up to
//! 2 s): on the event loop, every bar would freeze. Nothing in the daemon
//! needs it — clicks `spawn`, the sound source `stream`s on its own thread —
//! so this wrapper refuses it outright, without even trying. `capture` blocks
//! too, up to its deadline: only the launcher's scan thread calls it (a
//! guardrail), so it is delegated.

use crate::log::write as log;
use alpheus_commun::exec::{CaptureLimits, Captured, ExecError, Executor, Output, Stream};

pub const REFUSED: &str = "Executor::run refused: it blocks, and the daemon must never block its event loop";

pub struct NonBlocking<E: Executor>(pub E);

impl<E: Executor> Executor for NonBlocking<E> {
    fn run(&self, prog: &str, _args: &[&str]) -> Result<Output, ExecError> {
        log(&format!("{prog}: {REFUSED}"));
        Err(ExecError::Io(REFUSED.into()))
    }

    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        self.0.spawn(prog, args)
    }

    fn spawn_with_input(&self, prog: &str, args: &[&str], input: &[&[u8]]) -> Result<(), ExecError> {
        self.0.spawn_with_input(prog, args, input)
    }

    fn spawn_with_env(&self, prog: &str, args: &[&str], env: &[(&str, &str)]) -> Result<(), ExecError> {
        self.0.spawn_with_env(prog, args, env)
    }

    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError> {
        self.0.stream(prog, args)
    }

    fn capture(&self, prog: &str, args: &[&str], limits: CaptureLimits) -> Result<Captured, ExecError> {
        self.0.capture(prog, args, limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpheus_commun::exec::CaptureEnd;
    use alpheus_commun::exec::fake::FakeExecutor;
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
    fn capture_is_delegated() {
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
        assert_eq!(e.spawn_with_env("hook-x", &["a"], &[("ALPHEUS_EVENT", "dnd")]), Ok(()));
        assert_eq!(e.0.envs(), [("hook-x a".to_string(), vec![("ALPHEUS_EVENT".to_string(), "dnd".to_string())])]);
    }
}
