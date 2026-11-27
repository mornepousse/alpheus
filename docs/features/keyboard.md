# The keyboard module

`󰌌 us` in the bar: the active keyboard layout, and the compositor's bind mode when it is not the
default one (`󰌌 us resize`). This page says what it does, how to use it, how to configure it, how it
is built, which rules the tests enforce on it, and what it costs. It describes the code as it is
today.

## What it does

The `keyboard` module draws the keyboard glyph and the layout's short name, in the bar's normal
ink, in the status block. The name comes from the compositor, the way it gives it
(`English (US)`), and is shortened by default: the two or three ASCII letters in its parentheses
(`English (US)` → `us`, `German (CH)` → `ch`), else its first two characters (`French (AZERTY)` →
`fr`, `Russian` → `ru`), lower case.

- **The bind mode** follows the layout in the theme's accent when it is not `default`
  (`󰌌 us resize`), and only then: the default mode draws nothing.
- **Nothing is drawn** while neither the layout nor a mode other than `default` is known: a
  compositor that gives neither, or before its first snapshot. The module is not a status that can
  be down: the compositor source going away hides it, it never shows `–`.
- **The module is opt-in.** It is not in the default `modules`; add `"keyboard"` where you want it.
  In `ModuleId::ALL` it sits right after `window`; on a narrow bar it is removed right after the
  window title and before the system indicators, since it says what you set, not what changed.
- **Display only.** A click does nothing: switching the layout or leaving a mode is the
  compositor's own key binding, and a command run on a click would be a write to the compositor the
  bar does not make (CONTRIBUTING.md, "The rules").
- **No Caps Lock or Num Lock.** No source of the lock keys is both event-driven and private
  without keyboard focus: the bar never has focus, `wl_keyboard`'s modifiers reach only the
  focused surface, the LEDs in sysfs can go stale and reading `/dev/input` is a key-logger by shape.
  See "The keyboard layout comes from the compositor's own state, never from the keyboard" in
  [decisions.md](../decisions.md).

| Compositor | Layout and mode |
| --- | --- |
| Mango | yes: the monitors snapshot already read for the tags (`keyboardlayout`, `keymode`), the active monitor's |
| Sway | yes: `GET_INPUTS` once per session, then the `input` and `mode` events; no mode until Sway sends its first `mode` event; with several keyboards, the last one that changed |
| Hyprland, niri | nothing drawn (the backends of both are retired) |
| Other layer-shell compositors | nothing drawn |

## Using it

Add the module to `modules` in `bar.toml`:

```toml
modules = ["launcher", "workspaces", "window", "keyboard", "volume", "network", "clock"]
```

There is no click, verb or popup. The bar redraws when the compositor reports a new layout or mode.

## Configuration

In `bar.toml`, read at start. The section has an effect only with `"keyboard"` in `modules`.

| Key (under `[keyboard]`) | Default | Meaning |
| --- | --- | --- |
| `short` | `true` | the short name (`us`); `false`: the full name as the compositor gives it (`English (US)`) |
| `show_mode` | `true` | the bind mode after the layout when it is not `default`; `false`: never |

```toml
[keyboard]
short = false      # 󰌌 English (US)
show_mode = false  # never the bind mode
```

## How it is built

- **The data** is two fields of the compositor's state, `CompositorState.layout` and `.mode`,
  filled by the compositor source only when the module is in `modules`: `startup::keyboard_wanted`
  reads `Needs::KEYBOARD`, which only this module's row of `modules.rs` asks for, and passes it to
  `sources::start`. Without the module, Mango's two fields are parsed and dropped, so a layout
  switch publishes nothing and the main loop does not wake.
- **Under Sway** (`sources/sway.rs`, `compositor/sway.rs`), the module changes what the source
  sends: its subscription adds the `input` and `mode` events, and it reads `GET_INPUTS` once at the
  start of each session, where the first keyboard with a layout name gives the layout. Then each
  `input` event (`xkb_layout`, or `xkb_keymap`) carries the keyboard's new active layout, and each
  `mode` event the mode's name: the model takes them, and the source publishes its last state with
  them, without asking Sway anything (no `GET_INPUTS`, no tree read). Sway names no mode at start,
  so none is drawn before the first `mode` event. With several keyboards the last one that changed
  wins. Without the module the source subscribes to the workspace and window events only and never
  sends `GET_INPUTS`: these two messages are the owner's extension of the Sway guardrail
  (`sway_writes_only_its_messages`; "Sway is asked for its inputs once per session, and only with
  the keyboard" in [decisions.md](../decisions.md)).
- **The segment** is `segments::keyboard`, with `segments::short_layout` for the name; it carries
  no click zone. Its row in `modules.rs` has no hue, tab or popup, and `layout.rs` places it in
  `REMOVAL`.
- **The redraw.** `state::same_drawing` reads the layout and the mode beside the workspaces and
  the title, so a state that changes only the layout or the mode redraws the bar. Without the
  module both stay None and never cause a redraw.
- **The configuration** is `KeyboardConfig` in `config.rs`; `ModuleId::defaults` is the default
  `modules`, every module but this one.

**The path of one change.** Mango writes a new monitors snapshot on the watch the compositor
thread already holds; the thread parses it, and because the keyboard is wanted the state differs,
so it mails `Msg::Compositor`. The bar applies it and redraws; the segment reads the two fields.
Under Sway, the switch arrives as one `input` event on the subscribed connection, and the
compositor thread mails the last state with the new layout, without a read.

The tests: `short_layout_*` and `keyboard_segment_*` (`segments/tests.rs`), `keyboard_defaults`,
`keyboard_unknown_key_is_invalid`, `keyboard_is_not_a_default_module` (`config/tests.rs`),
`keyboard_needs_the_keyboard_fields_only` (`modules/tests.rs`), `keyboard_wanted_with_the_module`
(`startup/tests.rs`), `the_keyboard_leaves_right_after_the_title` (`layout/tests.rs`), and two
headless tests under a fake Mango (`keyboard_under_mango_shows_the_layout_and_follows_a_change`,
`keyboard_under_mango_draws_a_layout_only_change`,
`keyboard_off_under_mango_redraws_nothing_on_a_layout_change`); `a_layout_or_mode_only_change_redraws`
(`state.rs`) for the redraw check. Under Sway: `sway_get_inputs_*`, `sway_input_*`, `sway_mode_*`,
`sway_no_mode_before_the_first_mode_event`, `sway_keyboard_fields_stay_none_without_keyboard` and
`sway_inputs_unreadable_or_too_deep_changes_nothing` (`compositor/sway.rs`);
`keyboard_off_subscribes_to_two_events_and_never_asks_inputs`,
`keyboard_on_subscribes_to_four_events_and_asks_inputs_once`,
`an_input_event_updates_the_layout_without_reading_the_tree`,
`a_mode_event_is_published_without_reading_the_tree` and
`a_burst_of_input_and_window_events_reads_the_tree_once` (`sources/sway/tests.rs`, a fake Sway);
and two headless tests on the isolated Sway, where the harness's virtual keyboard has two layouts
and the harness switches them, or enters a mode, on its private socket
(`keyboard_under_sway_follows_a_layout_switch`, `keyboard_under_sway_shows_a_mode`).

## Rules it keeps

- No thread, timer, connection or child of its own: the layout rides the compositor source's
  existing read under Mango, and its existing connections under Sway
  (`at_rest_no_wakeups_with_the_keyboard_module`, both compositors).
- Sway is only read: `GET_INPUTS` and the two more events are the whole extension
  (`sway_writes_only_its_messages`, `tests/guardrails.rs`).
- Off costs nothing at run time: without the module the source publishes no layout
  (`keyboard_off_under_mango_redraws_nothing_on_a_layout_change`, and
  `a_layout_change_without_keyboard_publishes_nothing` in `sources/mango.rs`); under Sway it
  subscribes to no keyboard event and never asks for the inputs
  (`keyboard_off_subscribes_to_two_events_and_never_asks_inputs`).
- No click: `every_click_on_a_full_bar_stays_on_the_allowlist` (`tests/guardrails.rs`) draws the
  module on its full bar, and it adds no command.
- No log line carries a layout or a mode.

## What it costs

Measured in release, back to back with the version before it (the full table is in
[measurements](../measurements.md)).

- **Linked in, `keyboard` not in `modules`:** within the harness's spread at start (−108…−106 KB
  PSS over three pairings, taken on a loaded machine; the steady sign is unexplained, probably the
  shared pages split across the daemons running at the same time); the stripped daemon grew by
  16 KiB.
- **Module on, under Mango** (`keyboard_module_pss_under_mango`): +4 KB of anonymous memory over
  the same bar without it (PSS +68 KB), in each of three runs. The anonymous figure is the one that
  counts; PSS moves with the shared pages.
- **Layout changes:** +40 KB anonymous over the first 100 (the new letters' first rasterisation and
  the allocator settling), then +0 KB over the next 100 (bound 8 KB), in each run; flat up to 500
  changes in a longer run.
- **Under Sway** (`keyboard_module_pss_under_sway`): +4 KB of anonymous memory with the module
  (one `GET_INPUTS` reply, read and dropped, and two strings), +16 KB over the first 100 layout
  switches and +0 KB over the next 100, in each of three runs. Without the module the Sway source
  sends what it sent before.
- **Wake-ups:** none at rest once a change is drawn, under Mango and under Sway
  (`at_rest_no_wakeups_with_the_keyboard_module`).
