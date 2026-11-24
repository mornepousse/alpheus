//! Tests of what the worker does with a capture (`deliver.rs`): render it, copy it or save it, and
//! say how it ended. The executor is always a `FakeExecutor`; the files are in a temporary folder.

use super::*;
use crate::captures::screencopy::Format;
use crate::captures::{Scope, notice};
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::temp_path::TempPath;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake image bytes";

fn copy_job(scope: Scope) -> Job {
    Job { scope, goal: Goal::Copy }
}

fn save_job() -> Job {
    Job { scope: Scope::Area, goal: Goal::Save }
}

/// A delivery into `dir` (saves go to `dir/shots`, the private copy file to `dir/run`).
fn delivery<'a>(job: Job, copy: &'a [String], dir: &'a Path, shots: &'a Path) -> Delivery<'a> {
    Delivery { job, copy, save_dir: Some(shots), file_name: "20261104-090507.png".into(), temp_dir: dir }
}

fn wl_copy() -> Vec<String> {
    vec!["wl-copy".to_string()]
}

#[test]
fn the_png_reaches_the_default_copy_command_as_a_file_on_its_stdin() {
    let dir = TempPath::dir("deliver-copy");
    let run = dir.join("run");
    let fake = FakeExecutor::new();
    fake.ok("wl-copy --type image/png", "");
    let copy = wl_copy();
    let finish = deliver(&fake, PNG, &delivery(copy_job(Scope::Area), &copy, &run, &dir.join("shots")));
    assert_eq!(finish, Finish::Copied(Scope::Area));
    assert_eq!(fake.inputs(), [("wl-copy --type image/png".to_string(), PNG.to_vec())], "the type is named for the default copy");
    assert_eq!(std::fs::read_dir(&run).unwrap().count(), 0, "the private file is gone once the copy took it");
    assert!(!dir.join("shots").exists(), "a copy saves nothing");
}

/// "Copied" means the copy command took the image: its exit is checked, as `screenshot.sh` checks
/// `wl-copy`'s. A non-zero exit, a signal and a timeout are all a copy failure, the file is removed.
#[test]
fn a_copy_command_that_exits_badly_or_hangs_is_a_copy_failure() {
    let dir = TempPath::dir("deliver-badexit");
    let run = dir.join("run");
    let copy = wl_copy();
    for (what, answer) in [
        ("non-zero", Ok(alpheus_common::exec::Output { code: Some(1), stdout: String::new(), stderr: String::new() })),
        ("signal", Ok(alpheus_common::exec::Output { code: None, stdout: String::new(), stderr: String::new() })),
        ("timeout", Err(alpheus_common::exec::ExecError::Timeout)),
    ] {
        let fake = FakeExecutor::new();
        fake.respond("wl-copy --type image/png", answer);
        let finish = deliver(&fake, PNG, &delivery(copy_job(Scope::Area), &copy, &run, &dir));
        assert_eq!(finish, Finish::CopyFailed, "{what}");
        assert!(!finish.touches_the_clipboard() && notice(&finish).unwrap().critical, "{what}");
        assert_eq!(std::fs::read_dir(&run).unwrap().count(), 0, "{what}: the file is removed");
    }
}

/// An executor that panics must not strand the capture: the worker still ends it, as a failure,
/// and the private file is not left behind.
#[test]
fn a_panicking_executor_ends_the_capture_as_a_failure_and_leaves_no_file() {
    use alpheus_common::exec::{CaptureLimits, Captured, ExecError, Output};
    struct Panicking;
    impl Executor for Panicking {
        fn run(&self, _: &str, _: &[&str]) -> Result<Output, ExecError> {
            unreachable!()
        }
        fn spawn(&self, _: &str, _: &[&str]) -> Result<(), ExecError> {
            unreachable!()
        }
        fn spawn_with_input(&self, _: &str, _: &[&str], _: &[&[u8]]) -> Result<(), ExecError> {
            unreachable!()
        }
        fn spawn_with_env(&self, _: &str, _: &[&str], _: &[(&str, &str)], _: &[&str]) -> Result<(), ExecError> {
            unreachable!()
        }
        fn stream(&self, _: &str, _: &[&str]) -> Result<alpheus_common::exec::Stream, ExecError> {
            unreachable!()
        }
        fn capture(&self, _: &str, _: &[&str], _: CaptureLimits) -> Result<Captured, ExecError> {
            unreachable!()
        }
        fn run_silent(&self, _: &str, _: &[&str]) -> Result<Option<i32>, ExecError> {
            unreachable!()
        }
        fn run_with_stdin_file(&self, _: &str, _: &[&str], file: &Path, _: Duration) -> Result<Option<i32>, ExecError> {
            assert!(file.exists(), "the file is there when the command is started");
            panic!("a bug in the executor")
        }
    }
    let dir = TempPath::dir("deliver-panic");
    let run_dir = dir.join("run");
    let copy = wl_copy();
    let area = Some(Rect { x: 0, y: 0, width: 2, height: 2 });
    let finish = run(&Panicking, Work::Image { frame: frame(), area }, &delivery(copy_job(Scope::Area), &copy, &run_dir, &dir));
    assert_eq!(finish, Finish::CaptureFailed);
    assert_eq!(std::fs::read_dir(&run_dir).unwrap().count(), 0, "no capture.png left");
    // The same for a picked value: a failure, and no file left.
    let finish = run(&Panicking, Work::Text("#000000".into()), &delivery(pick_job(), &copy, &run_dir, &dir));
    assert_eq!(finish, Finish::CaptureFailed);
    assert_eq!(std::fs::read_dir(&run_dir).unwrap().count(), 0, "no pick.txt left");
}

#[test]
fn a_custom_copy_command_is_run_as_written() {
    let dir = TempPath::dir("deliver-custom");
    let fake = FakeExecutor::new();
    fake.ok("xclip -selection clipboard -t image/png", "");
    let copy: Vec<String> = ["xclip", "-selection", "clipboard", "-t", "image/png"].map(String::from).to_vec();
    let finish = deliver(&fake, PNG, &delivery(copy_job(Scope::Screen), &copy, &dir.join("run"), &dir.join("shots")));
    assert_eq!(finish, Finish::Copied(Scope::Screen));
    assert_eq!(fake.calls(), ["xclip -selection clipboard -t image/png"], "nothing appended to a command the user wrote");
    // `["wl-copy", "--primary"]` is custom too: only the bare default gets the type.
    let primary = vec!["wl-copy".to_string(), "--primary".to_string()];
    assert_eq!(copy_argv(&primary), ["wl-copy", "--primary"]);
    assert_eq!(copy_argv(&wl_copy()), ["wl-copy", "--type", "image/png"]);
}

#[test]
fn a_copy_that_cannot_start_is_a_copy_failure_and_no_file_stays() {
    let dir = TempPath::dir("deliver-nocopy");
    let run = dir.join("run");
    let fake = FakeExecutor::new(); // wl-copy is not there: the fake answers NotFound
    let copy = wl_copy();
    assert_eq!(deliver(&fake, PNG, &delivery(copy_job(Scope::Screen), &copy, &run, &dir)), Finish::CopyFailed);
    assert_eq!(std::fs::read_dir(&run).unwrap().count(), 0, "the private file is removed on failure too");
    // `copy = []` is no copy command at all: a failure, and nothing is run.
    let none: Vec<String> = Vec::new();
    assert_eq!(deliver(&fake, PNG, &delivery(copy_job(Scope::Screen), &none, &run, &dir)), Finish::CopyFailed);
    assert_eq!(fake.calls(), ["wl-copy --type image/png"], "only the first try ran anything");
}

#[test]
fn area_save_writes_the_dated_file_and_names_it_without_its_folder() {
    let dir = TempPath::dir("deliver-save");
    let shots = dir.join("pictures/screenshots");
    let fake = FakeExecutor::new();
    let copy = wl_copy();
    let finish = deliver(&fake, PNG, &delivery(save_job(), &copy, &dir.join("run"), &shots));
    assert_eq!(finish, Finish::Saved("20261104-090507.png".into()), "the name only");
    assert_eq!(std::fs::read(shots.join("20261104-090507.png")).unwrap(), PNG);
    assert!(fake.calls().is_empty(), "a save never touches the clipboard");
    // The same second again: a numbered file beside the first, never over it.
    let again = deliver(&fake, PNG, &delivery(save_job(), &copy, &dir.join("run"), &shots));
    assert_eq!(again, Finish::Saved("20261104-090507-2.png".into()));
    assert!(shots.join("20261104-090507.png").exists() && shots.join("20261104-090507-2.png").exists());
}

#[test]
fn a_folder_that_cannot_be_made_is_a_save_failure() {
    let dir = TempPath::dir("deliver-savefail");
    std::fs::write(dir.join("a-file"), "x").unwrap();
    let fake = FakeExecutor::new();
    let copy = wl_copy();
    let finish = deliver(&fake, PNG, &delivery(save_job(), &copy, &dir, &dir.join("a-file/shots")));
    assert_eq!(finish, Finish::SaveFailed);
}

/// A daemon killed between writing the private file and removing it leaves `capture.png` (or the
/// writer's `.capture.png.<pid>`) behind: the start clears those, and only those.
#[test]
fn stale_private_files_are_removed_at_start_and_nothing_else() {
    let dir = TempPath::dir("deliver-stale");
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::set_permissions(&run, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    for name in ["capture.png", ".capture.png.123", "pick.txt", ".pick.txt.77", "mode", "capture.png.keep", "pick.txt.keep", "other.png"] {
        std::fs::write(run.join(name), "x").unwrap();
    }
    remove_stale(&run);
    let mut left: Vec<String> = std::fs::read_dir(&run).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["capture.png.keep", "mode", "other.png", "pick.txt.keep"]);
    remove_stale(&dir.join("absent")); // no folder: nothing to do, no panic
}

/// No folder to save in (no home to anchor the default): a save failure, never a relative path.
#[test]
fn no_folder_is_a_save_failure() {
    let fake = FakeExecutor::new();
    let copy = wl_copy();
    let dir = TempPath::dir("deliver-nodir");
    let d = Delivery { save_dir: None, ..delivery(save_job(), &copy, &dir, &dir) };
    assert_eq!(deliver(&fake, PNG, &d), Finish::SaveFailed);
}

/// A 4 × 2 frame of XRGB pixels, pixel (x, y) being (10x, 20y, 30) as R, G, B.
fn frame() -> Frame {
    let mut data = Vec::new();
    for y in 0..2u8 {
        for x in 0..4u8 {
            data.extend_from_slice(&[30, 20 * y, 10 * x, 0]);
        }
    }
    Frame { width: 4, height: 2, stride: 16, format: Format::Xrgb8888, y_invert: false, data }
}

fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

#[test]
fn a_frame_is_cut_to_its_area_and_encoded() {
    let png = render(&frame(), Some(Rect { x: 1, y: 1, width: 2, height: 1 })).unwrap();
    assert_eq!(decode(&png), (2, 1, vec![10, 20, 30, 255, 20, 20, 30, 255]));
    // An area partly past the frame is cut to what is inside, and the PNG has that size.
    let cut = render(&frame(), Some(Rect { x: 3, y: 1, width: 5, height: 5 })).unwrap();
    assert_eq!(decode(&cut), (1, 1, vec![30, 20, 30, 255]));
    let whole = render(&frame(), None).unwrap();
    let (w, h, _) = decode(&whole);
    assert_eq!((w, h), (4, 2), "no area: the whole frame");
}

#[test]
fn an_area_outside_the_frame_is_a_capture_failure_and_nothing_is_copied() {
    let dir = TempPath::dir("deliver-offscreen");
    let fake = FakeExecutor::new();
    fake.ok("wl-copy --type image/png", "");
    let copy = wl_copy();
    let off = Some(Rect { x: 50, y: 50, width: 10, height: 10 });
    let finish = run(&fake, Work::Image { frame: frame(), area: off }, &delivery(copy_job(Scope::Area), &copy, &dir.join("run"), &dir));
    assert_eq!(finish, Finish::CaptureFailed);
    assert!(fake.calls().is_empty(), "a failed capture never reaches the clipboard");
    assert!(notice(&finish).unwrap().critical);
}

#[test]
fn a_whole_run_copies_a_png_of_the_area() {
    let dir = TempPath::dir("deliver-run");
    let fake = FakeExecutor::new();
    fake.ok("wl-copy --type image/png", "");
    let copy = wl_copy();
    let area = Some(Rect { x: 0, y: 0, width: 2, height: 2 });
    let finish = run(&fake, Work::Image { frame: frame(), area }, &delivery(copy_job(Scope::Area), &copy, &dir.join("run"), &dir));
    assert_eq!(finish, Finish::Copied(Scope::Area));
    let inputs = fake.inputs();
    assert_eq!(decode(&inputs[0].1).0, 2, "what the clipboard got is the cut area");
}

fn pick_job() -> Job {
    Job { scope: Scope::Pick, goal: Goal::Copy }
}

/// The picked value reaches the copy command as text on its standard input, the command as written
/// (the default `wl-copy` is not told `image/png`), and "copied" is its exit 0; the notice gives
/// the value, and the private file is gone.
#[test]
fn a_picked_value_reaches_the_copy_command_as_text() {
    let dir = TempPath::dir("deliver-pick");
    let run_dir = dir.join("run");
    let fake = FakeExecutor::new();
    fake.ok("wl-copy", "");
    let copy = wl_copy();
    let finish = run(&fake, Work::Text("#1A2BFC".into()), &delivery(pick_job(), &copy, &run_dir, &dir.join("shots")));
    assert_eq!(finish, Finish::Picked("#1A2BFC".into()));
    assert_eq!(fake.inputs(), [("wl-copy".to_string(), b"#1A2BFC".to_vec())], "the text alone, no type, no newline");
    assert_eq!(std::fs::read_dir(&run_dir).unwrap().count(), 0, "the private file is gone");
    assert!(!dir.join("shots").exists(), "a pick saves nothing");
}

/// A copy command that fails, exits badly or is missing: a copy failure, critical, and the clipboard
/// was never said to hold the value.
#[test]
fn a_picked_value_the_copy_command_refuses_is_a_copy_failure() {
    let dir = TempPath::dir("deliver-pick-bad");
    let run_dir = dir.join("run");
    let copy = wl_copy();
    for (what, answer) in [
        ("non-zero", Some(Ok(alpheus_common::exec::Output { code: Some(1), stdout: String::new(), stderr: String::new() }))),
        ("timeout", Some(Err(alpheus_common::exec::ExecError::Timeout))),
        ("missing", None),
    ] {
        let fake = FakeExecutor::new();
        if let Some(answer) = answer {
            fake.respond("wl-copy", answer);
        }
        let finish = run(&fake, Work::Text("#000000".into()), &delivery(pick_job(), &copy, &run_dir, &dir));
        assert_eq!(finish, Finish::CopyFailed, "{what}");
        assert!(!finish.touches_the_clipboard() && notice(&finish).unwrap().critical, "{what}");
        assert_eq!(std::fs::read_dir(&run_dir).unwrap().count(), 0, "{what}: the file is removed");
    }
    // No copy command at all.
    let fake = FakeExecutor::new();
    assert_eq!(run(&fake, Work::Text("#000000".into()), &delivery(pick_job(), &[], &run_dir, &dir)), Finish::CopyFailed);
    assert!(fake.calls().is_empty());
}
