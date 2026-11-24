# Captures

Screenshots of the screen or of an area, copied to the clipboard or saved, and a colour picker, from
a key or the Tools menu. This page says what exists today, how to use it, how to configure it, how it
is built, which rules the tests enforce on it and what it costs. Annotation, text recognition and
recording are not built yet: their verbs answer "not available yet" and their Tools rows are hidden.

## What it does

- `alpheus-launch --capture full` copies a screenshot of the focused output.
- `--capture area` lets you drag an area on the focused output and copies it.
- `--capture area-save` does the same and saves it as `%Y%m%d-%H%M%S.png` in the pictures folder.
- `--capture pick` freezes the focused output, lets you point at a pixel with a zoom lens, and copies
  its colour as text (`#RRGGBB` by default).

A notice says what happened: `Screen copied`, `Area copied`, `Saved: <file name>` (the name only),
`Copied: <value>` for the picker (its summary `Colour picker`),
or, in the critical colour, `Capture failed`, `Not copied: …` or `Not saved: …`. A cancel (Esc, a
right click, a click without a drag, focus taken elsewhere) is silent; a selector that could not
open, or that the compositor took away, is a `Capture failed`. Nothing is resident between captures.

## Using it

**Verbs.** Bind the verbs to keys in your compositor, or choose them in the Tools menu's Captures
category (`Screenshot`, `Area`, `Area (save)`, `Colour picker`; the category is hidden when the
compositor has no screencopy). One capture runs at a time: a verb sent while one runs is ignored.

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

**The result.** A selection is clamped to the output; one with no area is a cancel. "The screen" is
the output the selector would cover: the focused one, else the first the compositor listed. A
capture holds one output, at its own scale and rotation.

## Configuration

The key `[captures] dir` in `bar.toml` sets the folder of `area-save` (default
`$XDG_PICTURES_DIR/screenshots`, else the pictures entry of `user-dirs.dirs`, else
`~/Pictures/screenshots`; with no home to anchor it, a save fails rather than land in a relative
path). `pick_format` is the picker's text: `{hex}` (`#RRGGBB`, upper case) by default, with `{r}`,
`{g}`, `{b}` (0 to 255) and `{hex}` to fill, for instance `"rgb({r}, {g}, {b})"`; at least one
placeholder, no other `{…}` (so no literal `{`…`}` pair either), one line of at most 256 bytes. The other keys (`annotate`, `ocr`,
`record`) are read and checked but used by
later deliveries. See [configuration.md](../configuration.md#captures).

The copy goes through `launcher.toml`'s `copy`, read at each capture. The default `["wl-copy"]` is
given `--type image/png`; a custom command is run as written and must accept a PNG on its standard
input. `copy = []` means no copy command: a copy then fails. The picker's value goes to the same
command as text, run as written (the default `wl-copy` gets no `--type`), and counts as copied only
when it exits 0.

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
  `the_tools_menu_hides_the_captures_without_screencopy`; the rows of unbuilt verbs are hidden:
  `the_unbuilt_captures_are_hidden_until_their_tasks_land`.
- The logs are fixed words, never a colour or a position (`the_capture_logs_fixed_words_only`,
  `the_picker_logs_fixed_words_only`, `the_picker_model_is_pure_and_cannot_panic`), the PNG is
  encoded on the capture thread only and copied through a file (`the_png_is_encoded_on_the_capture_thread_and_copied_through_a_file`),
  and one file speaks the screencopy protocol (`screencopy_only_in_screencopy_rs`).

See [decisions.md](../decisions.md): "A capture never touches the clipboard unless it succeeded",
"The colour picker picks from a frozen frame and copies text through `copy`, as written",
"A capture waits for the selector's removal", "A screenshot reaches the copy command as a file on its
standard input", "A frame that never comes fails the capture", "Screenshots are encoded by the `png`
crate, on a short-lived thread, and nothing of a capture is kept".

## What it costs

Nothing at rest: no thread, no timer, no surface, no buffer. A capture lives for a few hundred
milliseconds: the selector's shm pool (≈ 3.7 MB while it is open), one frame (3.6 MB at 1280 × 720),
the PNG and the worker thread, all given back (`release_memory`). Ten more captures grew the daemon's
anonymous memory by 8 to 16 KB (limit 64 KB), with 0 wake-ups at rest afterwards
(`at_rest_no_wakeups_after_a_screen_capture`). The `png` crate adds ≈ 78 KB to the stripped binary.
The colour picker, while open, holds the frame (3.6 MB at 1280 × 720) and its surface's pool
(≈ 3.7 MB): +7.6 MB of PSS, all given back at close; ten more picks moved the daemon's anonymous
memory by −12 to +12 KB (limit 64 KB), with 0 wake-ups at rest afterwards
(`at_rest_no_wakeups_after_a_colour_pick`). See [measurements.md](../measurements.md).
