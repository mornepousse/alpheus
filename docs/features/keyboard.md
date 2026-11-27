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
| Sway | not yet: nothing drawn |
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
- **The segment** is `segments::keyboard`, with `segments::short_layout` for the name; it carries
  no click zone. Its row in `modules.rs` has no hue, tab or popup, and `layout.rs` places it in
  `REMOVAL`.
- **The configuration** is `KeyboardConfig` in `config.rs`; `ModuleId::defaults` is the default
  `modules`, every module but this one.

**The path of one change.** Mango writes a new monitors snapshot on the watch the compositor
thread already holds; the thread parses it, and because the keyboard is wanted the state differs,
so it mails `Msg::Compositor`. The bar applies it and redraws; the segment reads the two fields.

The tests: `short_layout_*` and `keyboard_segment_*` (`segments/tests.rs`), `keyboard_defaults`,
`keyboard_unknown_key_is_invalid`, `keyboard_is_not_a_default_module` (`config/tests.rs`),
`keyboard_needs_the_keyboard_fields_only` (`modules/tests.rs`), `keyboard_wanted_with_the_module`
(`startup/tests.rs`), `the_keyboard_leaves_right_after_the_title` (`layout/tests.rs`), and two
headless tests under a fake Mango (`keyboard_under_mango_shows_the_layout_and_follows_a_change`,
`keyboard_off_under_mango_redraws_nothing_on_a_layout_change`).

## Rules it keeps

- No thread, timer, connection or child of its own: the layout rides the compositor source's
  existing read (`at_rest_no_wakeups_with_the_keyboard_module`).
- Off costs nothing at run time: without the module the source publishes no layout
  (`keyboard_off_under_mango_redraws_nothing_on_a_layout_change`, and
  `a_layout_change_without_keyboard_publishes_nothing` in `sources/mango.rs`).
- No click: `every_click_on_a_full_bar_stays_on_the_allowlist` (`tests/guardrails.rs`) draws the
  module on its full bar, and it adds no command.
- No log line carries a layout or a mode.

## What it costs

Measured in release, back to back with the version before it (the full table is in
[measurements](../measurements.md)).

- **Linked in, `keyboard` not in `modules`:** within the harness's spread at start (−108…−106 KB
  PSS over three pairings); the stripped daemon grew by 16 KiB.
- **Module on, under Mango** (`keyboard_module_pss_under_mango`): 0…+4 KB of anonymous memory over
  the same bar without it (PSS 0…+176 KB).
- **100 layout changes:** +40…+44 KB anonymous, the same in each run (probably the new letters' first
  rasterisation), bound 64 KB.
- **Wake-ups:** none at rest after a change (`at_rest_no_wakeups_with_the_keyboard_module`).
