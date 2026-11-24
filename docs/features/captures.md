# Captures

Screenshots of the screen or of an area, copied to the clipboard or saved, from a key or the Tools
menu. This page says what exists today, how to use it, how to configure it, how it is built, which
rules the tests enforce on it and what it costs. Annotation, text recognition, recording and the
colour picker are not built yet: their verbs answer "not available yet" and their Tools rows are
hidden.

## What it does

- `alpheus-launch --capture full` copies a screenshot of the focused output.
- `--capture area` lets you drag an area on the focused output and copies it.
- `--capture area-save` does the same and saves it as `%Y%m%d-%H%M%S.png` in the pictures folder.

A notice says what happened: `Screen copied`, `Area copied` or `Saved: <file name>` (the name only),
or, in the critical colour, `Capture failed`, `Not copied: …` or `Not saved: …`. A cancel (Esc, a
right click, a click without a drag, focus taken elsewhere) is silent; a selector that could not
open, or that the compositor took away, is a `Capture failed`. Nothing is resident between captures.

## Using it

**Verbs.** Bind the verbs to keys in your compositor, or choose them in the Tools menu's Captures
category (`Screenshot`, `Area`, `Area (save)`; the category is hidden when the compositor has no
screencopy). One capture runs at a time: a verb sent while one runs is ignored.

**The area selector.** A quarter-dark layer over the focused output, a 2 px border in the accent
colour and a `W×H` label.

| Input | Does |
| --- | --- |
| drag, then release | selects and confirms |
| Enter | confirms the rectangle being dragged |
| Esc, a right click, a click without a drag | cancels (silent) |
| Space held while moving | moves the selection |
| arrows | nudge by 1 px; with Shift by 10 px; with Ctrl they resize |

**The result.** A selection is clamped to the output; one with no area is a cancel. "The screen" is
the output the selector would cover: the focused one, else the first the compositor listed. A
capture holds one output, at its own scale and rotation.

## Configuration

The key `[captures] dir` in `bar.toml` sets the folder of `area-save` (default
`$XDG_PICTURES_DIR/screenshots`, else the pictures entry of `user-dirs.dirs`, else
`~/Pictures/screenshots`; with no home to anchor it, a save fails rather than land in a relative
path). The other keys (`annotate`, `ocr`, `record`, `pick_format`) are read and checked but used by
later deliveries. See [configuration.md](../configuration.md#captures).

The copy goes through `launcher.toml`'s `copy`, read at each capture. The default `["wl-copy"]` is
given `--type image/png`; a custom command is run as written and must accept a PNG on its standard
input. `copy = []` means no copy command: a copy then fails.

On a compositor that fades layer surfaces, see
[compositors.md](../compositors.md#captures-and-layer-animations).

## How it is built

- `captures/mod.rs`: the pure flow. `plan` says what a verb asks for, `Flow` is one capture at a
  time with a generation number, `Finish` and `notice` say how it ended.
- `captures/app.rs`: the wiring. The steps of the flow are carried out here: open the selector,
  wait for the compositor (a `wl_display.sync`), ask one frame, hand it to the worker, show the
  notice. The frame and the worker's ending come back through the loop's mail (`Mail::Capture`).
- `captures/screencopy.rs`: the only file that speaks `zwlr_screencopy_manager_v1`. One frame in
  its own short-lived shm pool, dropped at the answer; a `PendingFrame` can be cancelled.
- `captures/select.rs` and `select_app.rs`: the selector's model and its Overlay layer surface.
- `captures/deliver.rs`: the worker's job on the short-lived `alpheus-capture` thread: cut the area,
  encode the PNG (`encode.rs`, the `png` crate), copy or save it. The copy writes the PNG to a
  private file (`$XDG_RUNTIME_DIR/alpheus/capture.png`, owner only) and runs the copy command with
  it on its standard input through `Executor::run_with_stdin_file`, waiting at most 5 s for its exit.
- `captures/paths.rs`: the folder and the dated name.

The path of one area capture: the verb → `Flow::begin` → the selector opens → its answer →
`Settle` (the sync, after the selector's surface is destroyed) → the frame is asked, with a 5 s
deadline → the frame arrives → the worker encodes and copies → its ending arrives → the notice.
A whole screen goes through the same `Settle`, since a menu may be closing.

## The rules its tests enforce

- A cancel is silent and never copies; a failure is critical and never copies; the success notice
  follows a capture that happened: `a_cancel_is_silent_and_never_copies`,
  `the_notices_say_what_happened_and_only_a_success_is_a_calm_one` (`captures/tests.rs`),
  `an_area_outside_the_frame_is_a_capture_failure_and_nothing_is_copied`,
  `a_copy_that_cannot_start_is_a_copy_failure_and_no_file_stays` (`captures/deliver/tests.rs`),
  `a_cancelled_capture_is_silent_and_copies_nothing` (headless); a selector that fails is a failure,
  not a cancel: `a_selector_that_fails_ends_the_capture_as_a_critical_failure`,
  `only_the_users_gestures_are_a_silent_cancel`, `a_selector_that_cannot_open_fails_its_capture`;
  the debug selector never answers a capture:
  `an_area_capture_replaces_the_debug_selector_and_still_copies` (headless).
- "Copied" means the copy command exited 0: `a_copy_command_that_exits_badly_or_hangs_is_a_copy_failure`.
- One capture at a time, an answer is for its own capture, a frame that never comes fails:
  `one_capture_at_a_time`, `a_late_frame_from_an_old_generation_is_ignored`,
  `a_frame_that_never_comes_fails_the_capture`, `a_second_capture_while_one_runs_is_ignored` (headless,
  the debug `select` verb included); the deadline is armed only while a frame is awaited:
  `the_frame_deadline_is_armed_only_while_a_frame_is_awaited` (`tests/guardrails.rs`).
- The frame is asked only after the compositor dealt with the removal, for an area and for a whole
  screen: `a_selection_waits_for_the_compositor_before_the_frame_is_asked`,
  `every_scope_waits_for_the_compositor_before_its_frame`,
  `a_frame_is_asked_only_after_the_selectors_removal_is_settled` (`tests/guardrails.rs`),
  `an_area_capture_has_no_dim_tint` (headless).
- A panic on the worker ends the capture as a failure and leaves no file:
  `a_panicking_executor_ends_the_capture_as_a_failure_and_leaves_no_file`, and the worker always mails
  its ending: `the_capture_worker_always_mails_its_ending`; a start clears a stale file:
  `stale_private_files_are_removed_at_start_and_nothing_else`,
  `a_start_clears_the_capture_files_a_killed_daemon_left` (headless).
- Without screencopy the verbs exit 2 and the Tools rows are hidden: `no_screencopy_hides_the_captures`,
  `the_tools_menu_hides_the_captures_without_screencopy`; the rows of unbuilt verbs are hidden:
  `the_unbuilt_captures_are_hidden_until_their_tasks_land`.
- The logs are fixed words (`the_capture_logs_fixed_words_only`), the PNG is encoded on the capture
  thread only and copied through a file (`the_png_is_encoded_on_the_capture_thread_and_copied_through_a_file`),
  and one file speaks the screencopy protocol (`screencopy_only_in_screencopy_rs`).

See [decisions.md](../decisions.md): "A capture never touches the clipboard unless it succeeded",
"A capture waits for the selector's removal", "A screenshot reaches the copy command as a file on its
standard input", "A frame that never comes fails the capture", "Screenshots are encoded by the `png`
crate, on a short-lived thread, and nothing of a capture is kept".

## What it costs

Nothing at rest: no thread, no timer, no surface, no buffer. A capture lives for a few hundred
milliseconds: the selector's shm pool (≈ 3.7 MB while it is open), one frame (3.6 MB at 1280 × 720),
the PNG and the worker thread, all given back (`release_memory`). Ten more captures grew the daemon's
anonymous memory by 8 to 16 KB (limit 64 KB), with 0 wake-ups at rest afterwards
(`at_rest_no_wakeups_after_a_screen_capture`). The `png` crate adds ≈ 78 KB to the stripped binary.
See [measurements.md](../measurements.md).
