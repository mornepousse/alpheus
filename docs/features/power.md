# The power menu

A ⏻ button at the bar's end that opens a small menu: Lock, Log out, Reboot, Power off, and rows of
your own. This page says what it does, how to use it, how to configure it, how it is built, which
rules the tests enforce on it, and what it costs. It describes the code as it is today.

## What it does

The `power` module draws `⏻` alone, in its own block, in the bar's normal text colour (no hue), after
the clock. A left click on it opens a popup under it with one row per session command that is set:

```
┌─────────────────────┐
│  Lock           1  │
│ 󰍃 Log out        2  │
│ 󰜉 Reboot         3  │
│ ⏻ Power off      4  │
└─────────────────────┘
```

- The rows come in the order Lock, Log out, Reboot, Power off. Each shows its glyph in the accent
  colour, then its word, and at its right its digit, dim, in a capsule (rows 1 to 9; the menu has
  at most 12). The selected row is on a band, which stops before the capsule.
- The menu is as wide as its longest text needs: a row's label, its question when it asks, its
  countdown's line when it has one, so it keeps one size whatever it shows, and a 24-character
  name shows whole.
- **Lock** and **Log out** run at once, then the menu closes.
- **Reboot** and **Power off** ask first: the row becomes `󰜉 Reboot? ⏎` or `⏻ Power off? ⏎` in the
  warning colour. A second activation runs the command, and the menu closes.
- **Rows of your own** (`[[power.entry]]`) come after the fixed ones, in file order: their icon
  (`•` without one) and their name. One runs at once, or asks first like Reboot when it has
  `confirm = true`. `[power] builtins = false` hides the four fixed rows.
- With no row at all (the four commands empty or hidden, and no row of your own) there is no menu,
  and no button.

alpheus never handles sleep, so the menu has no suspend or hibernate entry, and a row of your own
that names one is refused (see Configuration).

## Using it

- **Left click on ⏻** opens the menu; a second left click on it closes it.
- **↓ or Tab** selects the first row, **↑ or Shift+Tab** the last. Nothing is selected when the menu
  opens. Ctrl+N and Ctrl+P move too. The selection stops at the ends; it does not wrap round.
- **Enter or Space** runs the selected row (Enter with nothing selected does nothing). A **left
  press** on a row selects it and runs it. **Hovering** a row selects it.
- **A digit**, 1 to 9, selects its row and activates it: a row that does not ask runs at once; a
  row that asks asks, and the same digit pressed again 0.4 s to 3 s later confirms it (so does
  Enter). Another digit asks its own row's question; a digit past the rows does nothing but drop
  a pending question. The top row's keys count by position, whatever the layout types there: on
  AZERTY the `&` key is 1, no Shift needed. The keypad with NumLock works too, and a key that
  types a digit is always that digit.
- **Esc**, or a click outside the menu, closes it. So does a change of style that moves the bar,
  or the output going away.
- **The question.** A second activation counts only from 0.4 s after the question appeared, and
  only until 3 s after it, so a double click or a double Enter never confirms. Moving the
  selection, pressing another row, any other key, or 3 s passing cancels the question and draws
  the row plain again.
- **A held key** repeats only to move: a held Enter or digit acts once. The wheel, and the right and middle
  buttons, do nothing in the menu.
- **From a key.** `alpheus-launch --power` opens the menu under the bar's right end on the focused
  output (it works without the `power` module, and prints an error and exits 2 when the menu has no
  row or when there is no bar to open the menu under), and the same verb closes it.
  The keyboard is taken without a grab, because a verb has no pointer press to grab with. It is
  also the Tools menu's `Power menu` entry.

The click-opened menu is a popup with a grab (`Input::Grab`). [The bar page](bar.md) notes what that means for
the bar's keyboard on Hyprland.

## Configuration

The fixed rows run the commands of `[quick]` in `bar.toml`, the same ones the quick panel's
session buttons run; `[power]` adds rows of your own and can hide the fixed ones. Both are read at
start (a change needs a restart of the bar).

| Key (under `[quick]`) | Default | Meaning |
| --- | --- | --- |
| `lock` | `["loginctl", "lock-session"]` | the Lock row |
| `logout` | `[]` | the Log out row, which shows only once a command is set |
| `reboot` | `["systemctl", "reboot"]` | the Reboot row |
| `poweroff` | `["systemctl", "poweroff"]` | the Power off row |

| Key (under `[power]`) | Default | Meaning |
| --- | --- | --- |
| `builtins` | `true` | `false` hides the four rows above from this menu (the quick panel keeps them) |
| `countdown` | `0` | seconds counted after a confirming activation, 0 to 30; read and checked, the countdown itself is not built yet |
| `[[power.entry]]` | none, at most 8 | a row of your own: `name` (1 to 24 characters), `icon` (one character, default `•`), `run` (an argument list, not empty), `confirm` (default `false`: runs at once), `countdown` (0 to 30, this row's own; not used yet) |

A command is an argument list, run as written: there is no shell, so no `$VARIABLE` and no `;`. An
empty `[quick]` list hides its row.

```toml
[quick]
logout = ["my-logout-command"]

[power]
builtins = true

[[power.entry]]
name = "Quit session"
icon = "x"
run = ["my-session-quit", "--now"]
confirm = true
```

A row of your own never sleeps: a `run` holding `suspend`, `hibernate`, `hybrid-sleep`, `dpms`,
niri's `power-off-monitors` or `wlopm` anywhere in any argument, in any case, makes `bar.toml`
invalid (the defaults then apply, with the reason in the log, as for any refused key). So do a tenth
row, a name past 24 characters, an icon of two characters, an empty `run`, a blank program or a NUL.
The message names the row by its position (`power.entry 2: …`), never by its name or command; for
a sleep word it names the word found (`an argument of run contains "suspend"`). The screens' word is
read by its stem `dpm`, so an argument merely containing those three letters, a path for instance,
is refused too, and the message says `"dpm"`. Sway's `output … power off` is not refused: its words are
too common to spot without refusing harmless rows.

A `modules` list of your own needs `"power"` added, after `"clock"`, to show the button. On a
narrow bar the button is the last module to be removed. Without the `window` module in `modules`,
the free space goes before the last module, so a bar configured `[..., "clock", "power"]` shows the
⏻ alone at the right end, with the clock at the left (what is wanted there, if the clock should
stay beside it, is not decided yet).

## How it is built

Two files in `crates/alpheus-daemon/src/power/`, and the rule they share in `session.rs`:

- `mod.rs`: the pure model. `PowerEntry` (what a row runs: a fixed `Session`, or `User(i)`, the
  `i`th `[[power.entry]]`; its `command` and its log `word`), `PowerRow` (its entry, glyph, label,
  whether it asks, its countdown), `rows` and `any_rows` (built from `BarConfig` at open), `Power`
  (the rows, the selection, the pending question), `PowerAction` (what a key or the pointer asks of
  the wiring: nothing, redraw, close, or run a row and close), `key` (digits included), `hover`,
  `press`, `due`, `deadline`, `position_digit` (the top row's keys 1 to 9 by their evdev code,
  which `popup.rs`'s `popup_key_of` asks only while the power menu is open), `width` (the longest text plus the pads, the gap and the capsule) and
  `render` (the digit capsules are `ShapeKind::Chip(Tint::Pill)` shapes of the `Boxed`, painted
  under the cells by `paint_boxed`). The tests sit at the bottom of the file.
- `app.rs`: the wiring. `OPS` is the menu's row in the table of popups (`popup::POPUPS`).
  `power_toggle` opens the popup from a click or the verb; `power_key` and `power_pointer` hand
  input to the model; `power_action` carries out what the model answers: redraw, close, or run a
  row's command (`PowerEntry::command`) through the daemon's executor (`spawn_session`) and close.
- `session.rs`: the `Session` enum (glyph, word, prompt, which entries ask, which `[quick]` command),
  the 3 s and 0.4 s constants and `ask` and `expire`, shared with the quick panel's session
  buttons, so the panel and the menu cannot drift apart. A user row with `confirm` asks through the
  same `ask`.
- `config.rs`: `PowerConfig` and `PowerEntryConfig` (`[power]`), checked by `check_power` at load.

The button itself is a segment (`segments::power`, none when `power::any_rows` is false)
and `ModuleId::Power`, the last of `ModuleId::ALL`; its click zone is the module.

**The question's timer.** `power_timer` is one `DeadlineTimer` that `power_timer_sync` arms to the
question's end after every repaint, and disarms when there is no question. `power_closed`, the
menu's `closed` hook, disarms it whatever closed the menu, so nothing is left running.

**The path of a click.** The bar's click handler finds the module's zone and calls the popup table's
`toggle` for it, which is `power_toggle`. It builds a `Power` from `[quick]` and `[power]`, asks
for the popup, paints the rows and logs `power menu shown (lock reboot poweroff entry 1)` (a fixed
row by its `[quick]` key, a row of your own by its position, never a name or a command). A key
reaches `power_key`, the pointer `power_pointer`; a row that runs is spawned and logged as
`power: power off` or `power: entry 2` (`power: <word> could not start (<kind word>)` on a failed
start), never as its command or its name, which can name the user.

## Rules it keeps

- `the_tray_spawns_no_thread`: `power/mod.rs` starts no thread, process or command.
- `the_power_menu_has_no_panicking_path`: neither file's production code panics.
- `the_power_menu_lets_go_of_its_timer_and_runs_only_its_entry`: the popup's `closed` hook releases
  the timer, a repaint re-syncs it, and the only command spawned is the row's own (a fixed row's
  `[quick]` command, a user row's `run`, in the two arms of `PowerEntry::command`), once, through
  the executor.
- `every_power_menu_activation_is_a_configured_session`: over the four commands set or not, no row
  of your own, a plain one or one that asks, and the fixed rows shown or hidden, whatever the model
  is driven through it only ever asks to run a row whose command is set (a fixed row only while
  shown, and with its `[quick]` command), every row is reached, no command holds a sleep word, and
  Reboot, Power off and a `confirm` row never run without their own question and a second
  activation at least 0.4 s later. The keys driven include the digits 1 to 9.
- `the_top_rows_keys_are_digits_by_position` and `the_power_menu_reads_the_top_rows_digits_by_position`:
  evdev 2 to 10 are the digits 1 to 9 in the power menu only (AZERTY's `&` is 1), never with Ctrl,
  Alt or Super, never over a named key (an arrow, Enter) or a typed digit, which a virtual keyboard
  can send on any code; other popups, the keypad and QWERTY get the typed key. A held digit still
  goes through the repeat rule.
- `a_power_entry_naming_sleep_or_dpms_makes_bar_toml_invalid` and
  `a_power_entry_naming_sleep_makes_bar_toml_invalid`: a row of your own that names a sleep word is
  refused at load. The source matches the stem `dpm`, since `output_power_management_never_appears`
  keeps the word itself out of every source file.
- The headless tests (`power_a_click_opens_the_menu_and_down_enter_locks`,
  `power_a_click_on_a_row_runs_it`, `power_off_asks_then_a_second_enter_after_half_a_second_runs_it`,
  `power_two_enters_a_tenth_of_a_second_apart_run_nothing`,
  `power_escape_closes_it_with_its_question_and_nothing_stays_awake`,
  `power_verb_opens_and_closes_the_menu` and `power_a_digit_runs_a_user_row`) pin the click, the
  keys, a digit, the question and the verb against
  a fake executor that records each command; no test runs a real `loginctl` or `systemctl`.
- The reasons are in [decisions.md](../decisions.md): "The power menu runs the `[quick]` commands,
  and `[power]` adds rows", "A user row of the power menu never sleeps", "Reboot and power off ask
  first, and a second activation confirms from 0.4 s to 3 s", "Nothing is selected when the power
  menu opens", "A held key repeats only to move, in the power menu", "A digit picks a power row,
  and the same digit confirms", "The power menu has its own
  timer", "`alpheus-launch --power` opens the menu without a grab".

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
