//! Tests of the capture flow's pure half (`mod.rs`): which verb does what, one capture at a time,
//! and the three invariants of `screenshot.sh` (a cancel is silent, the success notice follows a
//! successful capture only, a failure is critical), the clipboard being the worker's (`deliver.rs`).

use super::*;
use alpheus_common::launcher_ipc::CaptureVerb;
use alpheus_common::theme::Rgb;

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
    for v in [CaptureVerb::Annotate, CaptureVerb::Ocr, CaptureVerb::Record, CaptureVerb::RecordArea, CaptureVerb::Stop] {
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

/// A flow at the point of waiting for its frame: begun, selected when it is an area, settled.
/// Returns it with its generation.
fn grabbing(scope: Scope) -> (Flow, u64) {
    let mut flow = Flow::default();
    flow.begin(job(scope, Goal::Copy));
    let g = flow.generation();
    if scope == Scope::Area {
        flow.selected(g, SelectorAnswer::Area(rect(1, 2, 3, 4)));
    }
    flow.settled(g);
    (flow, g)
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
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Copy)), Some(Step::Settle));
    assert_eq!(flow.begin(job(Scope::Area, Goal::Copy)), None, "a full screen in flight holds the slot too");
}

#[test]
fn every_scope_waits_for_the_compositor_before_its_frame() {
    // A whole screen is taken right after a menu may have closed (Tools, Screenshot): its frame
    // waits for the same round trip as an area's, so a closing surface is not in it.
    let mut flow = Flow::default();
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Copy)), Some(Step::Settle));
    let g = flow.generation();
    assert_eq!(flow.grabbed(g, true), Step::Nothing, "no frame was asked yet");
    assert_eq!(flow.settled(g), Step::Grab(None), "the whole screen: no area");
    assert_eq!(flow.settled(g), Step::Nothing, "once");
}

#[test]
fn a_cancel_is_silent_and_never_copies() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Area, Goal::Copy));
    assert_eq!(flow.selected(flow.generation(), SelectorAnswer::Cancelled), Step::Finish(Finish::Cancelled));
    assert_eq!(notice(&Finish::Cancelled, Scope::Area), None, "no notice");
    assert!(!Finish::Cancelled.touches_the_clipboard());
    // The selector asked for nothing (the debug verb): its answer is not a capture's.
    let mut idle = Flow::default();
    assert_eq!(idle.selected(0, SelectorAnswer::Area(rect(0, 0, 1, 1))), Step::Nothing);
    assert_eq!(idle.selected(0, SelectorAnswer::Cancelled), Step::Nothing);
}

#[test]
fn a_selection_waits_for_the_compositor_before_the_frame_is_asked() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Area, Goal::Copy));
    let g = flow.generation();
    // The selector's surface was just destroyed: the frame is not asked in the same turn.
    assert_eq!(flow.selected(g, SelectorAnswer::Area(rect(10, 20, 30, 40))), Step::Settle);
    assert!(flow.is_busy());
    // A frame answer, or a second settle, out of turn does nothing.
    assert_eq!(flow.grabbed(g, true), Step::Nothing);
    assert_eq!(flow.settled(g), Step::Grab(Some(rect(10, 20, 30, 40))));
    assert_eq!(flow.settled(g), Step::Nothing, "once");
    assert_eq!(flow.grabbed(g, true), Step::Encode);
    assert_eq!(flow.grabbed(g, true), Step::Nothing, "once");
}

#[test]
fn a_failed_frame_is_a_failure() {
    let (mut flow, g) = grabbing(Scope::Screen);
    assert_eq!(flow.grabbed(g, false), Step::Finish(Finish::CaptureFailed));
    flow.finished();
    assert!(!flow.is_busy());
}

/// A frame that never comes (a compositor that drops the request) must not hold the slot for ever:
/// the deadline fails the capture, once, and only the capture that was waiting.
#[test]
fn a_frame_that_never_comes_fails_the_capture() {
    assert_eq!(GRAB_LIMIT, std::time::Duration::from_secs(5));
    let (mut flow, g) = grabbing(Scope::Screen);
    assert_eq!(flow.grab_expired(g), Step::Finish(Finish::CaptureFailed));
    assert!(!flow.is_busy(), "the slot is free");
    assert_eq!(flow.grab_expired(g), Step::Nothing, "once");
    // A deadline of an earlier capture, fired late, never fails the next one.
    flow.begin(job(Scope::Screen, Goal::Copy));
    let g2 = flow.generation();
    flow.settled(g2);
    assert_eq!(flow.grab_expired(g), Step::Nothing);
    assert_eq!(flow.grabbed(g2, true), Step::Encode, "the new capture goes on");
    // And a deadline that fires once the frame is in does nothing.
    assert_eq!(flow.grab_expired(g2), Step::Nothing);
}

/// A late frame of capture N, answered after N timed out and N+1 began, is never taken for N+1's.
#[test]
fn a_late_frame_from_an_old_generation_is_ignored() {
    let (mut flow, old) = grabbing(Scope::Screen);
    flow.grab_expired(old);
    flow.begin(job(Scope::Screen, Goal::Save));
    let new = flow.generation();
    assert_ne!(old, new);
    assert_eq!(flow.settled(old), Step::Nothing, "a late round trip of N does not release N+1");
    assert_eq!(flow.settled(new), Step::Grab(None));
    assert_eq!(flow.grabbed(old, true), Step::Nothing, "N's frame is not N+1's");
    assert_eq!(flow.grabbed(old, false), Step::Nothing, "nor N's failure");
    assert!(flow.is_busy(), "N+1 still waits for its own");
    assert_eq!(flow.grabbed(new, true), Step::Encode);
}

#[test]
fn the_notices_say_what_happened_and_only_a_success_is_a_calm_one() {
    let n = |f: Finish| notice(&f, Scope::Screen).unwrap();
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
    assert_eq!(notice(&Finish::Cancelled, Scope::Area), None);
    assert!(Finish::Copied(Scope::Area).touches_the_clipboard());
    assert!(!Finish::Saved("x.png".into()).touches_the_clipboard());
}

/// A worker's ending carries its capture's number: one left over from a capture the glue reset (a
/// panic while the worker ran) never ends the next capture.
#[test]
fn a_late_ending_from_an_old_generation_is_ignored() {
    let (mut flow, old) = grabbing(Scope::Screen);
    assert_eq!(flow.grabbed(old, true), Step::Encode);
    flow.finished();
    flow.begin(job(Scope::Screen, Goal::Copy));
    let new = flow.generation();
    flow.settled(new);
    assert_eq!(flow.grabbed(new, true), Step::Encode);
    assert_eq!(flow.delivered(old, Finish::Copied(Scope::Screen)), Step::Nothing, "the old worker's ending");
    assert!(flow.is_busy(), "the new capture still waits for its own worker");
    assert_eq!(flow.delivered(new, Finish::Copied(Scope::Screen)), Step::Finish(Finish::Copied(Scope::Screen)));
}

/// A selector that could not open, or that the compositor took away (its layer closed, its output
/// gone), is a failure the user is told about, never a silent cancel; it copies nothing.
#[test]
fn a_selector_that_fails_ends_the_capture_as_a_critical_failure() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Area, Goal::Copy));
    let step = flow.selected(flow.generation(), SelectorAnswer::Failed);
    assert_eq!(step, Step::Finish(Finish::CaptureFailed));
    assert!(!flow.is_busy());
    assert!(notice(&Finish::CaptureFailed, Scope::Area).unwrap().critical && !Finish::CaptureFailed.touches_the_clipboard());
}

/// A selector left open by a capture that was reset (a panic in the glue) answers with that
/// capture's generation: when the next capture's selector replaces it, its cancel is not taken for
/// the new capture's, which still waits for its own selection.
#[test]
fn a_selector_answer_of_an_earlier_capture_does_not_end_the_current_one() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Area, Goal::Copy));
    let stale = flow.generation();
    flow.finished();
    assert_eq!(flow.begin(job(Scope::Area, Goal::Copy)), Some(Step::Select));
    let current = flow.generation();
    assert_eq!(flow.selected(stale, SelectorAnswer::Cancelled), Step::Nothing, "the old selector's close");
    assert_eq!(flow.selected(stale, SelectorAnswer::Area(rect(1, 2, 3, 4))), Step::Nothing);
    assert!(flow.is_busy(), "the new capture still runs");
    assert_eq!(flow.selected(current, SelectorAnswer::Area(rect(5, 6, 7, 8))), Step::Settle, "its own selector answers");
}

/// The colour picker is a capture of the whole focused output, settled like the others (a menu may
/// be closing), whose frame opens the picker instead of going to the worker.
#[test]
fn a_pick_settles_grabs_the_screen_then_opens_the_picker() {
    let pick = job(Scope::Pick, Goal::Copy);
    assert_eq!(plan(CaptureVerb::Pick, true), Plan::Capture(pick));
    let mut flow = Flow::default();
    assert_eq!(flow.begin(pick), Some(Step::Settle));
    let g = flow.generation();
    assert_eq!(flow.picked(g, PickAnswer::Colour(Rgb(1, 2, 3))), Step::Nothing, "no picker is open yet");
    assert_eq!(flow.settled(g), Step::Grab(None), "the whole output");
    assert_eq!(flow.grabbed(g, true), Step::Pick, "the frame opens the picker, never the encoder");
    assert!(!flow.waits_for_frame(), "the deadline is the frame's only: the picker waits for the user");
    assert_eq!(flow.grabbed(g, true), Step::Nothing, "once");
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Copy)), None, "one capture at a time, the picker included");
    assert_eq!(flow.picked(g, PickAnswer::Colour(Rgb(0x1A, 0x2B, 0xFC))), Step::Copy(Rgb(0x1A, 0x2B, 0xFC)));
    assert_eq!(flow.picked(g, PickAnswer::Cancelled), Step::Nothing, "the answer is taken once");
    let copied = Finish::Picked("#1A2BFC".into());
    assert_eq!(flow.delivered(g, copied.clone()), Step::Finish(copied.clone()));
    assert!(!flow.is_busy());
    assert!(copied.touches_the_clipboard());
    let n = notice(&copied, Scope::Pick).unwrap();
    assert_eq!((n.summary, n.body.as_str(), n.critical), ("Colour picker", "Copied: #1A2BFC", false), "the notice gives the value");
}

/// A frame that does not come fails the pick before any picker opens.
#[test]
fn a_pick_without_its_frame_is_a_failure() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Pick, Goal::Copy));
    let g = flow.generation();
    flow.settled(g);
    assert_eq!(flow.grabbed(g, false), Step::Finish(Finish::CaptureFailed));
}

/// A picker that could not open, or that the compositor took away (its layer closed, its output
/// gone), fails the pick as the area selector does: critical, nothing copied.
#[test]
fn a_picker_that_fails_ends_the_pick_as_a_critical_failure() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Pick, Goal::Copy));
    let g = flow.generation();
    flow.settled(g);
    flow.grabbed(g, true);
    assert_eq!(flow.picked(g, PickAnswer::Failed), Step::Finish(Finish::CaptureFailed));
    assert!(!flow.is_busy());
    assert!(notice(&Finish::CaptureFailed, Scope::Pick).unwrap().critical && !Finish::CaptureFailed.touches_the_clipboard());
}

/// An answer of an earlier capture's picker never ends the current one.
#[test]
fn a_picker_answer_of_an_earlier_capture_is_ignored() {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Pick, Goal::Copy));
    let stale = flow.generation();
    flow.finished();
    flow.begin(job(Scope::Pick, Goal::Copy));
    let g = flow.generation();
    flow.settled(g);
    flow.grabbed(g, true);
    assert_eq!(flow.picked(stale, PickAnswer::Cancelled), Step::Nothing);
    assert_eq!(flow.picked(stale, PickAnswer::Colour(Rgb(0, 0, 0))), Step::Nothing);
    assert!(flow.is_busy());
    assert_eq!(flow.picked(g, PickAnswer::Cancelled), Step::Finish(Finish::Cancelled));
}

/// A pick's notices are the colour picker's, its failures included; the screenshots' stay
/// `Screenshot`. The flow keeps the scope of the capture that ended, which the glue reads for the
/// notice once the flow is at rest.
#[test]
fn a_picks_failure_notice_is_the_colour_pickers() {
    for failure in [Finish::CaptureFailed, Finish::CopyFailed] {
        let pick = notice(&failure, Scope::Pick).unwrap();
        assert_eq!((pick.summary, pick.critical), ("Colour picker", true), "{failure:?}");
        assert_eq!(notice(&failure, Scope::Screen).unwrap().summary, "Screenshot");
        assert_eq!(notice(&failure, Scope::Area).unwrap().summary, "Screenshot");
    }
    let mut flow = Flow::default();
    flow.begin(job(Scope::Pick, Goal::Copy));
    let g = flow.generation();
    assert_eq!(flow.settled(g), Step::Grab(None));
    assert_eq!(flow.grabbed(g, false), Step::Finish(Finish::CaptureFailed));
    assert_eq!(flow.scope(), Scope::Pick, "the ended capture's scope");
    flow.begin(job(Scope::Area, Goal::Save));
    assert_eq!(flow.scope(), Scope::Area);
}
