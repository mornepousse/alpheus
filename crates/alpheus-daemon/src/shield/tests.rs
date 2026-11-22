use super::*;
use alpheus_common::temp_path::TempPath;

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

use crate::shield::usb::UsbDevice;

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
    // The 5 s grace after an exit 0 is not cut by the 60 s bound (final fix wave M2), and it says so.
    let (mut s, g) = switching_on(now);
    s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 58));
    assert_eq!(s.deadline(), Some(secs(now, 63)));
    assert_eq!(s.expired(secs(now, 60)), None, "the command ended at 58 s: its flag has until 63 s");
    assert_eq!(s.flag(Some(true), secs(now, 62)).1, Some(Finish::Switched(true)), "a flag at 62 s still counts");
    let (mut s, g) = switching_on(now);
    s.done(done(Job::Switch(true), g, RunOutcome::Exited(Some(0))), secs(now, 58));
    assert_eq!(s.expired(secs(now, 63)), Some(Finish::SwitchFailed(true, Reason::NoChangeAfter)));
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
        "Shield: on\nBlocked USB devices:\n  1234:5678  ·  Test Stick\n  abcd:0001  ·  Test Keyboard"
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

// ---- the verbs' decisions (shield spec § 9) ----

fn ctx(state: Option<bool>) -> VerbCtx {
    VerbCtx { state, action_busy: false, can_list: true, can_allow: true }
}

fn error(t: &str) -> VerbPlan {
    VerbPlan::Answer(Reply::Error(t.into()))
}

#[test]
fn on_and_off_in_the_asked_state_run_nothing() {
    assert_eq!(plan_verb(ShieldVerb::On, &ctx(Some(true))), VerbPlan::Answer(Reply::Done));
    assert_eq!(plan_verb(ShieldVerb::Off, &ctx(Some(false))), VerbPlan::Answer(Reply::Done));
    assert_eq!(plan_verb(ShieldVerb::On, &ctx(Some(false))), VerbPlan::Switch(true));
    assert_eq!(plan_verb(ShieldVerb::Off, &ctx(Some(true))), VerbPlan::Switch(false));
    assert_eq!(plan_verb(ShieldVerb::Toggle, &ctx(Some(true))), VerbPlan::Switch(false));
    assert_eq!(plan_verb(ShieldVerb::Toggle, &ctx(Some(false))), VerbPlan::Switch(true));
    // Even while a command runs: nothing to start, nothing wrong.
    let busy = VerbCtx { action_busy: true, ..ctx(Some(true)) };
    assert_eq!(plan_verb(ShieldVerb::On, &busy), VerbPlan::Answer(Reply::Done));
}

#[test]
fn a_verb_while_busy_is_an_error() {
    let busy = VerbCtx { action_busy: true, ..ctx(Some(false)) };
    for v in [ShieldVerb::On, ShieldVerb::Toggle] {
        assert_eq!(plan_verb(v, &busy), error(ERR_BUSY), "{v:?}");
    }
    let busy_on = VerbCtx { action_busy: true, ..ctx(Some(true)) };
    assert_eq!(plan_verb(ShieldVerb::Off, &busy_on), error(ERR_BUSY));
}

#[test]
fn an_unknown_state_is_an_error_but_for_status() {
    for v in [ShieldVerb::On, ShieldVerb::Off, ShieldVerb::Toggle, ShieldVerb::Allow] {
        assert_eq!(plan_verb(v, &ctx(None)), error(ERR_UNKNOWN), "{v:?}");
    }
    assert_eq!(plan_verb(ShieldVerb::Status, &ctx(None)), VerbPlan::Answer(Reply::Shield("Shield: unknown".into())));
}

#[test]
fn status_reads_the_list_only_while_on_and_listable() {
    assert_eq!(plan_verb(ShieldVerb::Status, &ctx(Some(true))), VerbPlan::StatusRead);
    assert_eq!(plan_verb(ShieldVerb::Status, &ctx(Some(false))), VerbPlan::Answer(Reply::Shield("Shield: off".into())));
    let nolist = VerbCtx { can_list: false, ..ctx(Some(true)) };
    assert_eq!(plan_verb(ShieldVerb::Status, &nolist), VerbPlan::Answer(Reply::Shield("Shield: on".into())));
}

#[test]
fn allow_needs_the_mode_on_and_both_commands() {
    assert_eq!(plan_verb(ShieldVerb::Allow, &ctx(Some(true))), VerbPlan::AllowRead);
    assert_eq!(plan_verb(ShieldVerb::Allow, &ctx(Some(false))), error(ERR_OFF));
    let noallow = VerbCtx { can_allow: false, ..ctx(Some(true)) };
    assert_eq!(plan_verb(ShieldVerb::Allow, &noallow), error(ERR_NO_ALLOW));
    let nolist = VerbCtx { can_list: false, ..ctx(Some(true)) };
    assert_eq!(plan_verb(ShieldVerb::Allow, &nolist), error(ERR_NO_ALLOW));
}

#[test]
fn the_picker_lines_ids_and_names_go_together() {
    use crate::shield::usb::UsbDevice;
    let d = |id, n: &str| UsbDevice { id, vid_pid: "1234:5678".into(), name: n.into() };
    let (lines, ids) = picker(&[d(7, "Test Keyboard"), d(12, "Test Stick")]);
    assert_eq!(lines, ["1234:5678  ·  Test Keyboard", "1234:5678  ·  Test Stick"]);
    assert_eq!(ids, [(7, "Test Keyboard".to_string()), (12, "Test Stick".to_string())]);
    // What the chosen index maps to: only a real position, never typed text. The name goes with the
    // id (the notice names the device; the listing is gone by then, so `?` was announced).
    assert_eq!(picked(&ids, Reply::Index(Some(1))), Some((12, "Test Stick".to_string())));
    assert_eq!(picked(&ids, Reply::Index(Some(2))), None);
    assert_eq!(picked(&ids, Reply::Index(None)), None, "typed text runs nothing");
    assert_eq!(picked(&ids, Reply::Chosen("1234:5678  ·  Test Stick".into())), None);
    assert_eq!(picked(&ids, Reply::Cancelled), None);
}

#[test]
fn the_panels_allow_names_the_device_from_its_listing() {
    use crate::shield::usb::UsbDevice;
    let d = |id, n: &str| UsbDevice { id, vid_pid: "1234:5678".into(), name: n.into() };
    let ok = Ok(Parsed { devices: vec![d(7, "Test Keyboard"), d(12, "Test Stick")], more: false });
    assert_eq!(device_name(Some(&ok), 12), "Test Stick");
    assert_eq!(device_name(Some(&ok), 99), "?", "an id the listing lacks");
    assert_eq!(device_name(Some(&Err(Reason::TimedOut)), 12), "?");
    assert_eq!(device_name(None, 12), "?");
}

#[test]
fn the_status_text_never_passes_the_reply_cap() {
    use crate::shield::usb::UsbDevice;
    use alpheus_common::launcher_ipc::MAX_SHIELD_BODY;
    // 64 devices of 64 four-byte characters: 256 bytes a name, past 16 KiB in all.
    let name = "\u{1F600}".repeat(64);
    let devices: Vec<UsbDevice> = (0..64).map(|id| UsbDevice { id, vid_pid: "1234:5678".into(), name: name.clone() }).collect();
    let p = Parsed { devices, more: false };
    let text = status_text(Some(true), Some(Ok(&p)));
    assert!(text.len() <= MAX_SHIELD_BODY, "{} bytes", text.len());
    let last = text.lines().last().unwrap();
    assert!(last.starts_with("  \u{2026} ") && last.ends_with(" more"), "the cut is said: {last}");
    let shown = text.lines().filter(|l| l.contains(&name)).count();
    assert!(shown > 0 && last.contains(&(64 - shown).to_string()), "{shown} shown, {last}");
    // A short list is never cut.
    let few = Parsed { devices: p.devices[..3].to_vec(), more: false };
    let text = status_text(Some(true), Some(Ok(&few)));
    assert_eq!(text.lines().count(), 5);
    assert!(!text.contains(" more"));
}

/// Every refusal a verb can get carries a word of the log's closed list (one wording per condition).
#[test]
fn every_refusal_is_a_word_of_the_closed_list() {
    let states = [None, Some(false), Some(true)];
    for verb in [ShieldVerb::On, ShieldVerb::Off, ShieldVerb::Toggle, ShieldVerb::Status, ShieldVerb::Allow] {
        for state in states {
            for (action_busy, can_list, can_allow) in [(false, true, true), (true, true, true), (false, false, true), (false, true, false)]
            {
                let c = VerbCtx { state, action_busy, can_list, can_allow };
                if let VerbPlan::Answer(Reply::Error(e)) = plan_verb(verb, &c) {
                    assert!(ERR_ALL.contains(&e.as_str()), "{verb:?} {state:?}: {e}");
                }
            }
        }
    }
    let mut sorted = ERR_ALL.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ERR_ALL.len(), "one wording per condition");
}
