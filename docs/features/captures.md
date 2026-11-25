# Captures

Screenshots of the screen or of an area, copied to the clipboard or saved, a colour picker, and an
area handed to an annotation tool or a text recognizer, or the screen recorded, from a key or the
Tools menu. This page says what exists today, how to use it, how to configure it, how it is built,
which rules the tests enforce on it and what it costs.

## What it does

- `alpheus-launch --capture full` copies a screenshot of the focused output.
- `--capture area` lets you drag an area on the focused output and copies it.
- `--capture area-save` does the same and saves it as `%Y%m%d-%H%M%S.png` in the pictures folder.
- `--capture pick` freezes the focused output, lets you point at a pixel with a zoom lens, and copies
  its colour as text (`#RRGGBB` by default).
- `--capture annotate` lets you drag an area and opens it in the annotation tool (satty by default);
  the image you save there is copied.
- `--capture ocr` lets you drag an area and copies the text the recognizer (tesseract by default)
  reads in it.
- `--capture record` records the focused output, `--capture record-area` a dragged area, with the
  recorder (wf-recorder by default) into `screenrecord-%Y%m%d-%H%M%S.mp4` in the videos folder;
  `--capture stop` ends the recording.

A notice says what happened: `Screen copied`, `Area copied`, `Saved: <file name>` (the name only),
`Copied: <value>` for the picker (its summary `Colour picker`), `Annotated image copied`, `Text copied`
(its summary `Text from area`; the text itself is never in the notice), `Recording saved: <file name>`
(its summary `Screen recording`), or, in the critical colour, `Capture failed`, `Not copied: …`,
`Not saved: …`, `Annotation failed`, `Text recognition failed` or `Recording failed`. A cancel (Esc, a
right click, a click without a drag, focus taken elsewhere, an annotation tool closed without saving,
a recognizer that read nothing) is silent; a selector that could not open, or that the compositor
took away, is a `Capture failed`. Nothing is resident between captures, nor once a recording ended.

## Using it

**Verbs.** Bind the verbs to keys in your compositor, or choose them in the Tools menu's Captures
category (`Screenshot`, `Area`, `Area (save)`, `Annotate`, `Record screen`, `Record area`, `Stop
recording`, `Colour picker`, `Text from area (OCR)`; the category is hidden when the compositor has no
screencopy, a tool's row when its `[captures]` program is `[]` or not on the `$PATH`, the two Record
rows while a recording runs, and Stop while none does). One capture runs at a time, an annotation
tool's or a recognizer's run included: a verb sent while one runs is ignored. A recording runs beside
the captures: screenshots may be taken while it records, but only one recording runs at a time.

**The area selector.** A quarter-dark layer over the focused output, a 2 px border in the accent
colour and a `W×H` label.

| Input | Does |
| --- | --- |
| drag, then release | selects and confirms |
| Enter | confirms the rectangle being dragged |
| Esc, a right click, a click without a drag | cancels (silent) |
| Space held while moving | moves the selection |
| arrows | nudge by 1 px; with Shift by 10 px; with Ctrl they resize |

**The colour picker.** The focused output is captured once, then shown frozen on an Overlay layer
over it (moving content stands still). Beside the pointer go a lens of 9 × 9 pixels magnified 10
times, the pointer's pixel outlined in white or black, a swatch of its colour and a card with its
`#RRGGBB`. The pointer is a crosshair. The lens reads the frame's own pixels, through the output's
scale (a fractional one included) and rotation; past an edge, the edge's pixels repeat.

| Input | Does |
| --- | --- |
| left click, or Enter | picks the pixel under the pointer and copies its value |
| Esc, a right click | cancels (silent: nothing copied, no notice) |

Focus taken elsewhere, or the launcher or the pad opened over it, cancels too; the compositor taking
the layer away (its output gone) is a `Capture failed`.

**Annotation and text recognition.** After the drag, the area goes to the tool as a private PNG. For
an annotation, alpheus waits for the editor to close: if you saved (or copied, which the default flags
turn into a save), the image is copied and the notice says so; closed without saving is a silent
cancel. For text recognition, the text the tool prints is trimmed and copied as text; nothing printed
is a silent cancel. A tool that exits with another code than 0 is a critical failure. Either way the
clipboard is touched only by a result.

**Recording.** `record` starts at once on the focused output, `record-area` after the drag. The
privacy module's screen glyph shows `alpheus` while it records. `--capture stop`, or `Stop recording`
in the Tools menu, ends it: the recorder finishes its file and the notice names it. A recording still
running when the daemon stops is ended the same way (the daemon waits up to 2 s for the recorder;
one still running then is let go, never killed, and finishes its file on its own). That ending is
logged only, with no notice: the notification server is alpheus's own and stops with it. Nothing is
resumed after a restart.

**The result.** A selection is clamped to the output; one with no area is a cancel. "The screen" is
the output the selector would cover: the focused one, else the first the compositor listed. A
capture holds one output, at its own scale and rotation.

## Configuration

The key `[captures] dir` in `bar.toml` sets the folder of `area-save` (default
`$XDG_PICTURES_DIR/screenshots`, else the pictures entry of `user-dirs.dirs`, else
`~/Pictures/screenshots`; with no home to anchor it, a save fails rather than land in a relative
path). `pick_format` is the picker's text: `{hex}` (`#RRGGBB`, upper case) by default, with `{r}`,
`{g}`, `{b}` (0 to 255) and `{hex}` to fill, for instance `"rgb({r}, {g}, {b})"`; at least one
placeholder, no other `{…}` (so no literal `{`…`}` pair either), one line of at most 256 bytes.

The three tools are commands (no shell), `[]` to turn one off (its verbs then exit 2 and its rows
hide). `annotate` holds `{file}` (the area's private PNG) and `{out}` (the private PNG it writes
back) once each; `ocr` holds `{file}` once and prints the text on its standard output; `record` holds
`{out}` (the video file) once and `{geometry}` (`x,y wxh` in the compositor's logical pixels: the
area, or the whole focused output for `record`) at most once. Each placeholder is a whole argument,
never inside a longer one and never the program. The defaults:

- `annotate = ["satty", "--filename", "{file}", "--output-filename", "{out}", "--early-exit",
  "--copy-command", "cat >/dev/null", "--save-after-copy", "--disable-notifications"]`: satty 0.20
  exits at the first copy or save; its own copy writes the image to `sh -c "cat >/dev/null"`, which
  reads it to the end and copies nothing, and only then saves to `{out}`. So a copy or a save in
  satty brings the image back to alpheus, which copies it; closing satty with neither is a silent
  cancel; the notice is alpheus's. A replacement copy command must read its whole standard input:
  one that does not (`true`) breaks satty's pipe for any image past 64 KiB, satty then saves nothing
  and the annotation is lost as a cancel;
- `ocr = ["tesseract", "{file}", "-", "-l", "eng"]`: `-` prints the text;
- `record = ["wf-recorder", "-g", "{geometry}", "-f", "{out}"]`: the recording goes to a free
  `screenrecord-%Y%m%d-%H%M%S.mp4` in `$XDG_VIDEOS_DIR` (else the videos entry of `user-dirs.dirs`,
  else `~/Videos`), never over a file. A recorder must finish its file on SIGINT and exit 0.

See [configuration.md](../configuration.md#captures).

The copy goes through `launcher.toml`'s `copy`, read at each capture. The default `["wl-copy"]` is
given `--type image/png`; a custom command is run as written and must accept a PNG on its standard
input. `copy = []` means no copy command: a copy then fails. The picker's value goes to the same
command as text, run as written (the default `wl-copy` gets no `--type`), and counts as copied only
when it exits 0. The annotated image goes to `copy` as a screenshot does (a PNG), the recognized text
as the picker's value does (text, as written).

On a compositor that fades layer surfaces, see
[compositors.md](../compositors.md#captures-and-layer-animations).

## How it is built

- `captures/mod.rs`: the pure flow. `plan` says what a verb asks for, `Flow` is one capture at a
  time with a generation number, `Finish` and `notice` say how it ended.
- `captures/app.rs`: the wiring. The steps of the flow are carried out here: open the selector,
  wait for the compositor (a `wl_display.sync`), ask one frame, hand it to the worker, show the
  notice; start the external tool or the recorder and watch their pidfds (`capture_child_ready`), stop
  the recorder, light the privacy glyph. The frame and the worker's ending come back through the
  loop's mail (`Mail::Capture`), a child's end through its pidfd.
- `captures/screencopy.rs`: the only file that speaks `zwlr_screencopy_manager_v1`. One frame in
  its own short-lived shm pool, dropped at the answer; a `PendingFrame` can be cancelled.
- `captures/select.rs` and `select_app.rs`: the selector's model and its Overlay layer surface.
- `captures/deliver.rs`: the worker's job on the short-lived `alpheus-capture` thread: cut the area,
  encode the PNG (`encode.rs`, the `png` crate), copy or save it. The copy writes the PNG to a
  private file (`$XDG_RUNTIME_DIR/alpheus/capture.png`, owner only) and runs the copy command with
  it on its standard input through `Executor::run_with_stdin_file`, waiting at most 5 s for its exit.
- `captures/paths.rs`: the folders, the dated names and the recorder's geometry.
- `captures/tools.rs`: the external annotation and recognition tools. `start` makes the tool's
  private output file and starts the tool with `{file}` and `{out}` filled; `judge` reads what it left
  once it ended. The tool is a watched child of the daemon (`Executor::spawn_watched`: its own process
  group, SIGINT if the daemon dies), whose pidfd is on the event loop: it turns readable only when the
  tool ends, so nothing waits or polls while the editor is open, however long.
- `captures/record.rs`: the recorder, a watched child too. `Recording::start` picks the free name and
  starts it, `stop` sends SIGINT to its own process group (nothing else), `finish` judges its end by
  the file, `end_at_shutdown` interrupts it and waits at most 2 s, then lets a recorder still running
  go (`Watched::release`) rather than kill it: a kill would leave the file unplayable. `LIVE` tells
  the signal guard a recording runs, so the forced exit comes 2 s later.
- `captures/pick.rs` and `pick_app.rs`: the picker's model (the `Shot` that reads the frozen frame
  through the output's scale and rotation, the lens, the value's text, the gestures) and its Overlay
  layer surface. The surface owns the frame: it goes with the surface at close. The lens, its
  frame and the swatch are plain rectangles (`fill_rect`); only the `#RRGGBB` card goes through the
  painter (the launcher's font and card colours). A draw repaints from
  the frame only what the previous overlay of that buffer covered, so a motion costs the lens's
  pixels, not the output's; it damages that area, the overlay on screen (the other buffer's, when
  two alternate) and the new one (`the_picker_leaves_no_old_lens_on_the_screen`). A redraw is skipped
  only while the frame's pixel under the pointer stays the same, so the lens always shows what a
  click copies.

The path of an annotation: the verb → the selector → `Settle` → the frame → the worker writes the
area's PNG to `capture-<n>.png` (`Prepared`) → the tool starts, its pidfd watched → the user saves
and closes → the pidfd turns readable → `judge` → the worker copies `capture-<n>-out.png` and removes
it → the notice. A recognition is the same, its text read from `capture-<n>.txt` (at most 256 KiB),
copied through `ocr.txt`. The path of a recording: the verb (and the selector for an area) →
`Settle` → the recorder starts, its pidfd watched, the privacy glyph lit → `stop` → SIGINT → the
recorder finishes its file and exits → the pidfd turns readable → `finish` → the glyph goes, the
notice.

The path of one area capture: the verb → `Flow::begin` → the selector opens → its answer →
`Settle` (the sync, after the selector's surface is destroyed) → the frame is asked, with a 5 s
deadline → the frame arrives → the worker encodes and copies → its ending arrives → the notice.
A whole screen goes through the same `Settle`, since a menu may be closing. A pick is a whole screen
whose frame opens the picker instead of going to the worker: the verb → `Settle` → the frame → the
picker opens over it → a click → the picker closes, its frame with it → the worker copies the value
(`pick.txt` in the private folder, on the copy command's standard input) → the notice.

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
- The picker: the lens is 9 × 9 around the pointer, clamped at the edges, and follows the scale and
  the rotation: `the_lens_shows_nine_by_nine_around_the_pointer`,
  `the_pixel_under_the_pointer_follows_the_scale_and_the_rotation`,
  `one_pixel_maps_as_map_area_maps_it_under_every_transform`; the value:
  `the_value_is_uppercase_hex_by_default`, `pick_format_fills_r_g_b_hex`, `pick_format_is_checked`;
  a cancel copies nothing and says nothing: `esc_picks_nothing_and_copies_nothing`,
  `a_cancelled_pick_is_silent_and_copies_nothing` (headless); a picker taken away or unable to open
  is critical: `a_picker_that_fails_ends_the_pick_as_a_critical_failure`,
  `only_the_users_gestures_are_a_silent_cancel` (`pick_app/tests.rs`),
  `a_picker_that_cannot_open_fails_its_capture`; the value is copied as text and only on exit 0:
  `a_picked_value_reaches_the_copy_command_as_text`,
  `a_picked_value_the_copy_command_refuses_is_a_copy_failure`; the frame lives in the open picker
  only: `the_picker_frame_is_kept_only_while_the_picker_is_open`; end to end on the test sway's known
  background: `colour_picker_copies_the_pixel_under_the_pointer` (headless).
- Without screencopy the verbs exit 2 and the Tools rows are hidden: `no_screencopy_hides_the_captures`,
  `the_tools_menu_hides_the_captures_without_screencopy`; a tool's row shows only when its program
  is set and found, the Record rows only while nothing records and Stop only while a recording runs,
  read from the daemon's own state: `the_capture_rows_follow_their_tool_and_stop_shows_only_while_recording`,
  `a_recording_lights_the_privacy_glyph_while_it_runs_and_its_watch_goes_with_it`; a tool set to `[]`
  refuses its verbs: `each_verb_is_planned_and_a_tool_set_to_nothing_refuses_its_verb`,
  `a_capture_tool_set_to_nothing_and_a_stop_with_nothing_recording_exit_2` (headless).
- The external tools: `{file}` and `{out}` are whole arguments
  (`the_tool_placeholders_are_whole_arguments_where_each_tool_needs_them`,
  `placeholders_are_filled_as_whole_arguments_only`); the annotated image or the recognized text is
  copied only after an exit 0 with a result, an exit 0 with nothing is a silent cancel, a failure is
  critical and never copies (`annotate_passes_whole_file_and_out_and_copies_the_result`,
  `ocr_copies_stdout_text_and_an_empty_output_is_a_cancel`, `a_tool_failure_is_critical_and_never_copies`,
  `an_annotation_runs_the_tool_then_copies_what_it_wrote`,
  `a_tool_closed_without_output_is_silent_and_a_failed_one_is_critical`,
  `a_recognition_copies_the_text_and_its_notice_never_holds_it`; headless:
  `annotate_copies_what_the_tool_wrote`, `ocr_copies_the_text_the_tool_printed`); the private files are
  owner-only and go on every path (`temp_files_are_0600_and_removed`; the annotated image too when the
  worker panics or never starts: `a_panicking_executor_ends_the_capture_as_a_failure_and_leaves_no_file`,
  `an_annotated_image_the_worker_never_runs_is_removed`); a tool runs as a watched child
  (`a_watched_program_signals_its_end_on_its_descriptor_and_gives_its_code` and its neighbours in
  `alpheus-common/src/exec/tests.rs`).
- The recording: the geometry and the free file are whole arguments, the stop reaches the recorder only
  (`record_spawns_with_the_geometry_and_stop_sends_sigint_to_it_only`, `a_recording_never_writes_over_a_file`,
  `interrupt_reaches_the_watched_program_only`, `an_interrupt_is_never_sent_to_a_reaped_pid`,
  `the_recording_signal_reaches_only_the_recorder_alpheus_started`); its end is judged by the file
  (`a_recording_that_fails_or_leaves_nothing_is_a_failure`); it leaves the capture slot free
  (`a_recording_starts_once_settled_and_leaves_the_slot_free`); it lights the privacy glyph
  (`recording_lights_the_privacy_screen_glyph`, headless
  `recording_lights_the_privacy_screen_glyph_on_the_bar`), and a panic while its end is read ends
  the recording rather than leave the glyph lit
  (`a_panic_reading_the_recorders_end_ends_the_recording`); the daemon's stop ends it with SIGINT,
  waits at most 2 s and never kills it (`the_daemon_stop_ends_the_recording_with_sigint`,
  `a_released_watched_program_is_not_killed`,
  `the_shutdown_grace_covers_the_recorders_stop_while_recording`,
  `the_shutdown_ends_the_recording_before_anything_goes`, headless
  `a_recording_running_at_the_daemon_stop_is_finished`); end to end: `record_then_stop_notifies_saved`.
- The logs are fixed words, never a colour or a position (`the_capture_logs_fixed_words_only`,
  `the_picker_logs_fixed_words_only`, `the_picker_model_is_pure_and_cannot_panic`), the PNG is
  encoded on the capture thread only and copied through a file (`the_png_is_encoded_on_the_capture_thread_and_copied_through_a_file`),
  and one file speaks the screencopy protocol (`screencopy_only_in_screencopy_rs`).

See [decisions.md](../decisions.md): "A capture never touches the clipboard unless it succeeded",
"The colour picker picks from a frozen frame and copies text through `copy`, as written",
"A capture waits for the selector's removal", "A screenshot reaches the copy command as a file on its
standard input", "A frame that never comes fails the capture", "Screenshots are encoded by the `png`
crate, on a short-lived thread, and nothing of a capture is kept", "Only screencopy.rs names the capture
protocol", "An external capture tool runs as a watched child of the daemon", "Recording is the
external tool's, driven and stopped by alpheus".

## What it costs

Nothing at rest: no thread, no timer, no surface, no buffer. A capture lives for a few hundred
milliseconds: the selector's shm pool (≈ 3.7 MB while it is open), one frame (3.6 MB at 1280 × 720),
the PNG and the worker thread, all given back (`release_memory`). Ten more captures grew the daemon's
anonymous memory by 8 to 16 KB (limit 64 KB), with 0 wake-ups at rest afterwards
(`at_rest_no_wakeups_after_a_screen_capture`). The `png` crate adds ≈ 78 KB to the stripped binary.
The colour picker, while open, holds the frame (3.6 MB at 1280 × 720) and its surface's pool
(≈ 3.7 MB): +7.6 MB of PSS, all given back at close; ten more picks moved the daemon's anonymous
memory by −12 to +12 KB (limit 64 KB), with 0 wake-ups at rest afterwards
(`at_rest_no_wakeups_after_a_colour_pick`). An external tool or the recorder costs one pidfd on the
event loop while it runs, quiet until it exits: no thread, no timer. While the recorder ran the
daemon's PSS was +227 to +231 KB and it woke 2 times in 5 s (bound 4); ten more recordings moved its
anonymous memory by 0 to +4 KB (limit 64 KB), with 0 wake-ups at rest afterwards
(`at_rest_no_wakeups_after_a_recording`). The tools, the recording and the privacy source add
≈ 36 KB to the stripped binary. See [measurements.md](../measurements.md).
