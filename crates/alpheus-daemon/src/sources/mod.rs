//! Compositor event sources: READ-ONLY connection, publishing of the
//! normalized state, backoff with growing delay when the source goes down
//! (1 s, 2 s, 4 s… 30 s at most; back to 1 s after a success). While down,
//! the bar shows "–". The compositor is detected once at start (`detect`), in the usual session-script
//! order, with niri last.
//!
//! Never:
//! - write to a compositor: each backend's requests are a fixed list, checked by
//!   `compositor_requests_are_read_only`, `mango_writes_only_its_request_lines` and
//!   `sway_writes_only_its_three_messages`; the one write alpheus makes is the configured
//!   workspace command, from `actions.rs`;
//! - open a socket anywhere else (`compositor_sockets_opened_only_in_sources`);
//! - poll: every backend blocks on its socket, so a quiet session costs no wake-up.
//!
//! See: docs/compositors.md (what each compositor gives and what `wm-set` stands for),
//! docs/features/bar.md ("What the bar reads, and what it writes"), docs/ARCHITECTURE.md
//! ("Sources: what feeds the loop").

pub mod dbus;
pub mod hypr;
pub mod mango;
pub mod niri;
pub mod resources;
pub mod sound;
pub mod sway;

use crate::compositor::CompositorState;
use crate::log::write as log;
use std::ffi::OsString;
use std::io;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use alpheus_common::compositor::{Compositor, VARIABLES, detect, mango_socket};

/// The daemon's start line naming a compositor (`Compositor` itself lives in alpheus-common).
pub trait CompositorName {
    /// Its name and the variable that named it, for the start line (`describe`).
    fn name(&self) -> (&'static str, &'static str);
}

impl CompositorName for Compositor {
    fn name(&self) -> (&'static str, &'static str) {
        match self {
            Compositor::Sway { .. } => ("Sway", "SWAYSOCK"),
            Compositor::Hyprland { .. } => ("Hyprland", "HYPRLAND_INSTANCE_SIGNATURE"),
            Compositor::Mango { .. } => ("Mango", "MANGO_INSTANCE_SIGNATURE"),
            Compositor::Niri { .. } => ("niri", "NIRI_SOCKET"),
        }
    }
}

/// The socket a Mango session opens: `$MANGO_INSTANCE_SIGNATURE` read again (the path
/// embeds Mango's pid), or the detected path if the variable is gone or unusable.
fn mango_path(var: impl Fn(&str) -> Option<OsString>, detected: &Path) -> PathBuf {
    mango_socket(var).unwrap_or_else(|| detected.to_path_buf())
}

/// The start line: the backend and its variable, or the four variables when none is set. Never
/// the path (it holds a pid, and nothing in the log needs it).
pub fn describe(c: Option<&Compositor>) -> String {
    match c {
        Some(c) => {
            let (name, variable) = c.name();
            format!("compositor: {name} (${variable})")
        }
        None => format!("no compositor detected ({} unset or empty): the bar shows \"–\"", VARIABLES.join(", ")),
    }
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
        // The payload is dropped unread: a panic's message can quote private text. Its thread and
        // place were logged by the daemon's panic hook when it happened (`log::install_panic_hook`).
        let mut panicked = false;
        let r: io::Result<()> = result.unwrap_or_else(|_| {
            panicked = true;
            Err(io::Error::other("panic"))
        });
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
                log(&format!("source down: {}", if panicked { "panic" } else { crate::log::error_word(&e) }));
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
        // A source thread only reads and publishes a small, bounded state: the default 8 MB stack
        // is overkill, and the smaller one serves the startup budget (only touched pages count
        // towards PSS, so the gain is modest, but real).
        .stack_size(256 * 1024)
        .spawn(move || {
            let _guard = SourceGuard { name, send: &send, value: PhantomData };
            supervise(session, &send, std::thread::sleep, Instant::now, |m| log(&prefixed(name, m)));
        })
        .map(|_| ())
}

/// Starts the compositor source, after one line naming it. No compositor recognized: "–" once,
/// with no backoff (nothing will change in this session).
pub fn start(c: Option<Compositor>, send: impl Fn(Option<CompositorState>) -> bool + Send + 'static) {
    log(&describe(c.as_ref()));
    let r = match c {
        None => {
            send(None);
            return;
        }
        Some(Compositor::Hyprland { directory }) => spawn_source("compositor", move |p| hypr::session(&directory, p), send),
        Some(Compositor::Niri { socket }) => spawn_source("compositor", move |p| niri::session(&socket, p), send),
        Some(Compositor::Mango { socket }) => {
            spawn_source("compositor", move |p| mango::session(&mango_path(|k| std::env::var_os(k), &socket), p), send)
        }
        Some(Compositor::Sway { socket }) => spawn_source("compositor", move |p| sway::session(&socket, p), send),
    };
    if let Err(e) = r {
        log(&format!("compositor thread: {word}", word = crate::log::error_word(&e)));
    }
}

#[cfg(test)]
mod tests;
