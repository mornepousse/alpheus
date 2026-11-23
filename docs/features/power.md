# The power menu

A ⏻ button at the bar's end that opens a small menu: Lock, Log out, Reboot, Power off. This page
says what it does, how to use it, how to configure it, how it is built, which rules the tests
enforce on it, and what it costs. It describes the code as it is today.

## What it does

The `power` module draws `⏻` alone, in its own block, in the bar's normal text colour (no hue), after
the clock. A left click on it opens a popup under it with one row per session command that is set:

```
┌──────────────┐
│  Lock        │
│ 󰍃 Log out     │
│ 󰜉 Reboot      │
│ ⏻ Power off   │
└──────────────┘
```

- The rows come in the order Lock, Log out, Reboot, Power off. Each shows its glyph in the accent
  colour, then its word. The selected row is on a band.
- **Lock** and **Log out** run at once, then the menu closes.
- **Reboot** and **Power off** ask first: the row becomes `󰜉 Reboot? ⏎` or `⏻ Power off? ⏎` in the
  warning colour. A second activation runs the command, and the menu closes.
- With all four commands empty there is no menu, and no button.

alpheus never handles sleep, so the menu has no suspend or hibernate entry.

## Using it

- **Left click on ⏻** opens the menu; a second left click on it closes it.
- **↓ or Tab** selects the first row, **↑ or Shift+Tab** the last. Nothing is selected when the menu
  opens. Ctrl+N and Ctrl+P move too. The selection stops at the ends; it does not wrap round.
- **Enter or Space** runs the selected row (Enter with nothing selected does nothing). A **left
  press** on a row selects it and runs it. **Hovering** a row selects it.
- **Esc**, or a click outside the menu, closes it. So does a change of style that moves the bar,
  or the output going away.
- **The question.** A second activation counts only from 0.4 s after the question appeared, and
  only until 3 s after it, so a double click or a double Enter never confirms. Moving the
  selection, pressing another row, any other key, or 3 s passing cancels the question and draws
  the row plain again.
- **A held key** repeats only to move: a held Enter acts once. The wheel, and the right and middle
  buttons, do nothing in the menu.
- **From a key.** `alpheus-launch --power` opens the menu under the bar's right end on the focused
  output (it works without the `power` module, and prints an error and exits 2 when no session
  command is set or when there is no bar to open the menu under), and the same verb closes it.
  The keyboard is taken without a grab, because a verb has no pointer press to grab with. It is
  also the Tools menu's `Power menu` entry.

The click-opened menu is a popup with a grab (`Input::Grab`). [The bar page](bar.md) notes what that means for
the bar's keyboard on Hyprland.

## Configuration

The menu has no setting of its own. It runs the commands of `[quick]` in `bar.toml`, the same ones
the quick panel's session buttons run, read at start (a change needs a restart of the bar).

| Key (under `[quick]`) | Default | Meaning |
| --- | --- | --- |
| `lock` | `["loginctl", "lock-session"]` | the Lock row |
| `logout` | `[]` | the Log out row, which shows only once a command is set |
| `reboot` | `["systemctl", "reboot"]` | the Reboot row |
| `poweroff` | `["systemctl", "poweroff"]` | the Power off row |

A command is an argument list, run as written: there is no shell, so no `$VARIABLE` and no `;`. An
empty list hides the row.

```toml
[quick]
logout = ["my-logout-command"]
```

A `modules` list of your own needs `"power"` added, after `"clock"`, to show the button. On a
narrow bar the button is the last module to be removed. Without the `window` module in `modules`,
the free space goes before the last module, so a bar configured `[..., "clock", "power"]` shows the
⏻ alone at the right end, with the clock at the left (what is wanted there, if the clock should
stay beside it, is not decided: power plan 1, decision 13, to be confirmed at the trial).

## How it is built

Two files in `crates/alpheus-daemon/src/power/`, and the rule they share in `session.rs`:

- `mod.rs`: the pure model. `Power` (the entries, the selection, the pending question),
  `PowerAction` (what a key or the pointer asks of the wiring: nothing, redraw, close, or run an
  entry and close), `key`, `hover`, `press`, `due`, `deadline` and `render`. The tests sit at the
  bottom of the file.
- `app.rs`: the wiring. `OPS` is the menu's row in the table of popups (`popup::POPUPS`).
  `power_toggle` opens the popup from a click or the verb; `power_key` and `power_pointer` hand
  input to the model; `power_action` carries out what the model answers: redraw, close, or run an
  entry's command through the daemon's executor (`spawn_session`) and close.
- `session.rs`: the `Session` enum (glyph, word, prompt, which entries ask, which `[quick]` command),
  the 3 s and 0.4 s constants and `activate` and `expire`, shared with the quick panel's session
  buttons, so the panel and the menu cannot drift apart.

The button itself is a segment (`segments::power`, none when `session::any_configured` is false)
and `ModuleId::Power`, the last of `ModuleId::ALL`; its click zone is the module.

**The question's timer.** `power_timer` is one `DeadlineTimer` that `power_timer_sync` arms to the
question's end after every repaint, and disarms when there is no question. `power_closed`, the
menu's `closed` hook, disarms it whatever closed the menu, so nothing is left running.

**The path of a click.** The bar's click handler finds the module's zone and calls the popup table's
`toggle` for it, which is `power_toggle`. It builds a `Power` from `[quick]`, asks for the popup,
paints the rows and logs `power menu shown (lock reboot poweroff)` (the entries, never the
commands). A key reaches `power_key`, the pointer `power_pointer`; an entry that runs is spawned
and logged as `power: power off` (`power: <word> could not start (<error>)` on a failed start),
never as its command, which can name the user.

## Rules it keeps

- `the_tray_spawns_no_thread`: `power/mod.rs` starts no thread, process or command.
- `the_power_menu_has_no_panicking_path`: neither file's production code panics.
- `the_power_menu_lets_go_of_its_timer_and_runs_only_its_entry`: the popup's `closed` hook releases
  the timer, a repaint re-syncs it, and the only command spawned is the entry's own `[quick]`
  command, once, through the executor.
- `every_power_menu_activation_is_a_configured_session`: whatever the model is driven through, it
  only ever asks to run an entry whose command is set, no command mentions sleep, and Reboot and
  Power off never run without their question and a second activation at least 0.4 s later.
- The headless tests (`power_a_click_opens_the_menu_and_down_enter_locks`,
  `power_a_click_on_a_row_runs_it`, `power_off_asks_then_a_second_enter_after_half_a_second_runs_it`,
  `power_two_enters_a_tenth_of_a_second_apart_run_nothing`,
  `power_escape_closes_it_with_its_question_and_nothing_stays_awake` and
  `power_verb_opens_and_closes_the_menu`) pin the click, the keys, the question and the verb against
  a fake executor that records each command; no test runs a real `loginctl` or `systemctl`.
- The reasons are in [decisions.md](../decisions.md): "The power menu runs the `[quick]` commands
  and has no setting of its own", "Reboot and power off ask first, and a second activation
  confirms from 0.4 s to 3 s", "Nothing is selected when the power menu opens", "A held key repeats
  only to move, in the power menu", "The power menu has its own timer", "`alpheus-launch --power`
  opens the menu without a grab".

## What it costs

Measured in release, back to back, 3 runs (the [memory tables](../measurements.md) have the figures and the
method):

- **Linked in, no `power` in `modules`:** within noise of the build before it (binary +8 KiB
  stripped).
- **Button, menu closed:** about 184 to 192 KB over a bar without it. About 128 KB of that is pages of
  the Nerd Font file already mapped (⏻ U+23FB is in it) and about 64 KB the module's code: clean,
  file-backed, paid once. It has no surface and no timer.
- **Menu open:** about 432 KB; once closed, +280 KB over idle (the first open's code and font
  pages); twenty more open and close cycles +12 KB (`power_menu_pss_idle_open_and_closed`). Its
  surface and pool exist only while it shows, its timer only while a question is pending.
- **At rest:** no loop wake-up after the menu closed, a pending question included
  (`at_rest_no_wakeups_after_the_power_menu`).
