use super::*;
use alpheus_commun::temp_path::TempPath;

#[test]
fn the_first_reading_seeds() {
    let mut s = Shield::default();
    assert_eq!(s.observe(Some(true)), None);
    assert_eq!(s.state(), Some(true));
    let mut s = Shield::default();
    assert_eq!(s.observe(None), None, "unknown is no seed");
    assert_eq!(s.observe(Some(false)), None, "the first known reading seeds");
    assert_eq!(s.observe(Some(true)), Some(Edge::On));
}

#[test]
fn a_change_is_an_edge() {
    let mut s = Shield::default();
    s.observe(Some(false));
    assert_eq!(s.observe(Some(false)), None, "same state");
    assert_eq!(s.observe(Some(true)), Some(Edge::On));
    assert_eq!(s.observe(Some(true)), None);
    assert_eq!(s.observe(Some(false)), Some(Edge::Off));
}

#[test]
fn unknown_in_between_changes_nothing() {
    let mut s = Shield::default();
    s.observe(Some(true));
    assert_eq!(s.observe(None), None);
    assert_eq!(s.state(), None);
    assert_eq!(s.observe(Some(true)), None, "on, unknown, on");
    s.observe(None);
    assert_eq!(s.observe(Some(false)), Some(Edge::Off), "on, unknown, off");
}

#[test]
fn the_unknown_spell_is_noted_once() {
    let mut s = Shield::default();
    s.observe(Some(true));
    assert!(!s.first_unknown(), "known: nothing to note");
    s.observe(None);
    assert!(s.first_unknown());
    assert!(!s.first_unknown(), "once per spell");
    s.observe(Some(true));
    s.observe(None);
    assert!(s.first_unknown(), "a new spell");
}

#[test]
fn read_flag_tells_missing_from_unreadable() {
    let d = TempPath::dir("shield-flag");
    let flag = d.as_ref().join("on");
    assert_eq!(read_flag(&flag), Some(false));
    std::fs::write(&flag, "").unwrap();
    assert_eq!(read_flag(&flag), Some(true));
    // A parent that is a plain file: the answer is an error, not "off".
    let plain = d.as_ref().join("plain");
    std::fs::write(&plain, "").unwrap();
    assert_eq!(read_flag(&plain.join("on")), None);
}

#[test]
fn a_directory_at_the_flag_path_is_not_on() {
    let d = TempPath::dir("shield-flag-kinds");
    let d = d.as_ref();
    std::fs::create_dir(d.join("dir")).unwrap();
    assert_eq!(read_flag(&d.join("dir")), None, "a directory is neither on nor off");
    std::fs::write(d.join("file"), "").unwrap();
    std::os::unix::fs::symlink(d.join("file"), d.join("to-file")).unwrap();
    assert_eq!(read_flag(&d.join("to-file")), Some(true), "a link to a regular file counts");
    std::os::unix::fs::symlink(d.join("dir"), d.join("to-dir")).unwrap();
    assert_eq!(read_flag(&d.join("to-dir")), None);
    std::os::unix::fs::symlink(d.join("absent"), d.join("dangling")).unwrap();
    assert_eq!(read_flag(&d.join("dangling")), Some(false), "a dangling link is nothing there");
}

#[test]
fn the_view_follows_the_config() {
    let mut c = ShieldConfig::default();
    let s = Shield::default();
    let v = s.view(&c, Instant::now());
    assert_eq!((v.state, v.can_list, v.can_allow), (None, true, false), "USBGuard's list by default");
    c.blocked = Some(vec![]);
    c.allow = vec!["x".into(), "{id}".into()];
    let v = s.view(&c, Instant::now());
    assert_eq!((v.can_list, v.can_allow), (false, true));
}

use crate::shield_usb::UsbDevice;

fn t0() -> Instant {
    Instant::now()
}

fn secs(base: Instant, s: u64) -> Instant {
    base + Duration::from_secs(s)
}

fn done(job: Job, generation: u64, outcome: RunOutcome) -> ShieldMail {
    ShieldMail::Done { job, generation, outcome, listed: None }
}

/// A model that has seen the flag off, with a switch to on begun.
fn switching_on(now: Instant) -> (Shield, u64) {
    let mut s = Shield::default();
    s.observe(Some(false));
    let (target, g) = s.begin_switch(now).expect("a switch starts");
    assert!(target, "off -> on");
    (s, g)
}

#[test]
fn the_attempt_ends_on_the_flag_the_exit_or_the_deadline() {
    let now = t0();
    // The flag first, then the exit.
    let (mut s, g) = switching_on(now);
    let (edge, fin) = s.flag(Some(true), secs(now, 1));
    assert_eq!((edge, fin), (Some(Edge::On), Some(Finish::Switched(true))));
    assert_eq!(s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 2)), None, "late Done: slot only");
    // The exit 0 first, then the flag within the grace.
    let (mut s, g) = switching_on(now);
    assert_eq!(s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 2)), None);
    assert_eq!(s.flag(Some(true), secs(now, 3)).1, Some(Finish::Switched(true)));
    // A non-zero exit fails at once.
    let (mut s, g) = switching_on(now);
    assert_eq!(
        s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(3))), secs(now, 2)),
        Some(Finish::SwitchFailed(true, Reason::Exit(3)))
    );
    // A signal.
    let (mut s, g) = switching_on(now);
    assert_eq!(s.done(done(Job::Switch(true), g, RunOutcome::Exited(None)), now), Some(Finish::SwitchFailed(true, Reason::Killed)));
    // Exit 0, then 5 s with no flag.
    let (mut s, g) = switching_on(now);
    s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 10));
    assert_eq!(s.expired(secs(now, 14)), None, "inside the grace");
    assert_eq!(s.expired(secs(now, 15)), Some(Finish::SwitchFailed(true, Reason::NoChangeAfter)));
    // Nothing for 60 s.
    let (mut s, _) = switching_on(now);
    assert_eq!(s.expired(secs(now, 59)), None);
    assert_eq!(s.expired(secs(now, 60)), Some(Finish::SwitchFailed(true, Reason::NoChangeIn60)));
    // The worker's own deadline.
    let (mut s, g) = switching_on(now);
    assert_eq!(
        s.done(done(Job::Switch(true), g, RunOutcome::TimedOut), secs(now, 60)),
        Some(Finish::SwitchFailed(true, Reason::NoChangeIn60))
    );
    // Output past the cap: keep waiting for the flag; the bound says output too long.
    let (mut s, g) = switching_on(now);
    assert_eq!(s.done(done(Job::Switch(true), g, RunOutcome::Capped), secs(now, 5)), None);
    assert_eq!(s.expired(secs(now, 60)), Some(Finish::SwitchFailed(true, Reason::OutputTooLong)));
    let (mut s, g) = switching_on(now);
    s.done(done(Job::Switch(true), g, RunOutcome::Capped), secs(now, 5));
    assert_eq!(s.flag(Some(true), secs(now, 6)).1, Some(Finish::Switched(true)), "capped, then the flag");
    // The bound before the grace's end wins.
    let (mut s, g) = switching_on(now);
    s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 58));
    assert_eq!(s.expired(secs(now, 60)), Some(Finish::SwitchFailed(true, Reason::NoChangeIn60)));
    // Missing and unstartable programs, and a panicked worker.
    for (o, r) in [
        (RunOutcome::NotFound, Reason::NotFound),
        (RunOutcome::CouldNotStart, Reason::CouldNotStart),
        (RunOutcome::WorkerFailed, Reason::CouldNotStart),
    ] {
        let (mut s, g) = switching_on(now);
        assert_eq!(s.done(done(Job::Switch(true), g, o), now), Some(Finish::SwitchFailed(true, r)));
    }
}

#[test]
fn a_late_change_clears_failed() {
    let now = t0();
    let (mut s, g) = switching_on(now);
    s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(1))), now);
    assert!(s.view(&ShieldConfig::default(), now).failed_until.is_some());
    // The helper finished after all: the flag appears, long after `failed`.
    let (edge, fin) = s.flag(Some(true), secs(now, 3));
    assert_eq!((edge, fin), (Some(Edge::On), None), "an edge as any other, no attempt to end");
    assert_eq!(s.view(&ShieldConfig::default(), secs(now, 3)).failed_until, None);
}

#[test]
fn one_action_slot_until_done() {
    let now = t0();
    let (mut s, g) = switching_on(now);
    assert!(s.action_busy());
    assert_eq!(s.begin_switch(now), None);
    assert_eq!(s.begin_allow(7, "Test Stick".into(), now), None);
    // The bound passes, the attempt fails, the slot is still held (a setuid helper the deadline cannot kill).
    assert!(s.expired(secs(now, 60)).is_some());
    assert!(s.action_busy(), "held until its worker reports");
    assert_eq!(s.begin_switch(secs(now, 61)), None);
    assert_eq!(s.begin_allow(7, "x".into(), secs(now, 61)), None);
    assert_eq!(s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 70)), None);
    assert!(!s.action_busy());
    assert!(s.begin_allow(7, "x".into(), secs(now, 71)).is_some());
}

#[test]
fn a_late_done_frees_the_slot_only() {
    let now = t0();
    let (mut s, g) = switching_on(now);
    s.expired(secs(now, 60));
    let (_, g2) = {
        // The first worker reports, then a second attempt runs.
        s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 61));
        s.begin_switch(secs(now, 62)).unwrap()
    };
    assert_ne!(g, g2);
    // A stray report for the first generation cannot end the second attempt.
    assert_eq!(s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(5))), secs(now, 63)), None);
    assert!(s.switching(), "the second attempt is untouched");
}

#[test]
fn one_read_at_a_time() {
    let mut s = Shield::default();
    assert!(s.begin_list());
    assert!(!s.begin_list(), "the second asker joins the read");
    let p = Parsed { devices: vec![UsbDevice { id: 1, vid_pid: "1234:5678".into(), name: "Test Stick".into() }], more: false };
    let m = ShieldMail::Done { job: Job::List, generation: 0, outcome: RunOutcome::Exited(Some(0)), listed: Some(p.clone()) };
    assert_eq!(s.done(m, t0()), Some(Finish::Listed(p)));
    assert!(s.begin_list(), "free again");
    let failed = |o| s_done_list(o);
    assert_eq!(failed(RunOutcome::TimedOut), Finish::ListFailed(Reason::TimedOut));
    assert_eq!(failed(RunOutcome::Capped), Finish::ListFailed(Reason::OutputTooLong));
    assert_eq!(failed(RunOutcome::Exited(Some(1))), Finish::ListFailed(Reason::Exit(1)));
    assert_eq!(failed(RunOutcome::NotFound), Finish::ListFailed(Reason::NotFound));
}

fn s_done_list(o: RunOutcome) -> Finish {
    let mut s = Shield::default();
    s.begin_list();
    s.done(done(Job::List, 0, o), t0()).unwrap()
}

#[test]
fn a_read_does_not_hold_the_action_slot() {
    let mut s = Shield::default();
    s.observe(Some(true));
    assert!(s.begin_list());
    assert!(!s.action_busy());
    assert!(s.begin_switch(t0()).is_some());
    s.done(done(Job::List, 0, RunOutcome::TimedOut), t0());
    assert!(s.action_busy(), "a read's report never frees the action slot");
}

#[test]
fn unknown_state_starts_nothing() {
    let mut s = Shield::default();
    assert_eq!(s.begin_switch(t0()), None);
    s.observe(Some(true));
    s.observe(None);
    assert_eq!(s.begin_switch(t0()), None);
    assert!(!s.action_busy(), "a refused start holds nothing");
}

#[test]
fn failed_shows_4_s() {
    let now = t0();
    let (mut s, g) = switching_on(now);
    s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(1))), now);
    let c = ShieldConfig::default();
    assert_eq!(s.view(&c, now).failed_until, Some(secs(now, 4)));
    assert!(!s.due(secs(now, 3)));
    assert!(s.due(secs(now, 4)));
    assert_eq!(s.view(&c, secs(now, 4)).failed_until, None);
    assert!(!s.due(secs(now, 5)), "once");
}

#[test]
fn the_deadline_follows_the_grace() {
    let now = t0();
    let mut s = Shield::default();
    assert_eq!(s.deadline(), None);
    let (mut s2, g) = switching_on(now);
    assert_eq!(s2.deadline(), Some(secs(now, 60)));
    s2.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 3));
    assert_eq!(s2.deadline(), Some(secs(now, 8)), "the grace");
    s2.flag(Some(true), secs(now, 4));
    assert_eq!(s2.deadline(), None, "none after a success");
    // An allow never arms the shield's timer.
    s.observe(Some(true));
    s.begin_allow(7, "Test Stick".into(), now);
    assert_eq!(s.deadline(), None);
    // A failure leaves none either.
    let (mut s3, g) = switching_on(now);
    s3.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(2))), now);
    assert_eq!(s3.deadline(), None);
}

#[test]
fn an_allow_ends_on_its_exit() {
    let now = t0();
    let mut s = Shield::default();
    s.observe(Some(true));
    let g = s.begin_allow(7, "Test Stick".into(), now).unwrap();
    assert_eq!(s.done(done(Job::Allow(7), g, RunOutcome::Exited(Some(0))), now), Some(Finish::Allowed(7, "Test Stick".into())));
    let g = s.begin_allow(8, "Other".into(), now).unwrap();
    assert_eq!(s.done(done(Job::Allow(8), g, RunOutcome::Exited(Some(1))), now), Some(Finish::AllowFailed(8, Reason::Exit(1))));
    for (o, r) in
        [(RunOutcome::Capped, Reason::OutputTooLong), (RunOutcome::TimedOut, Reason::TimedOut), (RunOutcome::NotFound, Reason::NotFound)]
    {
        let g = s.begin_allow(9, "Other".into(), now).unwrap();
        assert_eq!(s.done(done(Job::Allow(9), g, o), now), Some(Finish::AllowFailed(9, r)));
    }
}

#[test]
fn status_text_reads() {
    let p = Parsed {
        devices: vec![
            UsbDevice { id: 1, vid_pid: "1234:5678".into(), name: "Test Stick".into() },
            UsbDevice { id: 2, vid_pid: "abcd:0001".into(), name: "Test Keyboard".into() },
        ],
        more: false,
    };
    assert_eq!(
        status_text(Some(true), Some(Ok(&p))),
        "Shield: on\nBlocked USB devices:\n  Test Stick (1234:5678)\n  Test Keyboard (abcd:0001)"
    );
    assert_eq!(status_text(Some(true), Some(Ok(&Parsed::default()))), "Shield: on\nBlocked USB devices: none");
    assert_eq!(status_text(Some(true), Some(Err(Reason::TimedOut))), "Shield: on\nBlocked USB devices: cannot list (timed out)");
    assert_eq!(status_text(Some(true), None), "Shield: on", "no device list configured");
    assert_eq!(status_text(Some(false), Some(Ok(&p))), "Shield: off");
    assert_eq!(status_text(None, None), "Shield: unknown");
}

#[test]
fn reasons_are_the_closed_list() {
    let all = [
        (Reason::Exit(126), "exit 126"),
        (Reason::Killed, "killed"),
        (Reason::OutputTooLong, "output too long"),
        (Reason::NoChangeAfter, "no change after the command"),
        (Reason::NoChangeIn60, "no change in 60 s"),
        (Reason::TimedOut, "timed out"),
        (Reason::NotFound, "program not found"),
        (Reason::CouldNotStart, "could not start"),
    ];
    for (r, t) in all {
        assert_eq!(r.text(), t);
    }
}

#[test]
fn the_notices_read() {
    let c = ShieldConfig::default();
    assert_eq!(notice(&NoticeKind::On, &c), ("Shield on".into(), "Protections for public networks are on.".into()));
    assert_eq!(notice(&NoticeKind::Off, &c), ("Shield off".into(), "Back to the usual settings.".into()));
    let c = ShieldConfig { on_text: Some("Up".into()), off_text: Some("Down".into()), ..Default::default() };
    assert_eq!(notice(&NoticeKind::On, &c).1, "Up");
    assert_eq!(notice(&NoticeKind::Off, &c).1, "Down");
    assert_eq!(notice(&NoticeKind::SwitchFailed, &c), ("Shield unchanged".into(), "The command failed or was cancelled.".into()));
    assert_eq!(notice(&NoticeKind::Allowed("Test Stick".into()), &c), ("USB device allowed".into(), "Test Stick".into()));
    assert_eq!(notice(&NoticeKind::AllowFailed, &c), ("USB device not allowed".into(), "The command failed or was cancelled.".into()));
    assert_eq!(notice(&NoticeKind::NothingBlocked, &c).0, "No blocked USB device");
}
