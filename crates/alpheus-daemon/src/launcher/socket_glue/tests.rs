use super::*;
use alpheus_common::launcher_ipc::{CaptureVerb, DmenuFlags, DndEnd, DndVerb, OsdVerb, QuickVerb, ShieldVerb, TimerVerb};
use std::io;

/// Each verb to the one that answers it: the displays verbs to the output controller, never to
/// the launcher (which would close or open on them).
#[test]
fn each_verb_goes_to_its_handler() {
    let cases = [
        (Request::Outputs, Route::Displays),
        (Request::OutputsTry("step\tDP-1\toff\n".into()), Route::Displays),
        (Request::OutputsKeep, Route::Displays),
        (Request::OutputsRevert, Route::Displays),
        (Request::Dnd(DndVerb::Toggle), Route::Notifications),
        (Request::DndOn(DndEnd::For(60)), Route::Notifications),
        (Request::ClearNotifications, Route::Notifications),
        (Request::Osd(OsdVerb::Brightness), Route::Osd),
        (Request::Quick(QuickVerb::Top), Route::Quick),
        (Request::Timer(TimerVerb::Popup), Route::Timer),
        (Request::Timer(TimerVerb::Start(60)), Route::Timer),
        (Request::Notes, Route::Notes),
        (Request::ClipboardClear, Route::Clipboard),
        (Request::Shield(ShieldVerb::Status), Route::Shield),
        (Request::Shield(ShieldVerb::Allow), Route::Shield),
        (Request::Capture(CaptureVerb::Select), Route::Capture),
        (Request::Clipboard, Route::Launcher),
        (Request::Tools, Route::Launcher),
        (Request::Kill, Route::Launcher),
        (Request::Power, Route::Power),
        (Request::FailedRefresh, Route::Failed),
        (Request::Toggle, Route::Launcher),
        (Request::Close, Route::Launcher),
        (Request::Calc, Route::Launcher),
        (Request::Windows, Route::Launcher),
        (Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }, Route::Launcher),
    ];
    for (request, want) in cases {
        assert_eq!(route(&request), want, "{request:?}");
    }
}

#[test]
fn a_socket_bind_failure_is_logged_and_the_bar_runs_without_a_launcher() {
    assert_eq!(launcher_socket_outcome(Ok::<_, io::Error>(5)), (Some(5), None));
    // The error's text can quote the socket's path: the line carries the kind's word.
    let refused = io::Error::new(io::ErrorKind::PermissionDenied, "/run/user/1000/alpheus.sock: refused");
    assert_eq!(
        launcher_socket_outcome(Err::<u8, _>(refused)),
        (None, Some("launcher socket: permission denied — the bar runs without a launcher".to_string()))
    );
}

#[test]
fn only_a_large_dmenu_request_releases_memory_once_consumed() {
    assert!(
        releases_memory_once_consumed(&Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }, RELEASE_AFTER),
        "reached the threshold"
    );
    assert!(
        !releases_memory_once_consumed(&Request::Dmenu { prompt: None, lines: vec![], flags: DmenuFlags::default() }, RELEASE_AFTER - 1),
        "a small dmenu list: not worth a malloc_trim (it walks the whole heap)"
    );
    assert!(!releases_memory_once_consumed(&Request::Toggle, RELEASE_AFTER), "a tiny header: nothing to release");
    assert!(!releases_memory_once_consumed(&Request::Close, RELEASE_AFTER));
}

#[test]
fn max_connections_is_enforced() {
    assert!(!too_many_connections(MAX_CONNECTIONS - 1), "room for one more");
    assert!(too_many_connections(MAX_CONNECTIONS), "already at the limit: the next one is refused");
    assert!(too_many_connections(MAX_CONNECTIONS + 1));
}

/// The inflight count itself, not just the cap check above: tracked and
/// released in lockstep, whatever the reason a connection ends.
#[test]
fn inflight_is_tracked_and_released_in_lockstep() {
    let inflight = Cell::new(0usize);
    for i in 1..=MAX_CONNECTIONS {
        assert!(!too_many_connections(inflight.get()), "room for the {i}th connection");
        track_connection(&inflight);
        assert_eq!(inflight.get(), i);
    }
    assert!(too_many_connections(inflight.get()), "at the cap: the next one is refused");
    untrack_connection(&inflight);
    assert_eq!(inflight.get(), MAX_CONNECTIONS - 1, "one released");
    assert!(!too_many_connections(inflight.get()), "room again");
    for _ in 0..MAX_CONNECTIONS {
        untrack_connection(&inflight);
    }
    assert_eq!(inflight.get(), 0, "never goes negative: saturating");
}

/// accept() keeps failing (EMFILE: the error stays pending on a
/// level-triggered source): the listener pauses instead of spinning,
/// and says so once, not once per pass.
#[test]
fn a_failing_accept_pauses_the_listener_and_logs_once() {
    let failing = Cell::new(false);
    let mut lines = Vec::new();
    for _ in 0..3 {
        let mut calls = 0;
        let action = drain_listener(
            || {
                calls += 1;
                Err::<Option<u8>, _>(io::Error::from_raw_os_error(libc::EMFILE))
            },
            |_| panic!("nothing was accepted"),
            &failing,
            &mut |m| lines.push(m.to_string()),
        );
        assert_eq!(action, PostAction::Disable, "a pending error must not spin the loop");
        assert_eq!(calls, 1, "no retry within the same pass");
    }
    assert_eq!(lines.len(), 1, "logged once while it keeps failing: {lines:?}");
    assert!(lines[0].contains("paused"), "{lines:?}");
    // It recovers: everything pending is taken, the failure is forgotten.
    let mut queue = vec![Ok(None), Ok(Some(2u8)), Ok(Some(1))];
    let mut taken = Vec::new();
    let action = drain_listener(|| queue.pop().unwrap(), |c| taken.push(c), &failing, &mut |m| lines.push(m.to_string()));
    assert_eq!((action, taken), (PostAction::Continue, vec![1, 2]));
    assert!(!failing.get());
    assert_eq!(lines.len(), 2, "one line when it accepts again: {lines:?}");
}

/// A hostile request that hits a bug in the decoder: the connection
/// fails, the bar does not.
#[test]
fn a_panic_while_reading_a_request_is_a_failed_request() {
    let progress = guarded_progress(|| panic!("decoder bug"));
    assert!(matches!(progress, Progress::Failed(ref m) if m == "internal error"), "{progress:?}");
    assert!(matches!(guarded_progress(|| Progress::Pending), Progress::Pending));
}
