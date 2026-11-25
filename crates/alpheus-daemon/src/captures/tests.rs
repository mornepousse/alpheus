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

/// Every external tool named, as the defaults do.
const EVERY_TOOL: Tools = Tools { annotate: true, ocr: true, record: true };

#[test]
fn each_verb_is_planned_and_a_tool_set_to_nothing_refuses_its_verb() {
    let planned = |v| plan(v, true, EVERY_TOOL);
    assert_eq!(planned(CaptureVerb::Full), Plan::Capture(job(Scope::Screen, Goal::Copy)));
    assert_eq!(planned(CaptureVerb::Area), Plan::Capture(job(Scope::Area, Goal::Copy)));
    assert_eq!(planned(CaptureVerb::AreaSave), Plan::Capture(job(Scope::Area, Goal::Save)));
    assert_eq!(planned(CaptureVerb::Annotate), Plan::Capture(job(Scope::Area, Goal::Annotate)));
    assert_eq!(planned(CaptureVerb::Ocr), Plan::Capture(job(Scope::Area, Goal::Ocr)));
    assert_eq!(planned(CaptureVerb::Record), Plan::Capture(job(Scope::Screen, Goal::Record)));
    assert_eq!(planned(CaptureVerb::RecordArea), Plan::Capture(job(Scope::Area, Goal::Record)));
    assert_eq!(planned(CaptureVerb::Stop), Plan::Stop);
    assert_eq!(planned(CaptureVerb::Select), Plan::Select);
    // Each key set to `[]` refuses its own verbs, and only those.
    let none = Tools::default();
    for (v, tools) in [
        (CaptureVerb::Annotate, Tools { annotate: false, ..EVERY_TOOL }),
        (CaptureVerb::Ocr, Tools { ocr: false, ..EVERY_TOOL }),
        (CaptureVerb::Record, Tools { record: false, ..EVERY_TOOL }),
        (CaptureVerb::RecordArea, Tools { record: false, ..EVERY_TOOL }),
    ] {
        assert_eq!(plan(v, true, tools), Plan::Refuse(Refusal::NoTool), "{v:?}");
    }
    assert_eq!(plan(CaptureVerb::Annotate, true, Tools { annotate: true, ..none }), planned(CaptureVerb::Annotate));
    for v in [CaptureVerb::Full, CaptureVerb::Area, CaptureVerb::AreaSave, CaptureVerb::Pick, CaptureVerb::Stop] {
        assert_eq!(plan(v, true, none), planned(v), "{v:?} needs no tool");
    }
}

#[test]
fn no_screencopy_refuses_every_capture_but_not_the_debug_selector_nor_a_stop() {
    for v in CaptureVerb::ALL {
        let expected = match v {
            CaptureVerb::Select => Plan::Select,
            CaptureVerb::Stop => Plan::Stop,
            _ => Plan::Refuse(Refusal::NoScreencopy),
        };
        assert_eq!(plan(v, false, EVERY_TOOL), expected, "{v:?}");
    }
    assert!(Refusal::NoScreencopy.text().contains("screencopy"));
    let texts = [Refusal::NoScreencopy.text(), Refusal::NoTool.text(), Refusal::NotRecording.text()];
    assert!(texts.iter().all(|t| texts.iter().filter(|u| *u == t).count() == 1), "{texts:?}");
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
    assert_eq!(notice(&Finish::Cancelled, job(Scope::Area, Goal::Copy)), None, "no notice");
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
    let n = |f: Finish| notice(&f, job(Scope::Screen, Goal::Copy)).unwrap();
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
    assert_eq!(notice(&Finish::Cancelled, job(Scope::Area, Goal::Copy)), None);
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
    assert!(
        notice(&Finish::CaptureFailed, job(Scope::Area, Goal::Copy)).unwrap().critical && !Finish::CaptureFailed.touches_the_clipboard()
    );
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
    assert_eq!(plan(CaptureVerb::Pick, true, EVERY_TOOL), Plan::Capture(pick));
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
    let n = notice(&copied, job(Scope::Pick, Goal::Copy)).unwrap();
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
    assert!(
        notice(&Finish::CaptureFailed, job(Scope::Pick, Goal::Copy)).unwrap().critical && !Finish::CaptureFailed.touches_the_clipboard()
    );
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
        let pick = notice(&failure, job(Scope::Pick, Goal::Copy)).unwrap();
        assert_eq!((pick.summary, pick.critical), ("Colour picker", true), "{failure:?}");
        assert_eq!(notice(&failure, job(Scope::Screen, Goal::Copy)).unwrap().summary, "Screenshot");
        assert_eq!(notice(&failure, job(Scope::Area, Goal::Copy)).unwrap().summary, "Screenshot");
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

/// A flow whose worker just wrote the image for the external tool of `goal`: begun, selected,
/// settled, grabbed, prepared. Returns it with its generation.
fn tool_running(goal: Goal) -> (Flow, u64) {
    let mut flow = Flow::default();
    flow.begin(job(Scope::Area, goal));
    let g = flow.generation();
    assert_eq!(flow.selected(g, SelectorAnswer::Area(rect(1, 2, 3, 4))), Step::Settle);
    assert_eq!(flow.settled(g), Step::Grab(Some(rect(1, 2, 3, 4))));
    assert_eq!(flow.grabbed(g, true), Step::Encode, "the frame is encoded for the tool");
    assert_eq!(flow.delivered(g, Finish::Prepared), Step::RunTool, "the image written, the tool runs");
    (flow, g)
}

/// An annotation: the area's image goes to the tool, and what it writes back is copied by the
/// worker; the notice comes after the copy only.
#[test]
fn an_annotation_runs_the_tool_then_copies_what_it_wrote() {
    let (mut flow, g) = tool_running(Goal::Annotate);
    assert!(flow.is_busy() && flow.waits_for_tool(), "one capture at a time, the tool's run included");
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Copy)), None, "a screenshot waits for the editor to close");
    assert_eq!(flow.tool_ended(g, ToolEnd::Annotated), Step::CopyAnnotated);
    assert!(!flow.waits_for_tool());
    assert_eq!(flow.tool_ended(g, ToolEnd::Annotated), Step::Nothing, "once");
    assert_eq!(flow.delivered(g, Finish::Annotated), Step::Finish(Finish::Annotated));
    assert!(!flow.is_busy());
    let n = notice(&Finish::Annotated, flow.last_job()).unwrap();
    assert_eq!((n.summary, n.body.as_str(), n.critical), ("Screenshot", "Annotated image copied", false));
    assert!(Finish::Annotated.touches_the_clipboard());
    // The copy of the tool's image may fail like any copy: critical, the clipboard untouched.
    let (mut flow, g) = tool_running(Goal::Annotate);
    flow.tool_ended(g, ToolEnd::Annotated);
    assert_eq!(flow.delivered(g, Finish::CopyFailed), Step::Finish(Finish::CopyFailed));
}

/// A text recognition: the text the tool printed is copied; the notice says so without the text.
#[test]
fn a_recognition_copies_the_text_and_its_notice_never_holds_it() {
    let (mut flow, g) = tool_running(Goal::Ocr);
    assert_eq!(flow.tool_ended(g, ToolEnd::Text("Hello there".into())), Step::CopyText("Hello there".into()));
    assert_eq!(flow.delivered(g, Finish::TextCopied), Step::Finish(Finish::TextCopied));
    let n = notice(&Finish::TextCopied, flow.last_job()).unwrap();
    assert_eq!((n.summary, n.body.as_str(), n.critical), ("Text from area", "Text copied", false));
    assert!(!n.body.contains("Hello"));
    assert!(Finish::TextCopied.touches_the_clipboard());
}

/// A tool closed with no output is a silent cancel; one that failed is critical; neither copies.
#[test]
fn a_tool_closed_without_output_is_silent_and_a_failed_one_is_critical() {
    for goal in [Goal::Annotate, Goal::Ocr] {
        let (mut flow, g) = tool_running(goal);
        assert_eq!(flow.tool_ended(g, ToolEnd::Cancelled), Step::Finish(Finish::Cancelled), "{goal:?}");
        assert!(!flow.is_busy());
        assert_eq!(notice(&Finish::Cancelled, flow.last_job()), None);
        let (mut flow, g) = tool_running(goal);
        assert_eq!(flow.tool_ended(g, ToolEnd::Failed), Step::Finish(Finish::ToolFailed), "{goal:?}");
        let n = notice(&Finish::ToolFailed, flow.last_job()).unwrap();
        assert!(n.critical, "{goal:?}");
        assert!(!Finish::ToolFailed.touches_the_clipboard());
    }
    assert_eq!(notice(&Finish::ToolFailed, job(Scope::Area, Goal::Annotate)).unwrap().body, "Annotation failed");
    let ocr = notice(&Finish::ToolFailed, job(Scope::Area, Goal::Ocr)).unwrap();
    assert_eq!((ocr.summary, ocr.body.as_str()), ("Text from area", "Text recognition failed"));
    assert_eq!(notice(&Finish::Prepared, job(Scope::Area, Goal::Ocr)), None, "a step, never shown");
}

/// The end of an earlier capture's tool never ends the current one, and a tool's end before any
/// tool runs does nothing.
#[test]
fn a_tool_end_of_another_capture_is_ignored() {
    let mut idle = Flow::default();
    assert_eq!(idle.tool_ended(0, ToolEnd::Annotated), Step::Nothing);
    let (mut flow, old) = tool_running(Goal::Annotate);
    // The glue reset the capture while its tool ran, and the next one reached its own tool.
    flow.finished();
    assert_eq!(flow.tool_ended(old, ToolEnd::Annotated), Step::Nothing, "a reset flow has no tool");
    flow.begin(job(Scope::Area, Goal::Ocr));
    let g = flow.generation();
    flow.selected(g, SelectorAnswer::Area(rect(1, 2, 3, 4)));
    flow.settled(g);
    flow.grabbed(g, true);
    assert_eq!(flow.delivered(g, Finish::Prepared), Step::RunTool);
    assert_eq!(flow.tool_ended(old, ToolEnd::Annotated), Step::Nothing, "the old tool's end");
    assert!(flow.waits_for_tool(), "the new capture still waits for its own tool");
    assert_eq!(flow.tool_ended(g, ToolEnd::Cancelled), Step::Finish(Finish::Cancelled));
    // A worker that prepared an image for a capture with no tool fails it rather than hang.
    let (mut copy, g2) = grabbing(Scope::Screen);
    copy.grabbed(g2, true);
    assert_eq!(copy.delivered(g2, Finish::Prepared), Step::Finish(Finish::CaptureFailed));
}

/// A recording is settled like a screenshot (the dim or a closing menu is not recorded), then
/// leaves the slot: the recorder starts and captures can be taken while it runs.
#[test]
fn a_recording_starts_once_settled_and_leaves_the_slot_free() {
    let mut flow = Flow::default();
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Record)), Some(Step::Settle));
    let g = flow.generation();
    assert_eq!(flow.settled(g), Step::Record(None), "the whole output");
    assert!(!flow.is_busy(), "the slot is free while it records");
    assert_eq!(flow.begin(job(Scope::Screen, Goal::Copy)), Some(Step::Settle), "a screenshot during a recording");
    let mut flow = Flow::default();
    assert_eq!(flow.begin(job(Scope::Area, Goal::Record)), Some(Step::Select));
    let g = flow.generation();
    assert_eq!(flow.selected(g, SelectorAnswer::Area(rect(10, 20, 300, 200))), Step::Settle);
    assert_eq!(flow.settled(g), Step::Record(Some(rect(10, 20, 300, 200))));
    assert!(!flow.is_busy());
    // A cancelled selection records nothing, silently.
    flow.begin(job(Scope::Area, Goal::Record));
    assert_eq!(flow.selected(flow.generation(), SelectorAnswer::Cancelled), Step::Finish(Finish::Cancelled));
}

#[test]
fn a_recording_notice_names_its_file_and_a_failed_one_is_critical() {
    let record = job(Scope::Screen, Goal::Record);
    let saved = notice(&Finish::Recorded("screenrecord-20261125-101500.mp4".into()), record).unwrap();
    assert_eq!(
        (saved.summary, saved.body.as_str(), saved.critical),
        ("Screen recording", "Recording saved: screenrecord-20261125-101500.mp4", false)
    );
    let failed = notice(&Finish::RecordFailed, job(Scope::Area, Goal::Record)).unwrap();
    assert_eq!((failed.summary, failed.body.as_str(), failed.critical), ("Screen recording", "Recording failed", true));
    assert!(!Finish::Recorded("x.mp4".into()).touches_the_clipboard());
}

/// A capture asked while the annotation editor is open says so, calmly: nothing else would show why
/// the verb did nothing. Another busy stage (a selector, a recognizer) keeps to the log.
#[test]
fn a_capture_asked_during_an_annotation_is_answered_with_a_notice() {
    let (flow, _) = tool_running(Goal::Annotate);
    let n = flow.ignored_notice().expect("an annotation is open");
    assert_eq!((n.summary, n.body.as_str(), n.critical), ("Screenshot", "Ignored: annotation in progress", false));
    let (flow, _) = tool_running(Goal::Ocr);
    assert_eq!(flow.ignored_notice(), None, "a recognizer is short");
    assert_eq!(Flow::default().ignored_notice(), None, "nothing runs");
}

/// The ignored verb's notice is really sent: `capture_begin` hands the flow's answer to the
/// notifications when it refuses a capture. Reading the source is what pins the wiring, as the
/// notification server is not reachable from a unit test.
#[test]
fn a_refused_capture_sends_the_flows_ignored_notice() {
    let source = include_str!("app.rs");
    let start = source.find("    fn capture_begin(").expect("capture_begin");
    let body = &source[start..start + source[start..].find("\n    }\n").expect("its end")];
    let refused = body.find("self.capture.flow.begin(job)").expect("the flow refuses");
    let asked = body.find("if let Some(n) = self.capture.flow.ignored_notice()").expect("the notice is asked for");
    let sent = body.find("self.capture_notify(n)").expect("and sent");
    assert!(refused < asked && asked < sent, "{body}");
    assert!(body[refused..asked].contains("return;") || body[asked..].contains("return;"), "a refused capture still ends there: {body}");
}
