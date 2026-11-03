//! The displays verbs on a test's own daemon (its socket in the test's runtime directory, never the
//! session's), for the headless and memory tests: one verb with the TUI's wait, the snapshot once a
//! number of heads is read, a snapshot until a condition holds.
use super::{Daemon, Sway};
use alpheus_commun::displays::Output;
use alpheus_commun::displays::wire::{self, Snapshot};
use alpheus_commun::launcher_ipc::{self, Reply, Request, socket_path};
use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

const DELAY: Duration = Duration::from_secs(10);

/// One displays verb on this test's daemon, with the wait the TUI uses (`reply_timeout`).
pub fn displays(sway: &Sway, request: &Request) -> Reply {
    launcher_ipc::exchange_waiting(&socket_path(&sway.runtime), request, request.reply_timeout())
        .unwrap_or_else(|e| panic!("{request:?}: {e:?}"))
}

/// The daemon's snapshot, once it read a number of heads in `n`.
pub fn outputs_snapshot(sway: &Sway, d: &Daemon, n: RangeInclusive<usize>) -> Snapshot {
    let mut last = None;
    let start = Instant::now();
    while start.elapsed() < DELAY {
        match displays(sway, &Request::Outputs) {
            Reply::Outputs(body) => {
                let s = wire::decode_snapshot(&body).unwrap_or_else(|e| panic!("{e}: {body:?}"));
                if n.contains(&s.outputs.len()) {
                    return s;
                }
                last = Some(s);
            }
            other => panic!("outputs: {other:?} {:?}", d.log()),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{n:?} heads never read: {last:?} {:?}", d.log());
}

/// The output named `name` in `s`.
pub fn output<'s>(s: &'s Snapshot, name: &str) -> &'s Output {
    s.outputs.iter().find(|o| o.name == name).unwrap_or_else(|| panic!("no {name}: {s:?}"))
}

/// The daemon's snapshot until `ok` holds (10 s at most).
pub fn snapshot_until(sway: &Sway, d: &Daemon, ok: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let mut last = None;
    let start = Instant::now();
    while start.elapsed() < DELAY {
        let s = outputs_snapshot(sway, d, 0..=16);
        if ok(&s) {
            return s;
        }
        last = Some(s);
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("never: {last:?} {:?}", d.log());
}
