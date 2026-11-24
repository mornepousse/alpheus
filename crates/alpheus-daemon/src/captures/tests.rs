//! Tests of the capture flow's pure half (`mod.rs`): which verb does what, one capture at a time,
//! and the three invariants of `screenshot.sh` (a cancel is silent, the success notice follows a
//! successful capture only, a failure is critical), the clipboard being the worker's (`deliver.rs`).

use super::*;
use alpheus_common::launcher_ipc::CaptureVerb;

fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

fn job(scope: Scope, goal: Goal) -> Job {
    Job { scope, goal }
}

#[test]
fn each_verb_is_planned_and_the_unbuilt_ones_say_so() {
    let full = Plan::Capture(job(Scope::Screen, Goal::Copy));
    let area = Plan::Capture(job(Scope::Area, Goal::Copy));
    let area_save = Plan::Capture(job(Scope::Area, Goal::Save));
    assert_eq!(plan(CaptureVerb::Full, true), full);
    assert_eq!(plan(CaptureVerb::Area, true), area);
    assert_eq!(plan(CaptureVerb::AreaSave, true), area_save);
    assert_eq!(plan(CaptureVerb::Select, true), Plan::Select);
    for v in [CaptureVerb::Pick, CaptureVerb::Annotate, CaptureVerb::Ocr, CaptureVerb::Record, CaptureVerb::RecordArea, CaptureVerb::Stop] {
        assert_eq!(plan(v, true), Plan::Refuse(Refusal::NotYet), "{v:?}");
    }
}

#[test]
fn no_screencopy_refuses_every_capture_but_not_the_debug_selector() {
    for v in CaptureVerb::ALL {
        let expected = if v == CaptureVerb::Select { Plan::Select } else { Plan::Refuse(Refusal::NoScreencopy) };
        assert_eq!(plan(v, false), expected, "{v:?}");
    }
    assert!(Refusal::NoScreencopy.text().contains("screencopy"));
    assert_ne!(Refusal::NoScreencopy.text(), Refusal::NotYet.text());
}

#[test]
fn one_capture_at_a_time() {
    let mut flow = Flow::default();
    assert!(!flow.is_busy());
    assert_eq!(flow.begin(job(Scope::Area, Goal::Copy)), Some(Step::Select));
    assert!(flow.is_busy());
    // Whatever is asked while one runs is refused, a full screen as much as an area.
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Copy)), None);
    assert_eq!(flow.begin(job(Scope::Area, Goal::Save)), None);
    flow.finished();
    assert!(!flow.is_busy());
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Copy)), Some(Step::Grab(None)));
    assert_eq!(flow.begin(job(Scope::Area, Goal::Copy)), None, "a full screen in flight holds the slot too");
}

#[test]
fn a_cancel_is_silent_and_never_copies() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Area, Goal::Copy));
    assert_eq!(flow.selected(None), Step::Finish(Finish::Cancelled));
    assert_eq!(notice(&Finish::Cancelled), None, "no notice");
    assert!(!Finish::Cancelled.touches_the_clipboard());
    // The selector asked for nothing (the debug verb): its answer is not a capture's.
    let mut idle = Flow::default();
    assert_eq!(idle.selected(Some(rect(0, 0, 1, 1))), Step::Nothing);
    assert_eq!(idle.selected(None), Step::Nothing);
}

#[test]
fn a_selection_waits_for_the_compositor_before_the_frame_is_asked() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Area, Goal::Copy));
    // The selector's surface was just destroyed: the frame is not asked in the same turn.
    assert_eq!(flow.selected(Some(rect(10, 20, 30, 40))), Step::Settle);
    assert!(flow.is_busy());
    // A frame answer, or a second settle, out of turn does nothing.
    assert_eq!(flow.grabbed(true), Step::Nothing);
    assert_eq!(flow.settled(), Step::Grab(Some(rect(10, 20, 30, 40))));
    assert_eq!(flow.settled(), Step::Nothing, "once");
    assert_eq!(flow.grabbed(true), Step::Encode);
    assert_eq!(flow.grabbed(true), Step::Nothing, "once");
}

#[test]
fn a_failed_frame_is_a_failure() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Screen, Goal::Copy));
    assert_eq!(flow.grabbed(false), Step::Finish(Finish::CaptureFailed));
    flow.finished();
    assert!(!flow.is_busy());
}

#[test]
fn the_notices_say_what_happened_and_only_a_success_is_a_calm_one() {
    let n = |f: Finish| notice(&f).unwrap();
    let copied = n(Finish::Copied(Scope::Screen));
    assert_eq!((copied.summary, copied.body.as_str(), copied.critical), ("Screenshot", "Screen copied", false));
    assert_eq!(n(Finish::Copied(Scope::Area)).body, "Area copied");
    let saved = n(Finish::Saved("20261104-090507.png".into()));
    assert_eq!((saved.summary, saved.body.as_str(), saved.critical), ("Screenshot", "Saved: 20261104-090507.png", false));
    for failure in [Finish::CaptureFailed, Finish::CopyFailed, Finish::SaveFailed] {
        let f = n(failure.clone());
        assert!(f.critical, "{failure:?} is critical");
        assert!(!failure.touches_the_clipboard(), "{failure:?} leaves the clipboard alone");
    }
    assert_eq!(notice(&Finish::Cancelled), None);
    assert!(Finish::Copied(Scope::Area).touches_the_clipboard());
    assert!(!Finish::Saved("x.png".into()).touches_the_clipboard());
}
