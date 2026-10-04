//! Compositor event sources: READ-ONLY connection, publishing of the
//! normalized state, backoff with growing delay when the source goes down
//! (1 s, 2 s, 4 s… 30 s at most; back to 1 s after a success). While down,
//! the bar shows "–".

pub mod hypr;
pub mod niri;

use crate::compositor::CompositorState;
use crate::log::write as log;
use std::ffi::OsString;
use std::io;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Readable message of a caught panic (`catch_unwind`): `panic!("…")` and
/// `panic!("{}", x)` (the common case) give `&str` or `String`; anything
/// else (a value that doesn't implement `Display`) stays anonymous rather
/// than wrongly trying to show it.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with no message".to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compositor {
    Hyprland { directory: PathBuf },
    Niri { socket: PathBuf },
}

/// Same rule as nixos-config's `compositor()` (home/scripts/lib.sh):
/// NIRI_SOCKET first, then HYPRLAND_INSTANCE_SIGNATURE.
pub fn detect(var: impl Fn(&str) -> Option<OsString>) -> Option<Compositor> {
    let non_empty = |k: &str| var(k).filter(|v| !v.is_empty());
    if let Some(s) = non_empty("NIRI_SOCKET") {
        return Some(Compositor::Niri { socket: s.into() });
    }
    // The signature is a directory name: "/" or ".." would escape it.
    let sig = non_empty("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| s != ".." && !s.as_encoded_bytes().contains(&b'/'))?;
    let runtime = non_empty("XDG_RUNTIME_DIR")?;
    Some(Compositor::Hyprland { directory: PathBuf::from(runtime).join("hypr").join(sig) })
}

pub const MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff::new()
    }
}

impl Backoff {
    pub fn new() -> Self {
        Backoff { next: Duration::from_secs(1) }
    }

    /// Delay before the next attempt; the following one doubles (30 s at most).
    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (d * 2).min(MAX_DELAY);
        d
    }

    /// The source worked: the next outage starts over from 1 s.
    pub fn on_success(&mut self) {
        self.next = Duration::from_secs(1);
    }
}

/// Duration beyond which a session that published counts as a success, even
/// with no event after the initial state.
pub const STABLE_DURATION: Duration = Duration::from_secs(10);

/// Runs `session` (blocking, returns Err when the source goes down) as long as `send` accepts it, whatever it publishes
/// (compositor state, volume, system state…): publishes each state, publishes `None` ("–") on every outage, then waits
/// the backoff delay. `sleep`, `now` and `log` are injected: tests count delays and log lines in simulated time.
pub fn supervise<T>(
    mut session: impl FnMut(&mut dyn FnMut(T) -> bool) -> io::Result<()>,
    send: impl Fn(Option<T>) -> bool,
    mut sleep: impl FnMut(Duration),
    mut now: impl FnMut() -> Instant,
    mut log: impl FnMut(&str),
) {
    let mut backoff = Backoff::new();
    let mut first = true;
    loop {
        let mut publications = 0u32;
        let mut closed = false;
        let start = now();
        // A bug in `session` (or in a library it calls) must neither bring
        // the whole daemon down, nor freeze the bar on its last state: a
        // panic is caught here and handled exactly like an `Err` would have
        // been ("–", growing delay, one log line), then the source is
        // retried on the next round. `publish` captures `publications`,
        // `closed` and `send` by reference: their state at the moment of a
        // panic is whatever it was before, consistent (plain integers and
        // booleans, never a half-set invariant).
        let result = catch_unwind(AssertUnwindSafe(|| {
            session(&mut |e| {
                publications += 1;
                let ok = send(Some(e));
                closed |= !ok;
                ok
            })
        }));
        // `&*payload` (not `&payload`): without the explicit dereference,
        // `&Box<dyn Any + Send>` wrongly lets itself be converted into a
        // trait object over THE BOX itself (which also implements `Any`)
        // rather than over the value it contains — `downcast_ref` would then
        // always fail, silently (a verified trap: see the fix report).
        let r: io::Result<()> = result.unwrap_or_else(|payload| Err(io::Error::other(format!("panic: {}", panic_text(&*payload)))));
        if closed {
            return; // the bar stops
        }
        // Success: at least one event from the stream after the initial
        // state (a second publication), or a session held for
        // STABLE_DURATION. A source that serves the initial state and then
        // immediately goes down thus backs off instead of looping at 1 s
        // (wakeups, a flickering bar, a flooded log).
        let elapsed = now().saturating_duration_since(start);
        let succeeded = publications >= 2 || (publications >= 1 && elapsed >= STABLE_DURATION);
        if succeeded {
            backoff.on_success();
        }
        if let Err(e) = r {
            // Logged on the first outage and after each recovery, not on
            // every attempt (up to 2,880 lines a day otherwise). The
            // "alpheus-demon:" prefix is NOT repeated here: it's the `log`
            // closure given by the caller that adds it, only once (see
            // `run`, further below); the tests pass a closure that doesn't
            // need it.
            if first || succeeded {
                log(&format!("source down: {e}"));
            }
        }
        first = false;
        if !send(None) {
            return;
        }
        sleep(backoff.next_delay());
    }
}

/// If THIS thread ends in a panic — whatever its cause, even outside the
/// session watched by `supervise` (already protected by `catch_unwind`) —
/// publishes `None` ("–" shown) and logs it, rather than leaving the channel
/// open with nothing ever coming through again: the other end (`tx`), also
/// held by the main thread and the guard thread, stays alive as long as the
/// daemon runs; without this guard, the bar would stay frozen on its last
/// state instead of showing the outage.
struct SourceGuard<'a, T, F: Fn(Option<T>) -> bool> {
    name: &'static str,
    send: &'a F,
    value: PhantomData<fn(T)>,
}

impl<T, F: Fn(Option<T>) -> bool> Drop for SourceGuard<'_, T, F> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            log(&format!("{} thread: panic, bar set to \"–\"", self.name));
            (self.send)(None);
        }
    }
}

/// Prefixes a source's log line with its name, e.g. `prefixed("compositor", "source down: refused")`
/// reads "compositor source down: refused": a single source can appear several times in the log
/// (compositor, sound, network…), so every line it writes must say which one it came from.
fn prefixed(name: &str, m: &str) -> String {
    format!("{name} {m}")
}

/// Starts a supervised source in its own thread, named `name`: its log lines
/// read "<name> source down: …", a panic anywhere in it publishes "–".
pub fn spawn_source<T: Send + 'static>(
    name: &'static str,
    session: impl FnMut(&mut dyn FnMut(T) -> bool) -> io::Result<()> + Send + 'static,
    send: impl Fn(Option<T>) -> bool + Send + 'static,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _guard = SourceGuard { name, send: &send, value: PhantomData };
            supervise(session, &send, std::thread::sleep, Instant::now, |m| log(&prefixed(name, m)));
        })
        .map(|_| ())
}

/// Starts the compositor source. No compositor recognized: "–" once, with
/// no backoff (nothing will change in this session).
pub fn start(c: Option<Compositor>, send: impl Fn(Option<CompositorState>) -> bool + Send + 'static) {
    let r = match c {
        None => {
            log("neither niri nor Hyprland detected (NIRI_SOCKET, HYPRLAND_INSTANCE_SIGNATURE)");
            send(None);
            return;
        }
        Some(Compositor::Hyprland { directory }) => spawn_source("compositor", move |p| hypr::session(&directory, p), send),
        Some(Compositor::Niri { socket }) => spawn_source("compositor", move |p| niri::session(&socket, p), send),
    };
    if let Err(e) = r {
        log(&format!("compositor thread: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| pairs.iter().find(|(c, _)| *c == k).map(|(_, v)| OsString::from(v))
    }

    #[test]
    fn niri_detected_first() {
        assert_eq!(
            detect(env(&[
                ("NIRI_SOCKET", "/run/niri.sock"),
                ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
                ("XDG_RUNTIME_DIR", "/run/user/1000")
            ])),
            Some(Compositor::Niri { socket: "/run/niri.sock".into() })
        );
        assert_eq!(
            detect(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("XDG_RUNTIME_DIR", "/run/user/1000")])),
            Some(Compositor::Hyprland { directory: "/run/user/1000/hypr/abc".into() })
        );
        assert_eq!(detect(env(&[("NIRI_SOCKET", ""), ("HYPRLAND_INSTANCE_SIGNATURE", "")])), None);
        assert_eq!(detect(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc")])), None, "no XDG_RUNTIME_DIR");
    }

    #[test]
    fn supervise_is_generic_over_the_published_value() {
        let sends = RefCell::new(Vec::new());
        let mut round = 0;
        supervise(
            |publish: &mut dyn FnMut(u8) -> bool| {
                round += 1;
                publish(round);
                publish(round + 10);
                Err(io::Error::other("fell"))
            },
            |v: Option<u8>| {
                let mut s = sends.borrow_mut();
                s.push(v);
                s.len() < 6
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        assert_eq!(*sends.borrow(), [Some(1), Some(11), None, Some(2), Some(12), None]);
    }

    #[test]
    fn spawn_source_names_its_thread_and_prefixes_its_log() {
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_source(
            "sound",
            |publish: &mut dyn FnMut(String) -> bool| {
                publish(std::thread::current().name().unwrap_or("").to_string());
                Ok(())
            },
            move |v: Option<String>| {
                let _ = tx.send(v);
                false // stop after the first publication
            },
        )
        .unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Some("sound".to_string()));
    }

    #[test]
    fn prefixed_prepends_the_source_name() {
        assert_eq!(prefixed("compositor", "source down: refused"), "compositor source down: refused");
    }

    #[test]
    fn delay_doubles_up_to_30s() {
        let mut r = Backoff::new();
        let d: Vec<u64> = (0..8).map(|_| r.next_delay().as_secs()).collect();
        assert_eq!(d, [1, 2, 4, 8, 16, 30, 30, 30]);
        r.on_success();
        assert_eq!(r.next_delay(), Duration::from_secs(1));
    }

    /// Simulated clock: advances by `step` on each read.
    fn clock(step: Duration) -> impl FnMut() -> Instant {
        let mut t = Instant::now();
        move || {
            t += step;
            t
        }
    }

    #[test]
    fn outage_dash_then_backoff_with_growing_delay() {
        let delays = RefCell::new(Vec::new());
        let sends = RefCell::new(Vec::new());
        let mut round = 0;
        supervise(
            |publish| {
                round += 1;
                match round {
                    1..=6 | 8 => Err(io::Error::other("refused")),
                    7 => {
                        publish(CompositorState::default()); // initial state
                        publish(CompositorState::default()); // a stream event
                        Err(io::Error::other("fell"))
                    }
                    _ => {
                        publish(CompositorState::default());
                        Ok(())
                    }
                }
            },
            |e| {
                let mut v = sends.borrow_mut();
                v.push(e.is_some());
                v.len() < 11 // the 11th send: the bar stops
            },
            |d| {
                let mut v = delays.borrow_mut();
                v.push(d.as_secs());
                assert!(v.len() < 100, "supervise never stopped");
            },
            clock(Duration::ZERO),
            |_| {},
        );
        assert_eq!(*delays.borrow(), [1, 2, 4, 8, 16, 30, 1, 2], "back to 1 s after round 7 succeeds");
        let (f, t) = (false, true);
        assert_eq!(*sends.borrow(), [f, f, f, f, f, f, t, t, f, f, t], "\"–\" (None) on every outage");
    }

    /// A bug in `session` (or in a library it calls) that panics must
    /// neither bring the whole daemon down, nor freeze the bar: `supervise`
    /// catches it (`catch_unwind`), treats it like an ordinary outage ("–",
    /// growing delay, one log line), and keeps retrying the source
    /// afterwards — exactly like for an `Err`.
    #[test]
    fn panic_in_session_handled_like_an_outage_then_retried() {
        let delays = RefCell::new(Vec::new());
        let sends = RefCell::new(Vec::new());
        let logs = RefCell::new(Vec::new());
        let mut round = 0;
        supervise(
            |publish| {
                round += 1;
                match round {
                    1 => panic!("simulation: bug in the session"),
                    2 => {
                        publish(CompositorState::default()); // initial state
                        publish(CompositorState::default()); // a stream event: success
                        Ok(())
                    }
                    _ => Err(io::Error::other("refused")),
                }
            },
            |e| {
                let mut v = sends.borrow_mut();
                v.push(e.is_some());
                v.len() < 5
            },
            |d| {
                let mut v = delays.borrow_mut();
                v.push(d.as_secs());
                assert!(v.len() < 100, "supervise never stopped");
            },
            clock(Duration::ZERO),
            |m| logs.borrow_mut().push(m.to_string()),
        );
        // Round 1: panic → delay 1 s. Round 2: success (2 publications) →
        // backoff reset to 1 s. Round 3: ordinary outage; the 5th send
        // ("–" after this round) stops the bar before the next `sleep`.
        assert_eq!(*delays.borrow(), [1, 1]);
        let (f, t) = (false, true);
        assert_eq!(*sends.borrow(), [f, t, t, f, f], "\"–\" after the panic, like after an ordinary outage");
        // A single line: on the panic (first), not on round 3 (neither
        // first, nor a recovery behind it) — same rule as an Err.
        assert_eq!(logs.borrow().len(), 1, "{:?}", logs.borrow());
        assert!(logs.borrow()[0].contains("bug in the session"), "{:?}", logs.borrow());
    }

    /// Defense in depth: even if a panic happens OUTSIDE the session watched
    /// by `supervise` (already protected by `catch_unwind` — here, we
    /// directly simulate a panic in the thread that carries it), the bar
    /// must never stay frozen on its last state for lack of a final `None`
    /// published.
    #[test]
    fn guard_publishes_dash_if_thread_panics() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let r = received.clone();
        let send = move |e: Option<CompositorState>| {
            r.lock().unwrap().push(e.is_some());
            true
        };
        let h = std::thread::spawn(move || {
            let _guard = SourceGuard { name: "compositor", send: &send, value: std::marker::PhantomData };
            panic!("simulation: panic outside the watched session");
        });
        assert!(h.join().is_err(), "the thread must really panic, otherwise this test proves nothing");
        assert_eq!(*received.lock().unwrap(), [false], "\"–\" published by the guard despite the panic");
    }

    /// A source that serves the initial state and then immediately goes
    /// down: the delay still grows, a single log line.
    fn flapping(
        mut session: impl FnMut(&mut dyn FnMut(CompositorState) -> bool) -> io::Result<()>,
        step: Duration,
    ) -> (Vec<u64>, Vec<String>, usize) {
        let delays = RefCell::new(Vec::new());
        let log = RefCell::new(Vec::new());
        let published = RefCell::new(0);
        let cycles = RefCell::new(0);
        supervise(
            &mut session,
            |e| {
                if e.is_some() {
                    *published.borrow_mut() += 1;
                    return true;
                }
                *cycles.borrow_mut() += 1;
                *cycles.borrow() < 6
            },
            |d| {
                let mut v = delays.borrow_mut();
                v.push(d.as_secs());
                assert!(v.len() < 100, "supervise never stopped");
            },
            clock(step),
            |m| log.borrow_mut().push(m.to_string()),
        );
        (delays.into_inner(), log.into_inner(), published.into_inner())
    }

    #[test]
    fn initial_state_alone_is_not_a_success() {
        let (delays, log, _) = flapping(
            |p| {
                p(CompositorState::default());
                Err(io::Error::other("fell"))
            },
            Duration::from_secs(1),
        );
        assert_eq!(delays, [1, 2, 4, 8, 16]);
        assert_eq!(log.len(), 1, "{log:?}");
    }

    #[test]
    fn a_10s_session_counts_as_success() {
        let (delays, log, _) = flapping(
            |p| {
                p(CompositorState::default());
                Err(io::Error::other("fell"))
            },
            STABLE_DURATION,
        );
        assert_eq!(delays, [1, 1, 1, 1, 1]);
        assert_eq!(log.len(), 6, "one line per outage after each recovery");
    }

    #[test]
    fn hyprland_flapper_backs_off() {
        let f = hypr::fake::FakeHyprland::start_with("flapper", hypr::fake::CLIENTS.as_bytes(), true);
        let d = f.directory.clone();
        let (delays, log, published) = flapping(|p| hypr::session(&d, p), Duration::ZERO);
        assert_eq!(published, 6, "the initial state served every round");
        assert_eq!(delays, [1, 2, 4, 8, 16]);
        assert_eq!(log.len(), 1, "{log:?}");
    }

    #[test]
    fn niri_flapper_backs_off() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let directory = std::env::temp_dir().join(format!("alpheus-niri-flapper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("niri.sock");
        let l = UnixListener::bind(&path).unwrap();
        // Each connection: the full initial state, then close.
        let initial: String =
            include_str!("../../tests/fixtures/niri-evenements.jsonl").lines().take(6).map(|l| format!("{l}\n")).collect();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { return };
                let mut request = String::new();
                let _ = BufReader::new(&c).read_line(&mut request);
                let _ = (&c).write_all(initial.as_bytes());
            }
        });
        let (delays, log, published) = flapping(|p| niri::session(&path, p), Duration::ZERO);
        assert_eq!(published, 6, "one state published per round, once the full initial state has arrived");
        assert_eq!(delays, [1, 2, 4, 8, 16]);
        assert_eq!(log.len(), 1, "{log:?}");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn hyprland_signature_escaping_directory_refused() {
        for sig in ["../../etc", "a/b", ".."] {
            let v: &'static str = Box::leak(sig.to_string().into_boxed_str());
            let pairs: &'static [(&str, &str)] =
                Box::leak(Box::new([("HYPRLAND_INSTANCE_SIGNATURE", v), ("XDG_RUNTIME_DIR", "/run/user/1000")]));
            assert_eq!(detect(env(pairs)), None, "{sig}");
        }
    }
}
