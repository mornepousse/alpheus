# The focus timer

A countdown, a Pomodoro and a stopwatch, shown as a small chip in the bar, with a popup under it.
This page says what it does, how to use it, how to configure it, how it is built, which rules the
tests enforce on it, and what it costs. It describes the code as it is today.

## What it does

The `timer` module draws one chip, just before the clock. The chip changes once a minute at most:
it never shows seconds.

| The timer is | The chip reads | Colour |
| --- | --- | --- |
| idle (nothing runs) | `󰔛`, alone | dim |
| a countdown running | `󰔛 25m` (minutes left, rounded up; `󰔛 1h05` from an hour) | yellow |
| a Pomodoro, working | `󰔟 25m` | yellow |
| a Pomodoro, on a break | `󰅶 5m` | yellow |
| a stopwatch running | `󱎫 12m` (minutes counted, rounded down) | yellow |
| paused (any kind) | `󰏤 24m` | dim |
| an end that nobody has looked at yet | `󰂞 done` | warning colour |

The three kinds:

- **Countdown.** A length from 1 second to 24 hours. At the end you get a notification, an
  optional sound, and the chip reads `done` until you look at it.
- **Pomodoro.** A work session, then a short break, and so on for a number of rounds. After the last
  work session comes a long break, and then the set is over (`done`, as for a countdown). Each
  phase starts by itself when the previous one ends, and each end notifies. `Skip` goes to the next
  phase at once and says nothing.
- **Stopwatch.** Counts up from zero. It stops itself, paused, after 24 hours.

Only one timer exists at a time: starting a new one replaces whatever ran. Every kind can be paused
and resumed, and a countdown can take one more minute.

Time is the wall clock's. A laptop that sleeps through the end of a countdown rings it when it
wakes (within a minute), and a stopwatch counts the sleep. The timer survives a restart of alpheus:
its state is kept in a small file (see "How it is built").

## Using it

**With the mouse.**

- Left click on the chip: opens or closes the popup. Opening it clears `done`.
- Middle click: pauses a running timer, resumes a paused one, clears `done`. Nothing when idle.
- Right click and the wheel on the chip do nothing.

**The popup** shows a status row (`Ready`, `Done`, or the time as `24:13` with `of 25:00`,
`work 2/4`, `break 2/4` or `long break` on its right; dim while paused), a separator, and the
actions the timer's state offers:

| State | Actions |
| --- | --- |
| idle | `Timer ‹ 10 min ›`, `Pomodoro` (`25 · 5 min`: work and short break), `Stopwatch` |
| countdown | `Pause` or `Resume`, `Add 1 min`, `Stop` |
| Pomodoro | `Pause` or `Resume`, `Skip`, `Stop` |
| stopwatch | `Pause` or `Resume`, `Stop` |

The popup stays open after an action. The first action is selected when it opens, and again each
time the list of actions changes. Keys: ↑ ↓ Tab Shift+Tab move (they stop at the first and last
row), Enter or Space act, ← → change the length on the `Timer` row, Esc closes. With the pointer,
hovering selects, a left press acts, and the wheel over the `Timer` row changes its length (up
lengthens). The length goes in steps of one minute below 10 minutes and five minutes from 10, from
1 to 180 minutes. The time in the popup ticks each second, but only while the popup is open.

**From a script or a key bind**, with `alpheus-launch --timer <word>`:

| Word | What it does |
| --- | --- |
| `start <duration>` | starts a countdown: `90s`, `25m`, `1h30m`, `1m30s`, or a bare number of minutes; 1 second to 24 hours |
| `pomodoro` | starts a Pomodoro |
| `stopwatch` | starts a stopwatch |
| `pause`, `resume`, `toggle`, `stop`, `skip` | act on whatever runs; nothing to act on is not an error (the log says `timer: nothing to <word>`) |
| `popup` | opens or closes the popup under the chip on the focused output (under the bar's right end when the chip is hidden), taking the keyboard without a grab |

Every verb first clears `done`. An error (exit code 2) comes back when `timer` is not in `modules`;
exit code 3 means the daemon is not running or is older than the verb. A second `--timer popup`
within 250 milliseconds of the one that opened the popup is ignored, because some compositors fire
a key bind twice. Binding keys is up to you: alpheus does not set any.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar). The module must be in
`modules`; the default list already has it, just before `"clock"`. A `modules` list you wrote
yourself needs `"timer"` added.

| Key (under `[timer]`) | Default | Meaning |
| --- | --- | --- |
| `countdown` | `10` | minutes the popup's `Timer` row starts at, 1 to 180 |
| `work` | `25` | Pomodoro work session, minutes, 1 to 180 |
| `short_break` | `5` | Pomodoro short break, minutes, 1 to 180 |
| `long_break` | `15` | Pomodoro long break, minutes, 1 to 180 |
| `rounds` | `4` | work sessions before the long break, 1 to 12 |
| `sound` | `[]` | a program and its arguments, run at each end (never through a shell, never waited for); empty means no sound |
| `idle` | `"glyph"` | `"glyph"` draws the dim glyph when nothing runs; `"hidden"` draws nothing (the `popup` verb still works) |

A value out of range, a blank sound program or an argument holding a NUL character is refused when
the file is read, with a message naming the key. Example:

```toml
[timer]
work = 50
short_break = 10
sound = ["paplay", "/usr/share/sounds/freedesktop/stereo/complete.oga"]
```

## How it is built

Three files in `crates/alpheus-daemon/src/timer/`, split the way alpheus splits most features: a
pure model, a pure popup, and the wiring that touches the outside world.

- `mod.rs`: the model. The `Timer` value, every `Change`, the `End` a timer comes to, the chip, the
  next moment the chip changes, the state file's line and the notification an end becomes. It
  reads no clock and starts nothing; every time is passed in as Unix milliseconds. `Timer` is a
  small value that is never edited in place: `Timer::apply` returns the new one.
- `popup.rs`: the popup's rows, its selection, and what each key or pointer event asks for, as a
  `TimerAction`. It changes nothing itself.
- `app.rs`: the wiring (`TimerApp` on the `App`, and the `timer_*` methods). It reads the clock,
  keeps the one deadline, sends the notification, starts the sound, and reads and writes the state
  file.
- The tests sit next to each file (`tests.rs`, `app/tests.rs`, `popup/tests.rs`).

**The one deadline.** The timer owns a single `DeadlineTimer` (a one-shot timer on the event loop).
It is armed only while a timer runs, at the next instant anything changes: the end, the next minute
of the chip's text, or, while the popup is open, the next second. An idle or paused timer arms
nothing, so alpheus sleeps. When it fires, `timer_tick` asks the wall clock what happened. The
clock module's minute tick also calls `timer_check_clock`, because the deadline runs on the loop's
own clock, which stops during a suspend.

**The state file** is `$XDG_STATE_HOME/alpheus/timer` (owner only), one line such as
`countdown running <end> <total>` or `pomodoro paused <left> short 2`. It is written on each change
of state but never on a tick, and removed when the timer is idle. At start it is read as hostile
input (exact fields, digits only, every length bounded) and dropped, with one log line, if anything
is off. An end that passed while alpheus was not running is applied once, silently. `done` is never
saved.

**The path of a click, from the bar to `timer_do`.**

1. The bar's click handler (`App::click` in `wayland.rs`) finds which module was clicked. A left
   click on the timer's zone calls the popup table's `toggle` for it, which is `OPS` in
   `timer/app.rs`, so `timer_toggle` opens the popup. A middle click goes to
   `App::timer_middle_click` instead.
2. Inside the popup, a key reaches `timer_key` (or a pointer event `timer_pointer`), which asks
   `TimerPopup` what to do. If it answers `TimerAction::Do(change)`, the wiring calls `timer_do`.
3. A verb arrives on the launcher's socket, and `launcher/socket_glue.rs` routes it to
   `timer_verb`, which calls `timer_do` too.
4. `timer_do` is the one place a change is made. It applies an end that is already due first
   (`change_step`), then the change, then saves the file, writes the log line, sends the chip to
   the bar (`Msg::Timer`) and re-sets the deadline (`timer_sync`).

The "why" behind each of these choices is in [decisions.md](../decisions.md): the entries from
"The timer is drawn at rest" to "Logs carry fixed words and numbers only".

## Rules it keeps

These are enforced by tests, so a change that breaks one fails the build.

- `the_timer_is_wired_through_one_path` (`tests/guardrails.rs`): the deadline's `fired()` comes
  before the clock check; the clock's tick re-checks the timer; the state is loaded whenever
  `timer` is in `modules`; the model is assigned only in `timer_do`, `timer_check_clock` and
  `timer_load`; and `timer_do` goes through `change_step`.
- `the_timer_has_no_panicking_path`: no `unwrap`, `expect` or `panic!` in the three files' production
  code (the state file is not ours, and the arithmetic is on wall-clock times).
- `the_tray_spawns_no_thread`: `timer/mod.rs` and `timer/popup.rs` start no thread, process or
  command. The list it reads is itself checked: an entry that names no file fails the test.
- `nothing_resident_while_the_launcher_is_closed`: one `DeadlineTimer` is built in `timer/app.rs`,
  and no other timer.
- `the_timer_popup_closing_drops_its_tick_and_sees_the_end`: whatever closes the popup, its
  one-second tick goes and an end it showed counts as seen.

## What it costs

Measured in release, back to back with the version before it, 3 runs each (the full table is in the
README; the tests that measure are named here).

- **Linked in, chip idle:** −269…+222 KB at start, which is within the harness's spread (nothing
  measurable); the binary grew by 60 KiB stripped. The idle chip itself is +24…+48 KB over the same
  bar without it (`timer_pss_idle_running_and_popup`): one glyph and a cell, nothing resident.
- **Running:** +0…+4 KB of the daemon's own memory over idle.
- **Popup:** closed again, +88…+92 KB over running (the first open's code and font pages, paid
  once); 20 more open and close cycles add +16…+27 KB (bound: 64 KB). Its window exists only while
  it shows.
- **Wake-ups:** none while idle or paused (`at_rest_no_wakeups_with_the_timer_idle_or_paused`); about
  one a minute with a countdown running (`timer_running_wakes_about_once_a_minute`, which reads 4 in
  65 s; a one-second tick would read 68); with the popup open, two per second (the tick and the
  previous buffer's release), and none after it closes by a click
  (`timer_popup_ticks_each_second_only_while_open`).
