# The focus timer

A countdown, a Pomodoro and a stopwatch, shown as a small chip in the bar, with a popup under it.
This page says what it does, how to use it, how to configure it, how it is built, which rules the
tests enforce on it, and what it costs. It describes the code as it is today.

## What it does

The `timer` module draws one chip, in a block of its own, just before the clock. The chip changes
once a minute at most: it never shows seconds.

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

### The scheduled shutdown

The timer has a second slot: a shutdown, scheduled from Tools › System › Schedule shutdown (30 min,
1 h, 1 h 30, 2 h or 3 h) or with `alpheus-launch --timer shutdown <duration>`. It runs alongside
whatever the timer does: a Pomodoro never cancels it, it never ends a Pomodoro, and starting,
stopping or skipping a timer leaves it as it is. A new duration replaces the shutdown pending.

- **The chip** leads with it: `⏻ 1h20` alone, or `⏻ 1h20 · 󰔛 25m` with a timer running (the minutes
  left, rounded up). It shows even when the idle chip is `hidden`, and turns to the warning colour in
  the last minute.
- **The warning.** A minute before the end comes a critical notification, `Powering off in 1 min`,
  `Click to cancel`, with no timeout; a click on it cancels the shutdown. It is set apart from the
  notification rules (a drop or a mute rule never hides it, and do-not-disturb never hides a critical
  notice). Closing it without the click does not cancel: the chip goes on counting.
- **The end** runs `[quick] poweroff` through the executor. With `[quick] poweroff` empty the Tools
  entry is hidden, the verb is refused (exit 2), and a shutdown already on file is dropped, with one
  log line, before any warning.
- **Never a surprise.** An end noticed late (the laptop woke up after it, the bar restarted) gives the
  warning first, then powers off a minute later at the earliest: the new end is the later of the old
  end and now + 60 s. A warning the machine slept through, even for a few seconds, or one the clock
  went back before, no longer counts and is given again. At start, a due warning waits for alpheus's
  notification server to own its name, at most 10 s. With no server at all (the `notifications`
  module off, or the wait spent) the chip still shows the last minute and one log line says the
  warning was not shown.
- **Cancelling:** the `Cancel shutdown` row of the popup, the first choice of the Tools entry
  (`Cancel shutdown (in 1h20)`), a click on the warning, or `--timer shutdown-cancel`.
- **A restart of alpheus keeps it** (the timer's state file has its line); a reboot does not. The line
  carries when the shutdown was scheduled, and at start a shutdown scheduled before this boot (the
  boot time is `btime` in `/proc/stat`), or of unknown age, is dropped with one log line. The kernel
  gives `btime` as the wall clock now sees the boot, so a clock stepped forward since the shutdown was
  scheduled makes it look older than the boot and drops it: the safe side.

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

While a shutdown is scheduled, a last row `Cancel shutdown` follows the actions, whatever the timer's
state.

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
| `shutdown <duration>` | schedules the shutdown that long from now (the same forms as `start`, 1 second to 24 hours), replacing the one pending; exit 2 when `[quick] poweroff` is empty |
| `shutdown-cancel` | cancels the shutdown; nothing to cancel is not an error |

Every verb but the two shutdown ones first clears `done`. An error (exit code 2) comes back when
`timer` is not in `modules`; exit code 3 means the daemon is not running or is older than the verb.
A second `--timer popup` within 250 milliseconds of the one that opened the popup is ignored,
because some compositors fire a key bind twice. Binding keys is up to you: alpheus does not set any.

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

The scheduled shutdown has no key of its own: it runs `[quick] poweroff`
([configuration](../configuration.md#quick)), and an empty `poweroff` turns it off.

## How it is built

Four files in `crates/alpheus-daemon/src/timer/`, split the way alpheus splits most features: a
pure model, a pure popup, and the wiring that touches the outside world, plus the shutdown's own pure
model.

- `mod.rs`: the model. The `Timer` value, every `Change`, the `End` a timer comes to, the chip, the
  next moment the chip changes, the state file's line and the notification an end becomes. It
  reads no clock and starts nothing; every time is passed in as Unix milliseconds. `Timer` is a
  small value that is never edited in place: `Timer::apply` returns the new one.
- `popup.rs`: the popup's rows, its selection, and what each key or pointer event asks for, as a
  `TimerAction`. It changes nothing itself.
- `app.rs`: the wiring (`TimerApp` on the `App`, and the `timer_*` methods). It reads the clock,
  keeps the one deadline, sends the notification, starts the sound, and reads and writes the state
  file. The shutdown's wiring is here too (`shutdown_do`, `shutdown_check_clock`, the warning and
  the power off).
- `shutdown.rs`: the scheduled shutdown's model. The `Shutdown` value (its end and when it was
  scheduled, both wall-clock times), `next` (the step due: the warning, then the power off, never the
  power off first), `warned_at` (a warning moves the end to a minute away at least), `warning_holds`
  (a warning slept through no longer counts), the Tools choices, the chip text, the warning's
  notification and the state line. Like `mod.rs` it reads no clock and starts nothing.
- The tests sit next to each file (`tests.rs`, `app/tests.rs`, `popup/tests.rs`, `shutdown/tests.rs`).

**The one deadline.** The countdown owns a single `DeadlineTimer` (a one-shot timer on the event
loop). It is armed only while a timer runs, at the next instant anything changes: the end, the next
minute of the chip's text, or, while the popup is open, the next second. An idle or paused timer
arms nothing, so alpheus sleeps. When it fires, `timer_tick` asks the wall clock what happened. The
clock module's minute tick also calls `timer_check_clock`, because the deadline runs on the loop's
own clock, which stops during a suspend.

**The shutdown's deadline.** The shutdown has a second `DeadlineTimer`, armed only while a shutdown
is scheduled, at the earliest of its warning, its end and the chip's next minute. Its callback, the
clock's minute tick and the load at start all go through `shutdown_check_clock`, which asks
`shutdown_step` (the pure step) what is due; the power off is spawned in one place, in the arm of
the `PowerOff` step, which only comes after a warning.

**The state file** is `$XDG_STATE_HOME/alpheus/timer` (owner only), one line such as
`countdown running <end> <total>` or `pomodoro paused <left> short 2`. It is written on each change
of state but never on a tick, and removed when the timer is idle. At start it is read as hostile
input (exact fields, digits only, every length bounded) and dropped, with one log line, if anything
is off. An end that passed while alpheus was not running is applied once, silently. `done` is never
saved. A scheduled shutdown adds a second line, `shutdown <end> <scheduled at>` (Unix milliseconds),
read by the same rules, with its end at most 25 hours ahead; a file without it still loads.

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
"The timer is drawn at rest" to "Logs carry fixed words and numbers only", and for the shutdown
"A scheduled shutdown always warns a minute before, even when its time has passed".

## Rules it keeps

These are enforced by tests, so a change that breaks one fails the build.

- `the_timer_is_wired_through_one_path` (`tests/guardrails.rs`): the deadline's `fired()` comes
  before the clock check; the clock's tick re-checks the timer; the state is loaded whenever
  `timer` is in `modules`; the model is assigned only in `timer_do`, `timer_check_clock` and
  `timer_load`; and `timer_do` goes through `change_step`.
- `the_shutdown_is_wired_through_one_path`: the shutdown deadline's `fired()` comes before the
  check; the clock's tick and the load both check the shutdown; the check goes through the pure step;
  one place powers off, in the `PowerOff` arm, after the warning's; a warning moves the end a minute
  away at least; every change is saved; a due warning may wait for the notification server, whose
  arrival re-runs the check; the boot time is read at load; and with no `poweroff` command a shutdown
  goes before any warning.
- `the_timer_has_no_panicking_path`: no `unwrap`, `expect` or `panic!` in the four files' production
  code (the state file is not ours, and the arithmetic is on wall-clock times).
- `the_tray_spawns_no_thread`: `timer/mod.rs`, `timer/popup.rs` and `timer/shutdown.rs` start no
  thread, process or command. The list it reads is itself checked: an entry that names no file fails
  the test.
- `nothing_resident_while_the_launcher_is_closed`: two `DeadlineTimer`s are built in `timer/app.rs`
  (the timer's and the shutdown's), and no other timer.
- The shutdown's own tests: `the_warning_comes_sixty_seconds_before_the_end`,
  `a_past_shutdown_warns_first_then_powers_off`, `a_warning_holds_only_if_the_wall_clock_did_not_outrun_the_monotonic_one`
  (`shutdown/tests.rs`); `a_timer_change_never_touches_the_shutdown`,
  `the_chip_shows_the_shutdown_before_the_timer`, `an_old_state_file_loads_without_a_shutdown`
  (`tests.rs`); `the_shutdown_check_warns_then_powers_off_and_never_at_once`,
  `a_suspend_in_the_warning_minute_warns_again_and_never_powers_off_at_once`,
  `a_shutdown_from_before_this_boot_is_dropped`, `a_warning_waits_for_the_server_but_never_for_long`
  (`app/tests.rs`); and, headless, `shutdown_warns_then_runs_poweroff`,
  `a_past_shutdown_at_start_warns_with_a_bubble_before_any_power_off`,
  `a_shutdown_is_refused_without_a_poweroff_command` and `a_shutdown_and_a_timer_run_side_by_side`,
  where `[quick] poweroff` is a `touch` of a file in the test's own runtime folder.
- `the_timer_popup_closing_drops_its_tick_and_sees_the_end`: whatever closes the popup, its
  one-second tick goes and an end it showed counts as seen.

## What it costs

Measured in release, back to back with the version before it, 3 runs each (the full table is in
[measurements](../measurements.md); the tests that measure are named here).

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
- **A scheduled shutdown (on the branch, no base: new code):** no wake-up in 5 s away from the
  minute, 4 in 65 s (the chip's minute and the test bar's clock; bound 6), 3 release runs
  (`at_rest_no_wakeups_with_a_shutdown_scheduled`); no thread, and its state is one line and a chip
  text.
