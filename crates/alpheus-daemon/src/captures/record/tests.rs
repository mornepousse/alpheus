//! Tests of the recording (`record.rs`): the recorder's argv, the stop that reaches it alone, what
//! its end means, and the daemon's shutdown. The executor is a `FakeExecutor`: no recorder runs; the
//! test plays it by writing its file and ending its fake child.

use super::*;
use crate::config::CapturesConfig;
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::temp_path::TempPath;

const NAME: &str = "screenrecord-20261125-101500.mp4";

/// `record-area` starts the recorder with the area as one whole `-g` argument and the free file as
/// `-f`; `stop` sends SIGINT to that child, and to no other child of the daemon; its end with a file
/// is a saved recording named by the file.
#[test]
fn record_spawns_with_the_geometry_and_stop_sends_sigint_to_it_only() {
    let root = TempPath::dir("record-start");
    let videos = root.join("Videos");
    let exec = FakeExecutor::new();
    exec.watch_ok("satty", None);
    exec.watch_ok("wf-recorder", Some(Some(0)));
    // Another child of the daemon, an editor left open, must never get the recorder's signal.
    let editor = exec.spawn_watched("satty", &["--filename", "x.png"], None).unwrap();
    let template = CapturesConfig::default().record;
    let mut recording = Recording::start(&exec, &template, "1930,20 300x200", &videos, NAME).expect("started");
    let file = videos.join(NAME);
    assert_eq!(exec.calls()[1], format!("wf-recorder -g 1930,20 300x200 -f {}", file.display()));
    assert_eq!(exec.children()[1].stdout(), None);
    assert!(videos.is_dir(), "the folder is made");
    assert_eq!(recording.try_exit(), None, "it records");
    std::fs::write(&file, b"a recording").unwrap();
    assert!(recording.stop());
    let children = exec.children();
    assert_eq!(children[1].interrupts(), 1, "the recorder got SIGINT");
    assert_eq!(children[0].interrupts(), 0, "no other child did");
    let exit = recording.try_exit().expect("it finished its file and exited");
    assert_eq!(recording.finish(exit), Finish::Recorded(NAME.into()));
    assert!(file.exists(), "the recording is the user's");
    drop(editor);
}

/// The geometry is a whole argument wherever the template puts it, and a recorder without
/// `{geometry}` gets none.
#[test]
fn the_geometry_and_the_file_are_whole_arguments() {
    let root = TempPath::dir("record-argv");
    let exec = FakeExecutor::new();
    exec.watch_ok("rec", None);
    let template: Vec<String> = ["rec", "--file", "{out}", "--area", "{geometry}"].map(String::from).to_vec();
    let _r = Recording::start(&exec, &template, "0,0 1280x720", &root, NAME).unwrap();
    assert_eq!(exec.calls(), [format!("rec --file {} --area 0,0 1280x720", root.join(NAME).display())]);
    let template: Vec<String> = ["rec", "-o", "{out}"].map(String::from).to_vec();
    let _r = Recording::start(&exec, &template, "0,0 1280x720", &root, "other.mp4").unwrap();
    assert_eq!(exec.calls()[1], format!("rec -o {}", root.join("other.mp4").display()));
}

/// A name that is taken gets a number: a recording never writes over a file of the user.
#[test]
fn a_recording_never_writes_over_a_file() {
    let root = TempPath::dir("record-free");
    std::fs::write(root.join(NAME), "an older recording").unwrap();
    std::fs::write(root.join("screenrecord-20261125-101500-2.mp4"), "another").unwrap();
    let exec = FakeExecutor::new();
    exec.watch_ok("wf-recorder", None);
    let r = Recording::start(&exec, &CapturesConfig::default().record, "0,0 10x10", &root, NAME).unwrap();
    assert!(exec.calls()[0].ends_with("screenrecord-20261125-101500-3.mp4"), "{:?}", exec.calls());
    assert_eq!(std::fs::read_to_string(root.join(NAME)).unwrap(), "an older recording");
    assert_eq!(r.finish(Some(0)), Finish::RecordFailed, "no file was written");
}

/// A recorder that exits with another code, is killed, cannot start or leaves an empty or no file is
/// a failure; an empty file it left goes, one with something in it stays.
#[test]
fn a_recording_that_fails_or_leaves_nothing_is_a_failure() {
    let root = TempPath::dir("record-fail");
    let template = CapturesConfig::default().record;
    let exec = FakeExecutor::new();
    exec.watch_ok("wf-recorder", None);
    for (exit, content) in [(Some(1), Some("partial")), (None, Some("partial")), (Some(0), Some("")), (Some(0), None)] {
        let name = format!("rec-{exit:?}-{}.mp4", content.map_or(0, str::len));
        let r = Recording::start(&exec, &template, "0,0 10x10", &root, &name).unwrap();
        if let Some(c) = content {
            std::fs::write(root.join(&name), c).unwrap();
        }
        assert_eq!(r.finish(exit), Finish::RecordFailed, "{exit:?} {content:?}");
        assert_eq!(root.join(&name).exists(), content.is_some_and(|c| !c.is_empty()), "{exit:?} {content:?}");
    }
    assert!(Recording::start(&FakeExecutor::new(), &template, "0,0 10x10", &root, "x.mp4").is_none(), "not found");
    assert!(Recording::start(&exec, &[], "0,0 10x10", &root, "y.mp4").is_none(), "no program");
}

/// The daemon stopping while it records sends SIGINT to the recorder, waits for it at most 2 s, and
/// says how it ended: a recorder that finishes its file is a saved recording. One that ignores the
/// signal is killed at the limit, and the recording counts as failed.
#[test]
fn the_daemon_stop_ends_the_recording_with_sigint() {
    assert_eq!(STOP_LIMIT, Duration::from_secs(2));
    let root = TempPath::dir("record-shutdown");
    let template = CapturesConfig::default().record;
    let exec = FakeExecutor::new();
    exec.watch_ok("wf-recorder", Some(Some(0)));
    let recording = Recording::start(&exec, &template, "0,0 1280x720", &root, NAME).unwrap();
    std::fs::write(root.join(NAME), b"frames").unwrap();
    assert_eq!(recording.end_at_shutdown(STOP_LIMIT), Finish::Recorded(NAME.into()));
    let child = &exec.children()[0];
    assert_eq!(child.interrupts(), 1, "SIGINT, not a kill");
    assert_eq!(child.waits(), [STOP_LIMIT], "waited for at most the limit");
    assert!(!child.killed());
    let deaf = FakeExecutor::new();
    deaf.watch_ok("wf-recorder", None);
    let recording = Recording::start(&deaf, &template, "0,0 1280x720", &root, "deaf.mp4").unwrap();
    assert_eq!(recording.end_at_shutdown(STOP_LIMIT), Finish::RecordFailed);
    assert_eq!(deaf.children()[0].interrupts(), 1);
    assert!(deaf.children()[0].killed(), "killed once the wait is over");
}
