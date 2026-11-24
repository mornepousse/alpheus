//! Fake executor for tests: programmed responses, logged calls. An
//! unprogrammed command replies "not found": a test can NEVER reach a real tool.
//!
//! Never:
//! - reach a real tool: an unprogrammed command answers "not found", so a test that forgets one
//!   fails instead of touching the machine (AGENTS.md: never call a session tool for real);
//! - forget a call: every one is recorded, which is how the tests assert what would have run.
//!
//! See: CONTRIBUTING.md (fakes), AGENTS.md ("Rules for agents"), docs/ARCHITECTURE.md ("Tests").

use super::{CaptureEnd, CaptureLimits, Captured, ExecError, Executor, Output, Stream, Watched};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How the fake answers a `spawn_watched` of one program: started (ending with this code when
/// interrupted, or not by itself), or the error.
type WatchAnswer = Result<Option<Option<i32>>, ExecError>;

/// A `spawn_with_env` as recorded: its line and its variables.
pub type EnvCall = (String, Vec<(String, String)>);

#[derive(Default)]
pub struct FakeExecutor {
    responses: Mutex<HashMap<String, Result<Output, ExecError>>>,
    streams: Mutex<HashMap<String, Result<Vec<u8>, ExecError>>>,
    captures: Mutex<HashMap<String, Result<Captured, ExecError>>>,
    calls: Mutex<Vec<String>>,
    stops: Mutex<usize>,
    inputs: Mutex<Vec<(String, Vec<u8>)>>,
    envs: Mutex<Vec<EnvCall>>,
    unsets: Mutex<Vec<Vec<String>>>,
    delay: Duration,
    limits: Mutex<Vec<(String, CaptureLimits)>>,
    /// Per program: how a `spawn_watched` of it answers (Ok: started, ending with this code on an
    /// interrupt, or not by itself).
    watched: Mutex<HashMap<String, WatchAnswer>>,
    children: Mutex<Vec<FakeChild>>,
}

impl FakeExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every `run` and `capture` call takes `delay` (simulates a slow tool).
    pub fn with_delay(delay: Duration) -> Self {
        FakeExecutor { delay, ..Self::default() }
    }

    /// Programs the response to the exact command line "prog arg1 arg2".
    pub fn respond(&self, cmdline: &str, r: Result<Output, ExecError>) {
        self.responses.lock().unwrap().insert(cmdline.to_string(), r);
    }

    /// Shortcut: success with this standard output.
    pub fn ok(&self, cmdline: &str, stdout: &str) {
        self.respond(cmdline, Ok(Output::ok(stdout)));
    }

    /// Command lines received (run, spawn, stream and capture), in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Each `spawn_with_input`'s line and its input, the parts joined (fake passwords only); a
    /// `run_with_stdin_file`'s too, with the file's content.
    pub fn inputs(&self) -> Vec<(String, Vec<u8>)> {
        self.inputs.lock().unwrap().clone()
    }

    /// Each `spawn_with_env`'s line and its variables, in order.
    pub fn envs(&self) -> Vec<EnvCall> {
        self.envs.lock().unwrap().clone()
    }

    /// Each `spawn_with_env`'s removed names, in order (parallel to `envs`).
    pub fn unsets(&self) -> Vec<Vec<String>> {
        self.unsets.lock().unwrap().clone()
    }

    /// How many times `stop_running` was called.
    pub fn stops(&self) -> usize {
        *self.stops.lock().unwrap()
    }

    /// Each `capture`'s line and the limits it was given, in order.
    pub fn limits(&self) -> Vec<(String, CaptureLimits)> {
        self.limits.lock().unwrap().clone()
    }

    /// The limits each `capture` was given, in order.
    pub fn capture_limits(&self) -> Vec<CaptureLimits> {
        self.limits.lock().unwrap().iter().map(|(_, l)| *l).collect()
    }

    /// Programs the output of the stream "prog arg1 arg2" (then its end).
    pub fn stream_ok(&self, cmdline: &str, output: &[u8]) {
        self.streams.lock().unwrap().insert(cmdline.to_string(), Ok(output.to_vec()));
    }

    /// Programs a failure to start the stream "prog arg1 arg2".
    pub fn stream_err(&self, cmdline: &str, e: ExecError) {
        self.streams.lock().unwrap().insert(cmdline.to_string(), Err(e));
    }

    /// Programs the capture of "prog arg1 arg2".
    pub fn capture_ok(&self, cmdline: &str, captured: Captured) {
        self.captures.lock().unwrap().insert(cmdline.to_string(), Ok(captured));
    }

    /// Programs a failure to start the capture of "prog arg1 arg2".
    pub fn capture_err(&self, cmdline: &str, e: ExecError) {
        self.captures.lock().unwrap().insert(cmdline.to_string(), Err(e));
    }

    /// Lets `spawn_watched` start the program `prog`, whatever its arguments (they hold temporary
    /// paths): the child runs until the test ends it (`FakeChild::end`), or until an interrupt when
    /// `on_interrupt` is the end it then comes to.
    pub fn watch_ok(&self, prog: &str, on_interrupt: Option<Option<i32>>) {
        self.watched.lock().unwrap().insert(prog.to_string(), Ok(on_interrupt));
    }

    /// Programs a failure to start the watched program `prog`.
    pub fn watch_err(&self, prog: &str, e: ExecError) {
        self.watched.lock().unwrap().insert(prog.to_string(), Err(e));
    }

    /// The children `spawn_watched` started, in order.
    pub fn children(&self) -> Vec<FakeChild> {
        self.children.lock().unwrap().clone()
    }
}

/// One child of the fake's `spawn_watched`: what it was started with, and what was done to it. The
/// test ends it; nothing runs.
#[derive(Clone, Default)]
pub struct FakeChild(Arc<Mutex<FakeChildState>>);

#[derive(Default)]
struct FakeChildState {
    line: String,
    stdout: Option<PathBuf>,
    ended: Option<Option<i32>>,
    on_interrupt: Option<Option<i32>>,
    interrupts: usize,
    waits: Vec<Duration>,
    dropped: bool,
}

impl FakeChild {
    /// "prog arg1 arg2", as started.
    pub fn line(&self) -> String {
        self.0.lock().unwrap().line.clone()
    }

    /// The file its standard output went to.
    pub fn stdout(&self) -> Option<PathBuf> {
        self.0.lock().unwrap().stdout.clone()
    }

    /// The program ends: `code`, None for a signal.
    pub fn end(&self, code: Option<i32>) {
        self.0.lock().unwrap().ended = Some(code);
    }

    /// How many interrupts reached it while it ran.
    pub fn interrupts(&self) -> usize {
        self.0.lock().unwrap().interrupts
    }

    /// The limit of each `wait_exit`, in order.
    pub fn waits(&self) -> Vec<Duration> {
        self.0.lock().unwrap().waits.clone()
    }

    /// Whether its owner dropped it while it still ran (the real one is then killed).
    pub fn killed(&self) -> bool {
        let s = self.0.lock().unwrap();
        s.dropped && s.ended.is_none()
    }
}

/// The handle `spawn_watched` returns: a `FakeChild` seen from its owner.
struct FakeWatched(FakeChild);

impl Watched for FakeWatched {
    fn exit_fd(&self) -> Option<std::os::fd::OwnedFd> {
        None
    }

    fn try_exit(&mut self) -> Option<Option<i32>> {
        self.0.0.lock().unwrap().ended
    }

    fn interrupt(&mut self) -> bool {
        let mut s = self.0.0.lock().unwrap();
        if s.ended.is_some() {
            return false;
        }
        s.interrupts += 1;
        if let Some(code) = s.on_interrupt {
            s.ended = Some(code);
        }
        true
    }

    fn wait_exit(&mut self, limit: Duration) -> Option<Option<i32>> {
        let mut s = self.0.0.lock().unwrap();
        s.waits.push(limit);
        s.ended
    }
}

impl Drop for FakeWatched {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().dropped = true;
    }
}

fn line(prog: &str, args: &[&str]) -> String {
    std::iter::once(prog).chain(args.iter().copied()).collect::<Vec<_>>().join(" ")
}

impl Executor for FakeExecutor {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.responses.lock().unwrap().get(&l).cloned().unwrap_or(Err(ExecError::NotFound))
    }

    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        match self.responses.lock().unwrap().get(&l).cloned() {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err(ExecError::NotFound),
        }
    }

    fn spawn_with_input(&self, prog: &str, args: &[&str], input: &[&[u8]]) -> Result<(), ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        self.inputs.lock().unwrap().push((l.clone(), input.concat()));
        match self.responses.lock().unwrap().get(&l).cloned() {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err(ExecError::NotFound),
        }
    }

    fn run_with_stdin_file(&self, prog: &str, args: &[&str], file: &Path, _limit: Duration) -> Result<Option<i32>, ExecError> {
        let bytes = std::fs::read(file).map_err(|e| ExecError::Io(e.to_string()))?;
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        self.inputs.lock().unwrap().push((l.clone(), bytes));
        match self.responses.lock().unwrap().get(&l).cloned() {
            Some(Ok(output)) => Ok(output.code),
            Some(Err(e)) => Err(e),
            None => Err(ExecError::NotFound),
        }
    }

    fn spawn_with_env(&self, prog: &str, args: &[&str], env: &[(&str, &str)], unset: &[&str]) -> Result<(), ExecError> {
        if !super::env_ok(env) || unset.iter().any(|k| k.is_empty() || k.contains(['=', '\0'])) {
            return Err(ExecError::Io("invalid environment variable".into()));
        }
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        self.unsets.lock().unwrap().push(unset.iter().map(|k| k.to_string()).collect());
        self.envs.lock().unwrap().push((l.clone(), env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()));
        match self.responses.lock().unwrap().get(&l).cloned() {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err(ExecError::NotFound),
        }
    }

    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        match self.streams.lock().unwrap().get(&l).cloned() {
            Some(Ok(bytes)) => Ok(Box::new(std::io::Cursor::new(bytes))),
            Some(Err(e)) => Err(e),
            None => Err(ExecError::NotFound),
        }
    }

    fn capture(&self, prog: &str, args: &[&str], limits: CaptureLimits) -> Result<Captured, ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        self.limits.lock().unwrap().push((l.clone(), limits));
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.captures.lock().unwrap().get(&l).cloned().unwrap_or(Err(ExecError::NotFound))
    }

    /// Answers from the programmed captures: an `Exited` code is the code, anything else `None`.
    fn run_silent(&self, prog: &str, args: &[&str]) -> Result<Option<i32>, ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        match self.captures.lock().unwrap().get(&l).cloned() {
            Some(Ok(c)) => Ok(match c.end {
                CaptureEnd::Exited(code) => code,
                _ => None,
            }),
            Some(Err(e)) => Err(e),
            None => Err(ExecError::NotFound),
        }
    }

    fn stop_running(&self) {
        *self.stops.lock().unwrap() += 1;
    }

    /// Answers as `watch_ok` or `watch_err` programmed the program; an unprogrammed one is not found.
    fn spawn_watched(&self, prog: &str, args: &[&str], stdout: Option<&Path>) -> Result<Box<dyn Watched>, ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        let on_interrupt = match self.watched.lock().unwrap().get(prog).cloned() {
            Some(Ok(on_interrupt)) => on_interrupt,
            Some(Err(e)) => return Err(e),
            None => return Err(ExecError::NotFound),
        };
        let child = FakeChild::default();
        {
            let mut s = child.0.lock().unwrap();
            s.line = l;
            s.stdout = stdout.map(Path::to_path_buf);
            s.on_interrupt = on_interrupt;
        }
        self.children.lock().unwrap().push(child.clone());
        Ok(Box::new(FakeWatched(child)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::CaptureEnd;
    use std::time::Instant;

    #[test]
    fn unprogrammed_command_not_found_and_logged() {
        let f = FakeExecutor::new();
        assert_eq!(f.run("wpctl", &["status"]), Err(ExecError::NotFound));
        assert_eq!(f.calls(), vec!["wpctl status".to_string()]);
    }

    #[test]
    fn programmed_response() {
        let f = FakeExecutor::new();
        f.ok("nmcli -t radio wifi", "enabled\n");
        f.respond("bluetoothctl show", Err(ExecError::Timeout));
        assert_eq!(f.run("nmcli", &["-t", "radio", "wifi"]).unwrap().stdout, "enabled\n");
        assert_eq!(f.run("bluetoothctl", &["show"]), Err(ExecError::Timeout));
    }

    #[test]
    fn spawn_logged() {
        let f = FakeExecutor::new();
        f.ok("screen-lock", "");
        assert_eq!(f.spawn("screen-lock", &[]), Ok(()));
        assert_eq!(f.spawn("unknown", &[]), Err(ExecError::NotFound));
        assert_eq!(f.calls(), vec!["screen-lock".to_string(), "unknown".to_string()]);
    }

    #[test]
    fn simulated_delay() {
        let f = FakeExecutor::with_delay(Duration::from_millis(50));
        let t = Instant::now();
        let _ = f.run("wpctl", &["status"]);
        assert!(t.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn fake_capture_honours_the_delay() {
        let f = FakeExecutor::with_delay(Duration::from_millis(200));
        let l = CaptureLimits { deadline: Duration::from_secs(1), max_bytes: 10, max_lines: 10 };
        let t = Instant::now();
        let _ = f.capture("journalctl", &[], l);
        assert!(t.elapsed() >= Duration::from_millis(200));
    }

    #[test]
    fn fake_records_capture_limits() {
        let f = FakeExecutor::new();
        let a = CaptureLimits { deadline: Duration::from_secs(2), max_bytes: 1 << 20, max_lines: 1001 };
        let b = CaptureLimits { deadline: Duration::from_secs(5), max_bytes: 4 << 20, max_lines: 501 };
        let _ = f.capture("journalctl", &["-n", "1"], a);
        let _ = f.capture("coredumpctl", &[], b);
        assert_eq!(f.limits(), [("journalctl -n 1".to_string(), a), ("coredumpctl".to_string(), b)]);
    }

    #[test]
    fn programmed_stream() {
        use std::io::Read;
        let f = FakeExecutor::new();
        f.stream_ok("pw-dump -m -N -R", b"[]\n");
        f.stream_err("broken", ExecError::Timeout);
        let mut text = String::new();
        f.stream("pw-dump", &["-m", "-N", "-R"]).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "[]\n");
        assert_eq!(f.stream("broken", &[]).err(), Some(ExecError::Timeout));
        assert_eq!(f.stream("unknown", &[]).err(), Some(ExecError::NotFound), "never a real tool");
        assert_eq!(f.calls(), ["pw-dump -m -N -R", "broken", "unknown"]);
    }

    #[test]
    fn capture_programmed_and_logged() {
        let f = FakeExecutor::new();
        let c = Captured { stdout: b"x\n".to_vec(), end: CaptureEnd::Exited(Some(0)) };
        f.capture_ok("tools-menu --list", c.clone());
        f.capture_err("slow-menu", ExecError::Io("boom".into()));
        let l = CaptureLimits { deadline: Duration::from_secs(1), max_bytes: 10, max_lines: 10 };
        assert_eq!(f.capture("tools-menu", &["--list"], l), Ok(c));
        assert_eq!(f.capture("slow-menu", &[], l), Err(ExecError::Io("boom".into())));
        assert_eq!(f.capture("other", &[], l), Err(ExecError::NotFound));
        assert_eq!(f.calls(), ["tools-menu --list", "slow-menu", "other"]);
    }

    #[test]
    fn spawn_with_env_records_the_line_and_the_variables() {
        let f = FakeExecutor::new();
        f.ok("hook-x a", "");
        assert_eq!(f.spawn_with_env("hook-x", &["a"], &[("ALPHEUS_EVENT", "dnd")], &["ALPHEUS_X"]), Ok(()));
        assert_eq!(f.spawn_with_env("hook-y", &[], &[("A", "1")], &[]), Err(ExecError::NotFound), "never a real tool");
        assert!(matches!(f.spawn_with_env("hook-x", &["a"], &[("A=B", "x")], &[]), Err(ExecError::Io(_))));
        assert_eq!(f.calls(), ["hook-x a", "hook-y"]);
        assert_eq!(f.unsets(), [vec!["ALPHEUS_X".to_string()], Vec::<String>::new()]);
        assert_eq!(
            f.envs(),
            [
                ("hook-x a".to_string(), vec![("ALPHEUS_EVENT".to_string(), "dnd".to_string())]),
                ("hook-y".to_string(), vec![("A".to_string(), "1".to_string())])
            ]
        );
    }

    #[test]
    fn spawn_with_input_records_the_line_and_the_input() {
        let f = FakeExecutor::new();
        f.ok("nmcli --ask device wifi connect Bureau-Test", "");
        let args = ["--ask", "device", "wifi", "connect", "Bureau-Test"];
        assert_eq!(f.spawn_with_input("nmcli", &args, &[b"fake-pass", b"\n"]), Ok(()));
        assert_eq!(f.spawn_with_input("nmcli", &["x"], &[b"y"]), Err(ExecError::NotFound), "never a real tool");
        assert_eq!(f.calls(), ["nmcli --ask device wifi connect Bureau-Test", "nmcli x"]);
        assert_eq!(
            f.inputs(),
            [("nmcli --ask device wifi connect Bureau-Test".to_string(), b"fake-pass\n".to_vec()), ("nmcli x".to_string(), b"y".to_vec())]
        );
        assert!(f.calls().iter().all(|c| !c.contains("fake-pass")), "the input is not on the line");
    }

    #[test]
    fn spawn_watched_records_the_line_and_the_test_ends_the_child() {
        let f = FakeExecutor::new();
        f.watch_ok("recorder", Some(Some(0)));
        f.watch_ok("editor", None);
        f.watch_err("broken", ExecError::Io("boom".into()));
        let out = Path::new("/tmp/x.txt");
        let mut rec = f.spawn_watched("recorder", &["-f", "a.mp4"], Some(out)).unwrap();
        let mut ed = f.spawn_watched("editor", &["b.png"], None).unwrap();
        assert!(f.spawn_watched("broken", &[], None).is_err());
        assert_eq!(f.spawn_watched("other", &[], None).err(), Some(ExecError::NotFound), "never a real tool");
        assert_eq!(f.calls(), ["recorder -f a.mp4", "editor b.png", "broken", "other"]);
        let children = f.children();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].stdout().as_deref(), Some(out));
        assert!(rec.exit_fd().is_none());
        assert_eq!(rec.try_exit(), None);
        assert!(rec.interrupt(), "it ran");
        assert_eq!(rec.try_exit(), Some(Some(0)), "its programmed end on an interrupt");
        assert!(!rec.interrupt(), "ended: nothing sent");
        assert_eq!(children[0].interrupts(), 1);
        assert!(ed.interrupt());
        assert_eq!(ed.wait_exit(Duration::from_secs(2)), None, "an editor ends when the test says");
        children[1].end(Some(3));
        assert_eq!(ed.try_exit(), Some(Some(3)));
        assert_eq!(children[1].waits(), [Duration::from_secs(2)]);
        drop(rec);
        assert!(!children[0].killed(), "it had ended");
        let running = f.spawn_watched("editor", &[], None).unwrap();
        drop(running);
        assert!(f.children()[2].killed());
    }

    #[test]
    fn run_with_stdin_file_records_the_line_and_the_file_content_and_answers_the_exit() {
        let f = FakeExecutor::new();
        let path = std::env::temp_dir().join(format!("alpheus-fake-stdin-{}", std::process::id()));
        std::fs::write(&path, b"\x89PNG fake").unwrap();
        let limit = Duration::from_secs(5);
        f.ok("wl-copy --type image/png", "");
        f.respond("wl-copy bad", Ok(Output { code: Some(1), stdout: String::new(), stderr: String::new() }));
        assert_eq!(f.run_with_stdin_file("wl-copy", &["--type", "image/png"], &path, limit), Ok(Some(0)));
        assert_eq!(f.run_with_stdin_file("wl-copy", &["bad"], &path, limit), Ok(Some(1)), "the programmed exit code");
        assert_eq!(f.run_with_stdin_file("wl-copy", &["x"], &path, limit), Err(ExecError::NotFound), "never a real tool");
        assert_eq!(f.inputs().len(), 3);
        assert_eq!(f.inputs()[0], ("wl-copy --type image/png".to_string(), b"\x89PNG fake".to_vec()));
        let _ = std::fs::remove_file(&path);
        assert!(matches!(f.run_with_stdin_file("wl-copy", &[], &path, limit), Err(ExecError::Io(_))), "a missing file is an error");
    }
}
