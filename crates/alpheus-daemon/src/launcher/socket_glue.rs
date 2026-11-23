//! The launcher socket's side of the event loop: the listener and each connection registered on
//! calloop, the connection cap, the request deadline, and the verbs answered before the launcher's
//! guard (`socket.rs` keeps the socket itself, pure of calloop). Started by `startup::run`.

use crate::launcher::host::guarded;
use crate::launcher::socket::{Conn, MAX_CONNECTIONS, Progress, REQUEST_DEADLINE, Server};
use crate::log::write as log;
use crate::wayland::App;
use alpheus_common::launcher_ipc::{Reply, Request};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::cell::{Cell, RefCell};
use std::io;
use std::rc::{Rc, Weak};
use std::time::Duration;

/// How long the listener rests after an accept() error that stays pending.
const ACCEPT_PAUSE: Duration = Duration::from_secs(1);
/// A failed request that buffered at least this much gives its pages back.
const RELEASE_AFTER: usize = 1 << 20;

/// What `run` does with the launcher socket's bind result: kept (Some) when
/// it succeeds; logged and dropped (None) when it fails — a socket is
/// optional (spec § 3): never a reason to refuse to start the bar.
pub(crate) fn launcher_socket_outcome<T, E: std::fmt::Display>(r: Result<T, E>) -> (Option<T>, Option<String>) {
    match r {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(format!("launcher socket: {e} — the bar runs without a launcher"))),
    }
}

/// A dmenu request may have buffered up to `MAX_DMENU_BYTES` (4 MiB) before
/// being decoded: once it reached `RELEASE_AFTER`, its pages are given back
/// to the allocator as soon as it is consumed (worst case, every one of
/// `MAX_CONNECTIONS` at once: 32 MiB) — a toggle/close never buffers more
/// than its tiny header, nothing to release there. A small dmenu line list
/// is not worth a `malloc_trim` either (it walks the whole heap): it would otherwise run
/// after EVERY dmenu request, whatever its size.
fn releases_memory_once_consumed(request: &Request, buffered: usize) -> bool {
    matches!(request, Request::Dmenu { .. }) && buffered >= RELEASE_AFTER
}

/// Whether a newly accepted connection is refused for being the
/// `MAX_CONNECTIONS + 1`th at once (spec § 3): `watch_connection`'s only
/// stateful decision, pulled out so it is tested without a socket.
fn too_many_connections(inflight: usize) -> bool {
    inflight >= MAX_CONNECTIONS
}

/// One more connection tracked: the increment half of `watch_connection`'s
/// inflight bookkeeping, pulled out (alongside `untrack_connection`) so the
/// count itself — not just the cap check above — is tested without a
/// socket or calloop.
fn track_connection(inflight: &Cell<usize>) {
    inflight.set(inflight.get() + 1);
}

/// The matching decrement, whatever ends the connection (answered, failed,
/// expired, or a source that never even got registered).
fn untrack_connection(inflight: &Cell<usize>) {
    inflight.set(inflight.get().saturating_sub(1));
}

/// One pass of the level-triggered listener: takes every pending
/// connection. An error (EMFILE, ENFILE, ENOBUFS…) stays pending and would
/// wake the loop again at once: Disable instead (the caller re-enables it
/// after ACCEPT_PAUSE), logged once until accept() works again.
fn drain_listener<C>(
    mut accept: impl FnMut() -> io::Result<Option<C>>,
    mut take: impl FnMut(C),
    failing: &Cell<bool>,
    log: &mut dyn FnMut(&str),
) -> PostAction {
    loop {
        match accept() {
            Ok(Some(c)) => {
                if failing.replace(false) {
                    log("launcher socket: accepting again");
                }
                take(c);
            }
            Ok(None) => return PostAction::Continue,
            Err(e) => {
                if !failing.replace(true) {
                    log(&format!("launcher socket: accept: {e} — paused, retried every {} s", ACCEPT_PAUSE.as_secs()));
                }
                return PostAction::Disable;
            }
        }
    }
}

/// `Conn::read` behind the error boundary: a hostile request hitting a bug
/// in the decoder fails that connection, never the bar.
fn guarded_progress(read: impl FnOnce() -> Progress) -> Progress {
    guarded("socket read", read).unwrap_or_else(|| Progress::Failed("internal error".to_string()))
}

/// A reply behind the error boundary (the encoder is launcher code too).
pub(crate) fn reply_guarded(conn: Conn, r: &Reply) {
    let _ = guarded("socket reply", move || conn.reply(r));
}

/// The launcher's listening socket on the loop. Level-triggered: every
/// pending connection is accepted; one refused (another uid) leaves the
/// rest for the next pass. The closure holds the server only weakly: `run`
/// owns it, so its Drop (unlinking the socket file) runs when `run` returns
/// even if the loop's sources keep each other alive through handle clones.
pub(crate) fn watch_launcher_socket(handle: &LoopHandle<'static, App>, server: &Rc<Server>, qh: QueueHandle<App>) -> io::Result<()> {
    let fd = server.fd().try_clone_to_owned()?;
    let server: Weak<Server> = Rc::downgrade(server);
    let inflight = Rc::new(Cell::new(0usize));
    let failing = Cell::new(false);
    let me: Rc<Cell<Option<RegistrationToken>>> = Rc::new(Cell::new(None));
    let token = handle
        .insert_source(Generic::new(fd, Interest::READ, Mode::Level), {
            let me = me.clone();
            move |_, _, state: &mut App| {
                let Some(server) = server.upgrade() else { return Ok(PostAction::Remove) };
                let action =
                    drain_listener(|| server.accept(), |conn| watch_connection(state, conn, &qh, &inflight), &failing, &mut |m| log(m));
                if action == PostAction::Disable {
                    resume_listener_later(state, me.get());
                }
                Ok(action)
            }
        })
        .map_err(|e| io::Error::other(e.error.to_string()))?;
    me.set(Some(token));
    Ok(())
}

/// Re-enables the paused listener after ACCEPT_PAUSE: a one-shot timer that
/// exists only after an accept() error (never at rest).
fn resume_listener_later(state: &App, listener: Option<RegistrationToken>) {
    let (Some(handle), Some(token)) = (state.loop_handle.clone(), listener) else { return };
    let h = handle.clone();
    let timer = handle.insert_source(Timer::from_duration(ACCEPT_PAUSE), move |_, _, _: &mut App| {
        if let Err(e) = h.enable(&token) {
            log(&format!("launcher socket: not resumed: {e}"));
        }
        TimeoutAction::Drop
    });
    if let Err(e) = timer {
        log(&format!("launcher socket: resume timer: {} — the launcher stays off until the bar restarts", e.error));
    }
}

/// One accepted connection: read when readable; once whole, answered or
/// handed to the launcher. Its deadline (REQUEST_DEADLINE) is a timer that
/// exists only while the connection is being read.
fn watch_connection(state: &mut App, conn: Conn, qh: &QueueHandle<App>, inflight: &Rc<Cell<usize>>) {
    if too_many_connections(inflight.get()) {
        log("launcher socket: too many connections at once, one refused");
        reply_guarded(conn, &Reply::Error("too many connections".to_string()));
        return;
    }
    let Some(handle) = state.loop_handle.clone() else { return };
    let fd = match conn.fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(e) => {
            log(&format!("launcher socket: {e}"));
            return;
        }
    };
    let slot = Rc::new(RefCell::new(Some(conn)));
    let deadline: Rc<Cell<Option<RegistrationToken>>> = Rc::new(Cell::new(None));
    track_connection(inflight);
    let (s, d, n, h, q) = (slot.clone(), deadline.clone(), inflight.clone(), handle.clone(), qh.clone());
    let read = handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, state: &mut App| {
        // Taken by the deadline already: the socket was shut down, this is its hang-up.
        let Some(mut conn) = s.borrow_mut().take() else { return Ok(PostAction::Remove) };
        // Hostile bytes are decoded here, outside launcher_request's guard:
        // behind the error boundary too.
        match guarded_progress(|| conn.read()) {
            Progress::Pending => {
                *s.borrow_mut() = Some(conn);
                return Ok(PostAction::Continue);
            }
            Progress::Ready(request) => match route(&request) {
                // The notifications verbs are answered here, before the launcher's guard: a
                // panic in there closes the launcher, and these are not the launcher's.
                Route::Notifications => reply_guarded(conn, &state.notifications_verb(&request)),
                // The OSD's verb: a sysfs read and a pill, answered here like the
                // notifications verbs, before the launcher's guard.
                Route::Osd => reply_guarded(conn, &state.osd_verb(&request)),
                // The quick panel's verb: answered here like the OSD's.
                Route::Quick => reply_guarded(conn, &state.quick_verb(&request)),
                // The timer's verb: answered here too, before the launcher's guard.
                Route::Timer => reply_guarded(conn, &state.timer_verb(&request)),
                // The quick notepad's verb: answered here, its own guard inside.
                Route::Notes => reply_guarded(conn, &state.notes_verb(&q, &request)),
                // The ⏻ menu's verb: answered here (it closes an open launcher itself, guarded).
                Route::Power => reply_guarded(conn, &state.power_verb(&q, &request)),
                // The displays verbs: answered from the output controller, a try once the
                // compositor answered (its connection kept until then, `outputs/app.rs`).
                // The clipboard history's clear: answered here, never the launcher's.
                Route::Clipboard => reply_guarded(conn, &state.clipboard_verb(&request)),
                Route::Displays => state.displays_verb(&request, conn),
                // Shield mode's verbs: answered here; `status` and `allow` keep the connection while
                // the blocked devices are read (`shield/app.rs`).
                Route::Shield => state.shield_verb(&request, conn),
                // The failed-units count's verb: answered here, nothing of the launcher's.
                Route::Failed => reply_guarded(conn, &state.failed_verb(&request)),
                Route::Launcher => {
                    // A dmenu request may have buffered up to 4 MiB: once it
                    // reached RELEASE_AFTER, give the pages back as soon as it
                    // is consumed, whatever the daemon then decides to do with
                    // it (opened, or Busy). Read before `conn` is handed to
                    // `launcher_request`, which consumes it.
                    let buffered = conn.received();
                    if releases_memory_once_consumed(&request, buffered) {
                        crate::memory::release_memory();
                    }
                    state.launcher_request(&q, request, conn)
                }
            },
            Progress::Failed(m) => {
                log(&format!("launcher socket: {m}"));
                let buffered = conn.received();
                reply_guarded(conn, &Reply::Error(m));
                if buffered >= RELEASE_AFTER {
                    // A refused multi-MB request: its buffer is freed, give the pages back.
                    crate::memory::release_memory();
                }
            }
        }
        if let Some(t) = d.take() {
            h.remove(t);
        }
        untrack_connection(&n);
        Ok(PostAction::Remove)
    });
    if let Err(e) = read {
        log(&format!("launcher socket: {}", e.error));
        untrack_connection(inflight);
        return;
    }
    let n = inflight.clone();
    let timer = handle.insert_source(Timer::from_duration(REQUEST_DEADLINE), move |_, _, _: &mut App| {
        if let Some(conn) = slot.borrow_mut().take() {
            log("launcher socket: request not complete in time, dropped");
            let buffered = conn.received();
            reply_guarded(conn, &Reply::Error("request deadline passed".to_string()));
            untrack_connection(&n);
            if buffered >= RELEASE_AFTER {
                crate::memory::release_memory();
            }
        }
        TimeoutAction::Drop
    });
    match timer {
        Ok(t) => deadline.set(Some(t)),
        Err(e) => log(&format!("launcher socket: deadline timer: {}", e.error)),
    }
}

/// Who answers a whole request: the verbs answered here, before the launcher's guard, or the
/// launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Notifications,
    Osd,
    Quick,
    Timer,
    Notes,
    Power,
    Clipboard,
    Displays,
    Shield,
    Failed,
    Launcher,
}

fn route(request: &Request) -> Route {
    match request {
        Request::Dnd(_) | Request::DndOn(_) | Request::ClearNotifications => Route::Notifications,
        Request::Osd(_) => Route::Osd,
        Request::Quick(_) => Route::Quick,
        Request::Timer(_) => Route::Timer,
        Request::Notes => Route::Notes,
        Request::Power => Route::Power,
        Request::ClipboardClear => Route::Clipboard,
        Request::FailedRefresh => Route::Failed,
        Request::Outputs | Request::OutputsTry(_) | Request::OutputsKeep | Request::OutputsRevert => Route::Displays,
        Request::Shield(_) => Route::Shield,
        Request::Toggle
        | Request::Close
        | Request::Calc
        | Request::Windows
        | Request::Clipboard
        | Request::Tools
        | Request::Dmenu { .. } => Route::Launcher,
    }
}

#[cfg(test)]
mod tests;
