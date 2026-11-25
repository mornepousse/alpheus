//! Tests of the external tools' run (`tools.rs`) and of the worker's copy of what they gave
//! (`deliver.rs`): the whole `{file}` and `{out}`, the private files and their removal, and what an
//! exit means. The executor is always a `FakeExecutor`: no tool runs; the test plays the tool by
//! writing its output file and ending its fake child.

use super::*;
use crate::captures::deliver::{Delivery, Work, remove_stale, run};
use crate::captures::{Finish, Goal, Job, Scope};
use crate::config::CapturesConfig;
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::temp_path::TempPath;
use std::os::unix::fs::PermissionsExt;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nthe area";

/// The private folder of one test, owner only as the daemon's is.
fn private_dir(name: &str) -> (TempPath, PathBuf) {
    let root = TempPath::dir(name);
    let dir = root.join("alpheus");
    write_private(&dir, ".probe", b"").unwrap();
    let _ = remove_private(&dir, ".probe");
    (root, dir)
}

/// The worker's first run: the area's PNG written where the tool opens it.
fn prepare(dir: &Path, files: &ToolFiles, goal: Goal) {
    let copy = vec!["wl-copy".to_string()];
    let d = Delivery {
        job: Job { scope: Scope::Area, goal },
        copy: &copy,
        save_dir: None,
        file_name: String::new(),
        temp_dir: dir,
        tool_input: Some(&files.input),
    };
    assert_eq!(crate::captures::deliver::deliver(&FakeExecutor::new(), PNG, &d), Finish::Prepared);
}

/// The worker's second run, on `work`, with `copy` as `launcher.toml`'s copy.
fn copy_result(exec: &FakeExecutor, dir: &Path, copy: &[String], goal: Goal, work: Work) -> Finish {
    let d =
        Delivery { job: Job { scope: Scope::Area, goal }, copy, save_dir: None, file_name: String::new(), temp_dir: dir, tool_input: None };
    run(exec, work, &d)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// `{file}` and `{out}` are replaced only as whole arguments: a path with a space stays one argument,
/// and the placeholder inside a longer argument is left alone.
#[test]
fn placeholders_are_filled_as_whole_arguments_only() {
    let template: Vec<String> = ["tool", "{file}", "--out={out}", "{out}", "{file}x"].map(String::from).to_vec();
    let argv = fill(&template, &[(FILE, "/run/a b/capture-1.png"), (OUT, "/run/a b/capture-1-out.png")]);
    assert_eq!(argv, ["tool", "/run/a b/capture-1.png", "--out={out}", "/run/a b/capture-1-out.png", "{file}x"]);
}

/// The default annotation: satty gets the private image as `--filename` and the private output as
/// `--output-filename`, each a whole argument, with the flags that keep it off the clipboard; what it
/// wrote back is copied as a PNG through `copy`, and every file is gone afterwards.
#[test]
fn annotate_passes_whole_file_and_out_and_copies_the_result() {
    let (_root, dir) = private_dir("tools-annotate");
    let files = ToolFiles::new(&dir, 7, Tool::Annotate);
    prepare(&dir, &files, Goal::Annotate);
    let exec = FakeExecutor::new();
    exec.watch_ok("satty", None);
    let template = CapturesConfig::default().annotate;
    let mut child = start(&exec, Tool::Annotate, &template, &files).expect("started");
    let (input, output) = (files.input_path(), files.output_path());
    assert_eq!(
        exec.calls(),
        [format!(
            "satty --filename {} --output-filename {} --early-exit --copy-command cat >/dev/null --save-after-copy --disable-notifications",
            input.display(),
            output.display()
        )]
    );
    assert_eq!(exec.children()[0].stdout(), None, "an editor's output is the file it writes");
    // The user annotates and saves: the tool writes its image over the empty private file, exits 0.
    std::fs::write(&output, b"\x89PNG\r\n\x1a\nannotated").unwrap();
    exec.children()[0].end(Some(0));
    let exit = child.try_exit().expect("ended");
    assert_eq!(judge(Tool::Annotate, exit, &files), ToolEnd::Annotated);
    assert!(!input.exists(), "the image handed to it goes once it ended");
    assert!(output.exists(), "the result stays for the worker's copy");
    exec.ok("wl-copy --type image/png", "");
    let copy = vec!["wl-copy".to_string()];
    let finish = copy_result(&exec, &dir, &copy, Goal::Annotate, Work::Annotated(files.output.clone()));
    assert_eq!(finish, Finish::Annotated);
    assert_eq!(exec.inputs(), [("wl-copy --type image/png".to_string(), b"\x89PNG\r\n\x1a\nannotated".to_vec())], "the tool's image");
    assert_eq!(names_in(&dir), Vec::<String>::new(), "nothing is left");
}

/// The recognition's text comes from its standard output, a private file: trimmed, it is copied as
/// text through `copy` as written. An empty output is a silent cancel: nothing is copied.
#[test]
fn ocr_copies_stdout_text_and_an_empty_output_is_a_cancel() {
    let (_root, dir) = private_dir("tools-ocr");
    let template = CapturesConfig::default().ocr;
    let copy = vec!["wl-copy".to_string()];
    let files = ToolFiles::new(&dir, 3, Tool::Ocr);
    prepare(&dir, &files, Goal::Ocr);
    let exec = FakeExecutor::new();
    exec.watch_ok("tesseract", None);
    let mut child = start(&exec, Tool::Ocr, &template, &files).expect("started");
    assert_eq!(exec.calls(), [format!("tesseract {} - -l eng", files.input_path().display())]);
    assert_eq!(exec.children()[0].stdout(), Some(files.output_path()), "its standard output is the private text file");
    std::fs::write(files.output_path(), "\n  Hello, world\n\n").unwrap();
    exec.children()[0].end(Some(0));
    let end = judge(Tool::Ocr, child.try_exit().unwrap(), &files);
    assert_eq!(end, ToolEnd::Text("Hello, world".into()));
    assert_eq!(names_in(&dir), Vec::<String>::new(), "the image and the text file go once it is read");
    exec.ok("wl-copy", "");
    let finish = copy_result(&exec, &dir, &copy, Goal::Ocr, Work::Recognised("Hello, world".into()));
    assert_eq!(finish, Finish::TextCopied);
    assert_eq!(exec.inputs(), [("wl-copy".to_string(), b"Hello, world".to_vec())], "text, by the copy command as written");
    assert_eq!(names_in(&dir), Vec::<String>::new());
    // An empty or blank output: a cancel, and no copy runs.
    let files = ToolFiles::new(&dir, 4, Tool::Ocr);
    prepare(&dir, &files, Goal::Ocr);
    let exec = FakeExecutor::new();
    exec.watch_ok("tesseract", None);
    let mut child = start(&exec, Tool::Ocr, &template, &files).unwrap();
    std::fs::write(files.output_path(), " \n\t\n").unwrap();
    exec.children()[0].end(Some(0));
    assert_eq!(judge(Tool::Ocr, child.try_exit().unwrap(), &files), ToolEnd::Cancelled);
    assert_eq!(names_in(&dir), Vec::<String>::new());
    assert!(exec.inputs().is_empty(), "nothing copied");
    // An annotation closed without saving leaves its output empty: a cancel too.
    let files = ToolFiles::new(&dir, 5, Tool::Annotate);
    prepare(&dir, &files, Goal::Annotate);
    let exec = FakeExecutor::new();
    exec.watch_ok("satty", None);
    let _child = start(&exec, Tool::Annotate, &CapturesConfig::default().annotate, &files).unwrap();
    assert_eq!(judge(Tool::Annotate, Some(0), &files), ToolEnd::Cancelled);
    assert_eq!(names_in(&dir), Vec::<String>::new());
}

/// A tool that exits with another code, is killed, cannot start, or prints too much is a failure:
/// nothing reaches the copy command and every file goes.
#[test]
fn a_tool_failure_is_critical_and_never_copies() {
    let (_root, dir) = private_dir("tools-failure");
    for (tool, goal, prog, template) in [
        (Tool::Annotate, Goal::Annotate, "satty", CapturesConfig::default().annotate),
        (Tool::Ocr, Goal::Ocr, "tesseract", CapturesConfig::default().ocr),
    ] {
        for (n, exit) in [(1, Some(1)), (2, Some(2)), (3, None)] {
            let files = ToolFiles::new(&dir, n, tool);
            prepare(&dir, &files, goal);
            let exec = FakeExecutor::new();
            exec.watch_ok(prog, None);
            let _child = start(&exec, tool, &template, &files).unwrap();
            // Even with an output written: the exit decides.
            std::fs::write(files.output_path(), "something").unwrap();
            assert_eq!(judge(tool, exit, &files), ToolEnd::Failed, "{tool:?} {exit:?}");
            assert_eq!(names_in(&dir), Vec::<String>::new(), "{tool:?} {exit:?}");
            assert!(exec.inputs().is_empty(), "{tool:?} {exit:?}: never copied");
        }
        // A program that cannot start: no child, and the files are gone.
        let files = ToolFiles::new(&dir, 9, tool);
        prepare(&dir, &files, goal);
        let exec = FakeExecutor::new();
        assert!(start(&exec, tool, &template, &files).is_none(), "{tool:?}: not found");
        assert_eq!(names_in(&dir), Vec::<String>::new());
        let empty: Vec<String> = Vec::new();
        assert!(start(&exec, tool, &empty, &files).is_none(), "{tool:?}: no program");
        assert_eq!(names_in(&dir), Vec::<String>::new());
    }
    // A recognition that printed more than MAX_TEXT is a failure, never a cut text copied.
    let files = ToolFiles::new(&dir, 11, Tool::Ocr);
    std::fs::write(files.output_path(), vec![b'a'; MAX_TEXT as usize + 1]).unwrap();
    assert_eq!(judge(Tool::Ocr, Some(0), &files), ToolEnd::Failed);
    std::fs::write(files.output_path(), vec![b'a'; MAX_TEXT as usize]).unwrap();
    assert!(matches!(judge(Tool::Ocr, Some(0), &files), ToolEnd::Text(t) if t.len() == MAX_TEXT as usize), "at the cap is fine");
}

/// The image handed to a tool and the file it writes back are owner-only, whatever the tool's umask
/// (the output is made by alpheus before the tool starts); a run given up removes both, and a start
/// clears what a killed daemon left, and nothing else.
#[test]
fn temp_files_are_0600_and_removed() {
    let (_root, dir) = private_dir("tools-modes");
    for (tool, goal, prog, template) in [
        (Tool::Annotate, Goal::Annotate, "satty", CapturesConfig::default().annotate),
        (Tool::Ocr, Goal::Ocr, "tesseract", CapturesConfig::default().ocr),
    ] {
        let files = ToolFiles::new(&dir, 1, tool);
        prepare(&dir, &files, goal);
        let exec = FakeExecutor::new();
        exec.watch_ok(prog, None);
        let child = start(&exec, tool, &template, &files).unwrap();
        assert_eq!(mode(&files.input_path()), 0o600, "{tool:?}: the image");
        assert_eq!(mode(&files.output_path()), 0o600, "{tool:?}: the output");
        assert_eq!(std::fs::metadata(files.output_path()).unwrap().len(), 0, "{tool:?}: empty until the tool writes");
        assert_eq!(mode(&dir), 0o700);
        // Given up (the daemon stops while the tool runs): the child is dropped, the files removed.
        drop(child);
        files.remove();
        assert!(exec.children()[0].killed(), "{tool:?}: the tool goes with the run");
        assert_eq!(names_in(&dir), Vec::<String>::new(), "{tool:?}");
    }
    // What a daemon killed during a run left, the writer's temporary files included, goes at the
    // next start; a file of another name stays.
    for name in ["capture-12.png", "capture-12-out.png", "capture-12.txt", ".capture-12.png.4242", "ocr.txt", ".ocr.txt.77"] {
        std::fs::write(dir.join(name), "x").unwrap();
    }
    for name in ["capture-x.png", "capture-.png", "capture-12.mp4", "notes.txt", ".capture-12.png.pid", "capture-12-out.txt"] {
        std::fs::write(dir.join(name), "x").unwrap();
    }
    remove_stale(&dir);
    assert_eq!(
        names_in(&dir),
        [".capture-12.png.pid", "capture-.png", "capture-12-out.txt", "capture-12.mp4", "capture-x.png", "notes.txt"]
    );
}

#[test]
fn only_a_tool_runs_files_are_tool_files() {
    for name in ["capture-1.png", "capture-12-out.png", "capture-3.txt", ".capture-1.png.99", ".capture-2-out.png.1"] {
        assert!(is_tool_file(name), "{name}");
    }
    for name in
        ["capture.png", "capture-1.mp4", "capture--out.png", "capture-a.txt", ".capture-1.png.", "xcapture-1.png", "capture-1.png.bak"]
    {
        assert!(!is_tool_file(name), "{name}");
    }
}
