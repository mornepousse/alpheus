//! Sound: `pw-dump -m -N -R`, a child process read on this source's thread
//! (silent at rest: PipeWire only writes on a change). Supervised like the
//! compositor: "–" then a restart with a growing delay when it exits; killed
//! with the thread that launched it (PDEATHSIG) and at the end of every
//! session (its stream dropped).

use crate::log::write as log;
use crate::memory::release_memory;
use crate::pipewire::{self, Scope, SoundTap};
use crate::sources::spawn_source;
use crate::system::SoundState;
use alpheus_common::exec::{ExecError, Executor};
use std::io;
use std::sync::Arc;

/// The only command the daemon keeps running (tests/guardrails.rs).
/// `-R`: compact output (pretty-printing is otherwise on by default, even
/// with `-N`) — fewer bytes over the pipe; `pipewire::follow` reads it by
/// structure, not by line, so this changes nothing there.
pub const COMMAND: [&str; 4] = ["pw-dump", "-m", "-N", "-R"];

pub fn session(exec: &dyn Executor, scope: Scope, tap: &SoundTap, publish: &mut dyn FnMut(SoundState) -> bool) -> io::Result<()> {
    let output = exec.stream(COMMAND[0], &COMMAND[1..]).map_err(|e| match e {
        ExecError::NotFound => io::Error::new(io::ErrorKind::NotFound, "pw-dump not found"),
        e => io::Error::other(format!("pw-dump: {e:?}")),
    })?;
    let mut first = true;
    pipewire::follow_tapped(output, scope, tap, &mut |s| {
        let go_on = publish(s);
        if std::mem::take(&mut first) {
            // The initial dump (≈ 600 KB) is parsed and dropped by now:
            // hand its pages back (startup budget, 15 MB).
            release_memory();
        }
        go_on
    })
}

/// Starts the sound source in its thread (`sound`), for the whole session. `scope`: what is read
/// of it when the quick panel is closed (the OSD's needs, or the bar's alone), so a change nobody
/// draws publishes nothing; `tap`: shared with the loop, which opens and closes the panel's scope
/// (quick spec § 5).
pub fn start(exec: Arc<dyn Executor>, scope: Scope, tap: Arc<SoundTap>, send: impl Fn(Option<SoundState>) -> bool + Send + 'static) {
    if let Err(e) = spawn_source("sound", move |p| session(exec.as_ref(), scope, &tap, p), send) {
        log(&format!("sound thread: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::supervise;
    use crate::system::Volume;
    use alpheus_common::exec::fake::FakeExecutor;
    use std::cell::RefCell;
    use std::time::Instant;

    const INITIAL: &str = include_str!("../../tests/fixtures/pw-dump-initial.json");
    const DELTAS: &str = include_str!("../../tests/fixtures/pw-dump-deltas.json");

    #[test]
    fn session_reads_pw_dump_through_the_executor() {
        let fake = FakeExecutor::new();
        fake.stream_ok("pw-dump -m -N -R", INITIAL.as_bytes());
        let mut seen = Vec::new();
        let r = session(&fake, Scope::Osd, &SoundTap::default(), &mut |s| {
            seen.push(s);
            true
        });
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof, "pw-dump ending is an outage");
        assert_eq!(seen.iter().map(|s| s.sink).collect::<Vec<_>>(), [Some(Volume { percent: 84, muted: false })]);
        assert_eq!(fake.calls(), ["pw-dump -m -N -R"]);
    }

    /// `INITIAL` alone holds a single array, so `follow` hits EOF after one
    /// publication whatever `go_on` is; the deltas make the difference.
    /// Bites on `let _ = go_on; false` in `session`'s wrapper closure.
    #[test]
    fn session_follows_every_delta_through_to_the_end() {
        let fake = FakeExecutor::new();
        fake.stream_ok("pw-dump -m -N -R", format!("{INITIAL}{DELTAS}").as_bytes());
        let mut seen = Vec::new();
        let r = session(&fake, Scope::Osd, &SoundTap::default(), &mut |s| {
            seen.push(s.sink.map(|v| v.percent));
            true
        });
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(seen, [Some(84), Some(90), Some(90), Some(94), None, Some(90)], "every delta published, not just the initial dump");
    }

    #[test]
    fn missing_pw_dump_is_an_outage() {
        let fake = FakeExecutor::new();
        assert_eq!(session(&fake, Scope::Osd, &SoundTap::default(), &mut |_| true).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn pw_dump_exit_dash_then_restart() {
        let fake = FakeExecutor::new();
        fake.stream_ok("pw-dump -m -N -R", INITIAL.as_bytes());
        let sends = RefCell::new(Vec::new());
        let delays = RefCell::new(Vec::new());
        supervise(
            |p| session(&fake, Scope::Osd, &SoundTap::default(), p),
            |s: Option<SoundState>| {
                let mut v = sends.borrow_mut();
                v.push(s.map(|s| s.sink.map(|v| v.percent)));
                v.len() < 6
            },
            |d| delays.borrow_mut().push(d.as_secs()),
            Instant::now,
            |_| {},
        );
        assert_eq!(*sends.borrow(), [Some(Some(84)), None, Some(Some(84)), None, Some(Some(84)), None], "\"–\" between two runs");
        assert_eq!(*delays.borrow(), [1, 2], "the initial dump alone is not a success: the delay grows");
    }
}
