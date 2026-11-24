# Decisions

This file lists decisions that are settled: things alpheus does a certain way on purpose, and
the reason. It is not a history of how alpheus was built. Code comments point here when a rule
would otherwise look arbitrary to someone reading the code.

Plans cited by name ("plan 1, Decisions written down 3") are in the tag `archive/design-history-2026-11`.
Sources cited as "CLAUDE.md, '<section>'" refer to the project history file kept in the same tag
(`git show archive/design-history-2026-11:CLAUDE.md`).

Entries are in the order they were written, the newest last. The wording is neutral: "decision",
not a person's name.

To add one, append an entry with this shape and point to it from the code that depends on it:

```
### <the decision, as a short sentence>

One or two sentences: what is decided, and why.

Date: <YYYY-MM-DD> · Source: <the spec section, plan decision or commit it comes from>
```

An entry may carry one more line, `Status: awaiting confirmation at the trial`. It means the
decision was taken while the feature was built, its source says it is to be confirmed once the
feature has been tried, and that has not been done yet; the line is removed when it has. An entry
with no `Status` line is settled.

Each feature's decisions are added when its code is brought to the house standard
(`docs/STYLE.md`), so the file does not yet cover every feature: the ones it covers have a page in
`docs/features/` (listed in [features/README.md](features/README.md)), and the older decisions of
the rest are still in `docs/specs/`; the plans they cite, and the history file CLAUDE.md, are in the tag `archive/design-history-2026-11`.

---

### The timer is drawn at rest

The `timer` module is in the default modules, and when nothing runs it still shows one dim
glyph. Without it the mouse would have nowhere to click to open the popup. `[timer] idle =
"hidden"` draws nothing at rest instead (the `alpheus-launch --timer popup` verb still opens the
popup).

Date: 2026-11-15 · Source: timer spec § 2; timer plan 1, decision 1

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### The timer counts wall-clock time

A timer keeps its end (or its start) as a Unix time in milliseconds, not as a count of ticks. So
time that passes while the laptop sleeps counts: a countdown rings when the laptop wakes, and a
stopwatch has counted the sleep. A clock set backwards can make a countdown's time left look
longer than the countdown itself, so the time left is cut to the total.

Date: 2026-11-15 · Source: timer spec § 1; timer plan 1, decision 5

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### One deadline, armed only while a timer runs

The timer uses one `DeadlineTimer` (a one-shot timer on the event loop), set to the next moment
anything about the timer changes: the end, or the next minute of the chip's text. An idle or
paused timer arms nothing, so the daemon sleeps. A one-second tick would wake the daemon 60 times
a minute for a chip that changes once.

Date: 2026-11-15 · Source: timer spec "Constraints" and "Approaches"; timer plan 1, decision 4

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### The clock's minute tick re-checks the timer

The deadline is a monotonic timer on the event loop, and monotonic timers do not run while the
laptop sleeps (timer spec § 1), so after a suspend it fires late. The clock module's tick is also
a timer on the loop; it is re-aimed at each wall-clock minute (at each second when the clock's
format shows seconds), so it is at most a minute away. It calls `timer_check_clock` as well, so an
end that passed during sleep is seen within a minute of waking.
A deadline that fires early only arms the next one.

Date: 2026-11-15 · Source: timer spec § 1; timer plan 1, decision 4; guardrail `the_timer_is_wired_through_one_path`

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### A late end is applied once

If the end of a Pomodoro phase passed long ago (a sleep, a stalled loop), it is applied once, and
the next phase starts at the moment the end is noticed, not at the old end. Otherwise one long
sleep would make several phases end one after another with a notification each.

Date: 2026-11-15 · Source: timer plan 1, decisions 5 and 6, Review Focus 1

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### A change applies a due end first

Before any change (a click, a key, a verb) the timer first applies an end that is already due. A
Pause or an Add after a resume therefore finds nothing to pause or extend, instead of pausing a
timer that has in fact already ended. `change_step` does this in one place, and `timer_do` must go
through it.

Date: 2026-11-15 · Source: timer plan 1, Review Focus 1; guardrail `the_timer_is_wired_through_one_path`

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### Under do-not-disturb the end goes to the history only

When do-not-disturb is on, a timer's end sends its notification to the server, which keeps it in
the history and counts it on the bell, but shows no bubble. The optional sound does not play. The
chip's `done` shows either way. The notification is sent only while alpheus's own server owns its
name, and is never kept for later: a late timer notice is noise.

Date: 2026-11-15 · Source: timer spec § 5; timer plan 1, decision 8

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### The 24-hour stopwatch stop is silent

A stopwatch stops itself, paused, after a day. That writes one log line and notifies nothing.

Date: 2026-11-15 · Source: timer plan 1, decisions 9 and 13

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### `done` is cleared by looking, and is never saved

After a countdown or a Pomodoro set ends, the chip reads `done` until someone looks at it:
opening the popup, a middle click, any `--timer` verb or starting a new timer clears it. An end
that the popup showed counts as seen when the popup closes. `done` is not written to the state
file, so it does not come back after a restart.

Date: 2026-11-15 · Source: timer spec §§ 2 and 6; timer plan 1, decision 7

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### The state file is hostile input

The timer's state file is read back at start as if someone else had written it: exact fields,
digits only, every length within its bounds, a running end at most 25 hours ahead, a round from 1
to 12. Anything else is dropped with one log line, and the timer starts idle. The code that reads
it never panics (guardrail `the_timer_has_no_panicking_path`).

Date: 2026-11-15 · Source: timer spec § 6; timer plan 1, decision 13

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### The popup lists what the state allows

The popup offers only the actions that make sense now: an idle timer offers to start one of the
three kinds, a countdown offers Pause or Resume, Add 1 min and Stop, a Pomodoro offers Skip
instead of Add, a stopwatch only Pause or Resume and Stop. The first action is selected when the
popup opens, and again whenever the list changes. It stays open after an action. Stop asks no
question and a new start replaces whatever runs.

Date: 2026-11-15 · Source: timer spec § 3; timer plan 1, decisions 10 and 11

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### The popup's one-second tick exists only while it is open

While the popup is open and a timer runs, the time in it counts each second, through the same one
deadline. When the popup closes, by any way, the tick goes with it (the popup's `closed` hook
re-sets the deadline). With the popup closed the chip changes at most once a minute.

Date: 2026-11-15 · Source: timer spec § 3 and "Constraints"; timer plan 1, decision 4; guardrail `the_timer_popup_closing_drops_its_tick_and_sees_the_end`

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### The verb opens the popup under the chip

`alpheus-launch --timer popup` opens the popup under the chip on the focused output's bar, or
under the bar's right end when the chip is hidden, taking the keyboard without a grab. A second
verb within 250 ms of the one that opened it is ignored, because some compositors fire a key bind
twice (Hyprland re-fires a press bind, CLAUDE.md).

Date: 2026-11-15 · Source: timer spec § 4; timer plan 1, decision 17; CLAUDE.md, "Quick settings, delivery 1" (the debounce reason)

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### Logs carry fixed words and numbers only

The timer's log lines are fixed words (`timer: paused`, `timer: countdown ended`) and numbers. They
never hold the sound's command, a path, or an error's own text, since any of these can name the
user. A failed start of the sound logs a kind word only (`not found`, `timeout`, `io`). The rule is
the daemon's, not the timer's: the power menu (`power: power off could not start (not found)`), the
resources popup's `Monitor…` row, the quick panel (`quick: <what> could not start (<kind word>)`,
the `what` word of the action, never the program) and the right click on a module (`action: could
not start (<kind word>)`) all log the entry's word and the executor error's kind word, never the
command, the program, the arguments or the error's own text. Every other line that used to print a
system error (`{e}`, `{e:?}`) logs `log::error_word` or `log::kind_word` instead: an OS message can
quote a path, and so can a library's. A source thread that panics logs `source down: panic` and
nothing the panic carried; the daemon-wide hook already logged its thread and place. The guardrail
`no_log_line_names_a_program_an_error_text_or_a_panic_payload` reads every production line and keeps
a short list of lines that format an error for a reader that is not the log (the launcher's box, a
client's reply).

Date: 2026-11-15 (extended 2026-11-24: the whole daemon) · Source: timer spec "Constraints"; timer plan 1, decision 18; power spec § 3 and plan 1, decision 10; resources popup spec "Constraints"

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": the timer part; every one is to be confirmed at the trial)

### A source that is down is not a state

A value that becomes unknown because its source went away is not taken for a state, and each
consumer answers in its own way:

- **Hooks:** a source going down (the system bus closing, NetworkManager or BlueZ leaving the bus,
  a missing reading) fires nothing and keeps the seed; the next real value is compared with it. Of
  the battery events, only `battery_charge` ignores `unknown`: `battery_level` still fires, and its
  `ALPHEUS_CHARGE` may read `unknown`.
- **The `failed` count:** the last known value is kept and the notice is not re-armed. Only a count
  that was never known is absent, and the `failed` module draws nothing then rather than a
  placeholder.

The bar's other modules do not keep their value: the network, Bluetooth and battery modules show
`–` while their bus is out.

Date: 2026-11-15 · Source: hooks spec § 2; hooks plan 1, decision 6; diagnostics spec § 6 and "Decisions written down" 13

### The failed-units count is read on triggers, and `Subscribe` is off by default

systemd sends the Manager's `PropertiesChanged` only while some client has called `Subscribe()`, so
without it no signal about the count arrives. `[failed] subscribe = true` makes the icon instant,
and its cost lands on systemd and dbus-daemon (a signal per unit change to every subscriber), not
on alpheus. The default is `false` until it is chosen after use. Without a subscription the count
is read on triggers: session start, resume, `alpheus-launch --failed-refresh`, and a read added to
another domain's wake-up at most once a minute. No timer is added for it.

Date: 2026-11-22 · Source: diagnostics spec §§ 6 and 9, and its "Decisions written down" 1 (final fix wave, the owner to choose at the trial); diagnostics plan 1, "Decisions written down" 1 said on, which that decision reverses

### The failed-units count is asked for one property, never the whole interface

The count is a `Get` of `NFailedUnits` on the Manager object, on the owner's unique name. A
`GetAll` of the Manager would also carry its environment, which may hold secrets, so the call
builder refuses any other interface and property pair, and the daemon never reads a unit name: it
counts, it does not list.

Date: 2026-11-21 · Source: diagnostics spec § 6; guardrails `the_user_manager_is_asked_for_one_property_and_subscribed_to` and `no_acting_systemd1_method_in_the_daemon`

### The failed-units notice waits for both managers

`[failed] notify` sends one notice when the total rises above the last total notified, and a total
of 0 re-arms it. The notice waits until the system manager and the user manager have each answered
once (a manager nobody will ask, with `user_units = false` or no session bus, is not waited for),
so the first notice carries the whole total and not half of it. Like the battery alerts, it counts
as given only while alpheus's own notification server is serving, and the latest total is offered
again when the server starts.

Date: 2026-11-21 · Source: diagnostics spec § 6 ("Notification")

### The bar has no ignore list for failed units

A unit that fails at every boot keeps `󰀦 1` on the bar until it is fixed or reset (the Diag tab's
`c`). An ignore list would need the daemon to list units and read their names, which it never
does (see "The failed-units count is asked for one property, never the whole interface").

Date: 2026-11-22 · Source: diagnostics spec "Decisions written down" 9

### A hook fires on a change, never for the state at start

Each event's first observed value is a seed and never fires, and for the first 3 seconds after the
daemon starts (`QUIET`) any change only moves the seed, so that the state at start never fires a
hook. The window covers the portal's first answer, the first PipeWire graph, a camera held across a
restart and a profile applied at start. `started` is the one event that ignores the window. The
window is a timestamp compared when an edge arrives, never a timer.

Date: 2026-11-15 · Source: hooks spec § 2 ("Edges only") and "Constraints"; hooks plan 1, decision 5

### A hook's data reaches it through the environment, never the arguments

The event's data is given as `ALPHEUS_*` environment variables. No placeholder is expanded in the
command, and `~` and `$VAR` are not expanded either (a hook that wants a shell is `["sh", "-c",
"…"]`). Placeholders such as `{device}` were rejected as an injection surface: a device or profile
name comes from outside and must never become an argument or a shell word. The names handed over
are cleaned and cut to 255 bytes, and no hook log line ever holds a command, a variable or a name.

Date: 2026-11-15 · Source: hooks spec §§ 3 and 5 and "Approaches"; hooks plan 1, decisions 2, 3 and 4

### Each hook has a token bucket

Per event, 5 runs may happen at once, then one more every 3 seconds. A run without a token is
dropped, not queued, and the first drop of an episode logs one `throttled` line. A source that
flaps (a dock plugging in a loop, a radio bouncing) therefore costs at most that. The bucket is a
timestamp compared when the event arrives, not a timer.

Date: 2026-11-15 · Source: hooks spec § 4; hooks plan 1, decision 8

### A hook never adds a source

Events come from what the configured modules already read. An event whose module is not in
`modules` never fires, and the daemon says so once at start (`hooks: <event> never fires (needs
<modules>)`). A hook on `bluetooth` with no Bluetooth module would otherwise make the daemon read
BlueZ: a cost hidden behind a hook.

Date: 2026-11-15 · Source: hooks spec § 2 ("A hook never adds a source") and "Approaches"; hooks plan 1, decision 9

### A hook runs in its own systemd scope

A hook is started like the launcher starts an app: in `systemd-run --user --scope --quiet
--collect --` when a user manager runs, and detached on its own otherwise, or when `systemd-run`
is missing. The hook therefore never dies with the bar, and a restart of the bar never kills it.
The choice is made once at start. alpheus never waits for a hook, reads its output or kills it.

Date: 2026-11-15 · Source: hooks spec § 3; hooks plan 1, decision 11

### The calendar is written in English, like the clock

The month title (`October 2026`) and the weekday initials (`Mo Tu We`) are in English, as the
clock's text is.

Date: 2026-10-05 · Source: tray plan 2, the first decisions list (formerly "Questions"), item 3

### The calendar's first day comes from the locale, read at each open

The first day of the week is the locale's `LC_TIME` setting. The `C` and `POSIX` locales, or a
locale that cannot be read, give Monday (ISO 8601). The locale is read each time the calendar
opens, through `newlocale`, `nl_langinfo_l` and `freelocale`: this is thread-safe, changes no
global locale, and nothing is kept while the calendar is closed.

Date: 2026-10-05 · Source: tray plan 2, the first decisions list (formerly "Questions"), item 4 and "Controller decisions" 11; tray spec "Calendar"

### Calendar week numbers are ISO 8601

When `[clock] week_numbers` is on, the numbers are ISO 8601 weeks. In a calendar whose week starts
on Sunday, a row shows the ISO week of its Monday.

Date: 2026-10-05 · Source: tray plan 2, the first decisions list (formerly "Questions"), item 5 and "Controller reconciliation"

### The power menu runs the `[quick]` commands and has no setting of its own

The ⏻ menu runs `[quick]`'s `lock`, `logout`, `reboot` and `poweroff`, the commands the quick
panel's session buttons run, so one setting serves both. A `[power]` table would only repeat them.
An entry whose command is empty is not shown, and with all four empty the menu and its button do not
exist. alpheus never handles sleep, so there is no suspend or hibernate entry.

Date: 2026-10-13 · Source: power spec §§ 1, 3 and 6

### Reboot and power off ask first, and a second activation confirms from 0.4 s to 3 s

Lock and Log out run at once. Reboot and Power off turn their row into `Reboot? ⏎` or
`Power off? ⏎` in the warning colour; a second activation runs the command only from 0.4 s after the
question appeared and until 3 s after it, so a double click or a double Enter never confirms. Moving
the selection, hovering or pressing another entry, any other key, or 3 s passing cancels the
question. The rule is two functions, `session::activate` and `session::expire`, shared with the
quick panel, so the panel and the menu cannot drift apart.

Date: 2026-10-13 · Source: power spec § 2, § 4; quick settings spec § 2 (the 0.4 s rule); power plan 1, decisions 1 and 5

Status: awaiting confirmation at the trial for one part, any other key cancelling the question (power plan 1, "Decisions written down", decision 5, listed for confirmation in the plan's closing note); the 0.4 s to 3 s rule is settled

### Nothing is selected when the power menu opens

As in a tray menu, no entry is selected at open: ↓ or Tab selects the first, ↑ or Shift+Tab the
last, and Enter with nothing selected does nothing; this is also how the spec's own test reads
("Down and Enter on Lock"). The selection is clamped and never wraps, as in every alpheus list.

Date: 2026-10-13 · Source: power plan 1, decisions 3 and 4

Status: awaiting confirmation at the trial (power plan 1, "Decisions written down", decision 3, listed for confirmation in the plan's closing note)

### A held key repeats only to move, in the power menu

The keyboard's auto-repeat moves the selection but never activates: a held Enter acts once. Without
this the repeat would arrive inside the 0.4 s to 3 s window and confirm the question that the
first Enter had just asked. The wheel and the right and middle buttons do nothing in the menu.

Date: 2026-10-13 · Source: power plan 1, decisions 6 and 7

Status: awaiting confirmation at the trial (power plan 1, "Decisions written down", decision 6, listed for confirmation in the plan's closing note)

### The power menu has its own timer

The question's 3 s expiry uses a `DeadlineTimer` of the menu's own, armed only while a question is
pending and released whatever closes the menu. The quick panel's timer is not reused, because that
one may still serve a followed Wi-Fi attempt after its panel has closed.

Date: 2026-10-13 · Source: power plan 1, decision 12

### `alpheus-launch --power` opens the menu without a grab

The verb opens the menu under the bar's right end on the focused output and takes the keyboard
without a grab: a verb has no pointer press to grab with, exactly as `--quick` does. It works
whether or not the `power` module is in `modules`, and is refused (exit 2) only when no `[quick]`
session command is set or no bar can anchor the menu; both are decided before an open launcher is
closed. A second verb closes the menu, except within 250 ms of its own open (some compositors fire a key bind twice:
Hyprland re-fires a press bind, CLAUDE.md).

Date: 2026-11-21 · Source: tools menu spec § 7; tools menu plan 1, `power_verb`; CLAUDE.md, "Quick settings, delivery 1" (the debounce reason)

### The resources history is kept always, as whole samples

While the `resources` module is on, the bar keeps a ring of the last 60 samples (about 3 KB, written
in place on the existing tick, with no allocation after start), whether or not the popup is open.
The popup therefore opens on a graph that is already full. A ring filled only while the popup is
open would save those 3 KB but open on an empty graph that takes a minute or two to say anything.
Every pass is stored, including one with no reading, so a missing reading is a gap in the graph and
the time axis never shifts.

Date: 2026-11-15 · Source: resources popup spec § 3; resources popup plan 1, decisions 1 and 3

Status: awaiting confirmation at the trial (resources popup plan 1, "Decisions written down": confirm them at the trial)

### The resources popup spans 60 times the interval

The popup's samples come at the same cadence, `[resources] interval`, whether it is open or closed,
so its 60 samples span `60 × interval` seconds and the header reads `last {interval} min` (two
minutes at the default of 2 s, one at `interval = 1`). A faster tick while the popup is open would
mix two spacings in one ring and need the timer re-armed on every open and close.

Date: 2026-11-15 · Source: resources popup spec § 3; resources popup plan 1, decision 2

Status: awaiting confirmation at the trial (resources popup plan 1, "Decisions written down": confirm them at the trial)

### Every resources item keeps its block in the popup

Each item of `[resources] show` has its block in the popup, in the order written, whether or not it
has a figure at that moment (a dash and an empty graph stand in), so the popup keeps its size while
it is open and a reading that appears or goes never resizes it.

Date: 2026-11-15 · Source: resources popup spec § 2; resources popup plan 1, decision 12

Status: awaiting confirmation at the trial (resources popup plan 1, "Decisions written down": confirm them at the trial)

### Each resources graph has a set scale; the network's follows its peak, with a floor

CPU and disk graphs run from 0 to 100 %, memory from 0 to the machine's total, temperature and GPU
from 0 to 100 °C (clamped). The network graph shares one scale between download and upload, the
highest rate of either in the ring, never below 128 KiB/s, so that an idle link does not fill the
graph with noise. A point is in permille of the scale, rounded down, at most 1000.

Date: 2026-11-15 · Source: resources popup spec § 4; resources popup plan 1, decision 4

Status: awaiting confirmation at the trial (resources popup plan 1, "Decisions written down": confirm them at the trial)

### The resources popup has no timer of its own

The popup repaints on each tick of the bar's resources timer, and only while it is the open popup;
a tick with the popup closed paints nothing. It owns no timer, so opening it adds no wake-up, and
the reading is applied to the bar before the popup is repainted.

Date: 2026-11-15 · Source: resources popup spec § 2 and "Constraints"; resources popup plan 1, decision 12

Status: awaiting confirmation at the trial (resources popup plan 1, "Decisions written down": confirm them at the trial)

### The left click opens the resources popup and the right click runs `[actions] resources`

A left click on the resources module opens the popup, which has a `Monitor…` row that runs
`[actions] resources`; the right click runs the same command directly. It was the left click's job
before the popup existed. With `[actions] resources` empty the popup has no `Monitor…` row.

Date: 2026-11-15 · Source: resources popup spec § 1; resources popup plan 1, decision 10

Status: awaiting confirmation at the trial (resources popup plan 1, "Decisions written down": confirm them at the trial)

### The active media player is the pinned one, else the best status

A player chosen in the media popup stays the active one while it exists and is not stopped.
Otherwise the active player is the one with the best status (playing over paused; a stopped player
is never shown), and a tie goes to the player whose status changed last. Players the `ignore` list
names are never queried, so they never count. (The reason for the order is not recorded.)

Date: 2026-10-08 · Source: media spec § 3 ("Active player"); media plan 1, Task 1, "Decisions written down", item "The active player"

### One connection owning two MPRIS names keeps the first

Players are matched to signals by the unique name of their connection, because a signal carries
only that. So when one connection owns two MPRIS names, only the first is followed, and the second
is refused like a name already known. A 17th player is refused too.

Date: 2026-10-08 · Source: media plan 1, Task 1, "Decisions written down", item "`add`"; `Players::add`

### The media popup computes the position, and reads it only while open

MPRIS does not signal the playback position. The popup works it out as the last position read, plus
the time since, times the rate, while the player plays, and cuts it to the track's length. The
position is read with one call when the popup or the notification panel opens, on `Seeked`, and
when the status, the track or the rate changes while one is open; closed, it stays unknown. The
`Seeked` rule is on the bus only while one is open, and the 1 s redraw timer exists only while one
is shown and the active player plays: no polling, no wake-up at rest.

Date: 2026-10-08 · Source: media spec § 2 ("Time"), § 3 ("Seeked") and "Goal"; media plan 1, "Controller reconciliation", item 5

### A player is labelled by its identity, else by its name

The popup's list, and the bar's last fallback for a title, name a player by its `Identity` when it
has one, and otherwise by the first label after `org.mpris.MediaPlayer2.` (`vlc` for
`org.mpris.MediaPlayer2.vlc.instance42`). (The reason is not recorded.)

Date: 2026-10-08 · Source: media spec § 2 (amended 2026-10-08); media plan 1, "Controller reconciliation", the list of confirmed choices (fourth bullet)

### A paused player's module is fully dim

On the bar, `▶ Title` takes the module's purple (as `module_colors` says) while the player plays;
`⏸ Title` is dim as a whole, whatever `module_colors` says. A state wins over the hue, as it does
for the other modules.

Date: 2026-10-08 · Source: media spec § 1 ("Content"); media plan 1, Task 2, "Decisions written down", item "Paused", and the list of confirmed choices after item 9 in "Controller reconciliation" (first bullet)

### Media titles are cut by display width, never inside a cluster

The title is cut to `title_max` cells and ends with `…`. Width is counted per character as the grid
counts it, after putting the text in NFC form, and a cut never falls inside a cluster (a combining
mark, a variation selector or a skin tone after its base, a ZWJ sequence, a flag's two regional
indicators). It is written by hand on the `unicode-width` crate the daemon already uses, so that no
dependency is added.

Date: 2026-10-08 · Source: media spec § 1 (amended 2026-10-08); media plan 1, Task 1, "Decisions written down", item "`truncate_title`"

### The microphone pill shows only when its mute flips

The OSD shows the default source only when its mute state flips, with the level the source has
then. A change of its level never shows: WebRTC apps move the capture gain during calls, and the
pill would pop all through one. The sound source publishes no microphone level change alone, so a
call wakes nothing. A message that changes both the sink and the microphone shows the sink.

Date: 2026-10-09 · Source: OSD spec § 1 and § 2 (amendments of 2026-10-09)

### A sound change within a second of a start or a new default device shows nothing

The first state after `pw-dump` (re)starts is a baseline and never shows. Each stream (sink,
source) has its own 1 s grace, started by that baseline and by its default appearing, going or being
switched: a change inside it shows nothing, since the levels of a new default settle in silence
(WirePlumber restores them). The end of the second counts as outside it. While the sink's name card
is on screen, a change inside it updates the card. The switch itself shows the card, unless an
earlier grace is running; a card waiting for a volume that arrives inside the second shows then.

Date: 2026-10-09 · Source: OSD spec § 2 (and its amendment of the final review); OSD plan 1, "Decisions written down", item 2; `side` in `osd/mod.rs`

### The OSD counts a volume change in whole percent

A change is worth a sink pill when the mute flipped or the integer percent differs. The sound model
publishes whole percents (as wpctl and the bar show them), so a level that moved by less than half
a point never reaches the OSD. Carrying tenths would publish, and wake the loop, for changes nobody
can see.

Date: 2026-10-09 · Source: OSD plan 1, "Decisions written down", item 1

### Brightness is shown on request, read once

The brightness pill comes only from `alpheus-launch --osd brightness`, which reads the backlight's
`brightness` and `max_brightness` once, then shows the pill. Nothing watches the backlight and
the OSD never writes it (the quick panel's slider changes it, through `brightnessctl`): the user's
brightness key keeps changing it the same way and then calls the verb. A watcher would pop the pill each time hypridle dims and restores the
screen.

Date: 2026-10-09 · Source: OSD spec § 2 ("Brightness is on request")

### The OSD pill is an Overlay surface with an empty input region, made on show

The pill is one layer surface on the Overlay layer (Hyprland draws the Top layer under a fullscreen
window), anchored at the bottom with an exclusive zone of 0, taking no keyboard. Its input region is
empty, so it never takes a click; if the empty region cannot be made, nothing is shown, since a
surface without one would take every click. It is created when a pill is shown and dropped when it
goes, with its own pool, so that nothing of it stays at rest.

Date: 2026-10-09 · Source: OSD spec § 3 and "Goal"; OSD plan 1, Task 3, "Decisions written down", item "The surface"

### The pill follows the focused output only when its content changes

A pill is created on the focused output. While it is shown it moves to a newly focused output only
when its content changes, as the notification bubbles do (`bubbles::moves`); a change of focus alone
leaves it where it is.

Date: 2026-10-09 · Source: OSD plan 1, "Decisions written down", item 8; Task 3, "Decisions written down", item "The output"

### The OSD logs once when its surface appears and once when it goes

One log line when the surface appears (with what it shows) and one when it goes (with how many
changes it showed), never one per change: a held key would write 25 lines a second to the journal.
No line names a device (a card's label stays out of the journal).

Date: 2026-10-09 · Source: OSD plan 1, "Decisions written down", item 7; `describe` in `osd/mod.rs`

### A tray menu is shown only once its layout has arrived

Pressing an icon asks for its menu, but nothing is drawn until the menu's first layout is back, and
if it takes more than 2 s nothing is drawn at all. A half-built menu is never shown.

Date: 2026-10-05 · Source: tray plan 2, the first decisions list (formerly "Questions"), item 8; tray spec § 1 ("Menus": no answer within 2 s)

### A tray menu opens with its first entry selected

The first selectable entry is selected before the popup is first drawn, so Enter or a key press has
something to act on at once.

Date: 2026-10-05 · Source: tray plan 2, "Controller reconciliation", the bullet on decisions added after the skeleton

### A submenu opens in the same popup

A submenu replaces the menu's current level inside the same popup (→ or Enter enters it, ← goes back
one level), with the first row showing `◂ <parent label>`. No cascading popups are opened.

Date: 2026-10-05 · Source: tray plan 2, the first decisions list (formerly "Questions"), item 2

### A failed Activate opens the menu of the icon that was pressed

A left click sends `Activate`; when the item answers with a D-Bus error (Steam's libappindicator has
no such method) the bar opens its menu instead, if the item has a `Menu`, else nothing. That answer comes later, so the bar remembers the icon,
the cells it occupies and the button that pressed it, and the menu belongs to that press, not to
whatever is under the pointer when the answer arrives. A press that lands on no icon forgets the
remembered one, so a late answer never opens a menu for an icon the user has left. A timeout is not a
failure, because a slow application may have acted.

Date: 2026-10-05 · Source: tray spec § 2 (left click row); tray plan 2, "Controller decisions", item 5; commit 40be9a6 (the last icon cleared by a press on no icon)

### A tray menu is closed by the button that opened it, or by any right click

A press on the icon whose menu is open only closes that menu, and does nothing else, when it is made
with the button that opened it, or with the right button whatever opened it. So Steam's menu, opened
by a left click, closes with the same left click, with no new Activate and no flicker.

Date: 2026-10-06 · Source: commit 40be9a6 (the 2026-10-06 ruling quoted in its message); tray plan 2, "Controller reconciliation", the bullet on decisions added after the skeleton (a second right click on the icon whose menu is open closes it)

### A press right after the compositor closed a menu only closes it

A compositor may deliver `xdg_popup.done` before the press that follows it reaches the bar. For 300 ms
after such a close, a press on the same icon that would have closed the menu (its opening button, or a
right click) still only closes: it does not reopen what was just closed.

Date: 2026-10-06 · Source: commit 40be9a6 (`recently_closed_menu`, `closed_recently`)

### A late submenu graft is refused once the selection has moved

A submenu's children are fetched when it is opened, and entered when they arrive. If by then the
user has moved the selection, gone back, asked for another submenu or run an entry, the fetch is
dropped. Steam and Electron send `LayoutUpdated` often enough that a late graft could otherwise jump
into "More" long after the user looked elsewhere.

Date: 2026-10-06 · Source: commit 40be9a6 (`submenu_wanted` cleared, `submenu_still_selected`); comments in `tray/menu_app.rs`

### A burst of tray changes is redrawn once per frame callback

A signal such as `NewIcon` only marks a property dirty. The bar asks for one frame callback on the
first bar that can take one, and re-reads every dirty property once when it comes. The request is
committed with one pixel of buffer damage on every compositor, because an undamaged commit may get no
frame callback at all (Hyprland) or one throttled to about a second (niri).

Date: 2026-10-05 · Source: tray spec § 1 ("Bursts"); tray plan 2, "Controller decisions", item 10

### A closed session bus is never reconnected

When the session bus closes, the tray is dropped and left empty, one line is logged, and no
reconnection is tried: a retry loop would wake the bar at rest. The media and notification
consumers that live on the tray's connection go with it.

Date: 2026-10-05 · Source: tray spec § 3 ("No session bus": no retry loop); tray plan 2, "Controller decisions", item 12; `tray_output` (the consumers' log lines)

### The microphone lights for a recording stream, never a meter or a filter

The microphone indicator lights for a link from an audio source to a `Stream/Input/Audio` where the
link is active or the stream runs. It is not lit for a stream that is a meter (`stream.monitor`, such as
pavucontrol's), one that reads a sink's monitor (`stream.capture.sink`, the visualisers), a member of
a `node.link-group` (filter chains, loopbacks and echo cancellation, which forward sound and do not
record) or a passive stream.

Date: 2026-10-16 · Source: privacy spec § 2 ("The rules", Microphone)

### A muted app stream still lights the microphone

The model has no per-stream mute: a stream that is linked and running holds the device, so the
indicator stays lit even when the app muted its own stream, since the app still holds the
microphone. Only the default source's mute changes how it is drawn: the glyph is dim, and so is the
popup's heading.

Date: 2026-10-16 · Source: privacy spec § 1 ("Muted microphone"); privacy plan 1, "Review Focus", item 2; privacy plan 1, "Decisions written down", item 1; CLAUDE.md, "Privacy indicators, delivery 1" (the dim glyph also in the popup's heading); the note "The model has no per-stream mute" in `an_app_muted_stream_still_lights_the_microphone` (`privacy/tests.rs`)

Status: awaiting confirmation at the trial (privacy plan 1, "Decisions written down": handed over to be read over at the trial, see the trial note)

### A running camera with no link lights it unnamed

When a device video source (a v4l2 or libcamera device, or a source with the Camera role) is running
and no link from it is seen, the camera indicator is lit with no app name. The same holds for a
running screen producer, as long as no link has named a screen holder.

Date: 2026-10-16 · Source: privacy spec § 2 ("The rules", Camera, "GNOME's rule"); the screen case is `capture` in `privacy/mod.rs`

### An indicator going off stays shown for half a second

An indicator that is no longer live stays shown for 0.5 s, and one that comes back within that time
never blinked: a stream that renegotiates (a call changing codec, a share switching window) drops
for a few batches. Lighting up is immediate. One deadline timer, armed only while an indicator is
pending off, serves every indicator.

Date: 2026-10-16 · Source: privacy spec § 4 (the figure is GNOME's)

### A compositor's screencast release counts after a one-second hold

Hyprland sends a release about 500 ms after the last frame of a cast, and a screenshot gives a short
blip, so its release counts only after a hold of 1 s, after which the 0.5 s off-delay applies (1.5 s
in all). niri's hold is zero. A cast that comes back during the hold never blinks the screen.

Date: 2026-10-16 · Source: privacy spec § 3 (Hyprland); privacy plan 1, "Decisions written down", item 4

Status: awaiting confirmation at the trial (privacy plan 1, "Decisions written down": handed over to be read over at the trial, see the trial note)

### A screen lit by the compositor alone reads `(unnamed)`

The compositor's cast events say that a screen is captured and what is captured, never who captures
it. The screen indicator is lit when PipeWire or the compositor says so, and the popup's names come
from PipeWire; a screen lit by the compositor alone shows `(unnamed)`.

Date: 2026-10-16 · Source: privacy spec § 3 (amended 2026-10-16 at planning)

### The privacy popup lists apps and selects nothing

The popup has one heading per lit indicator and one row per app, and no row can be selected: Escape
closes it and every other key, the wheel and a click inside do nothing. While open it follows what is
shown, and it closes (`nothing captured`) once nothing is shown, after the off-delay. It is at most 32
cells wide; a longer name is cut.

Date: 2026-10-16 · Source: privacy spec § 5; privacy plan 1, "Decisions written down", items 5 and 6

Status: awaiting confirmation at the trial (privacy plan 1, "Decisions written down": handed over to be read over at the trial, see the trial note)

### The camera is read from `/proc` after a trigger, never counted

An open or a close of a `/dev/video*` node, a node appearing or going, or a lost event queue only
means "look again". What is shown comes from one scan of `/proc`, because a counter of opens drifts
(descriptors inherited by `fork` or `dup`, opens made before the daemon started, an overflowing event
queue). The scan reads the links of `<pid>/fd` of the daemon's own user, never opens anything under
`/dev`, and names a holder by its `comm`.

Date: 2026-10-20 · Source: privacy camera spec §§ 1 and 2

### A burst of camera triggers is scanned at most one second after its first

A trigger moves the scan 300 ms ahead, so that an open and its close (a browser listing the cameras)
scan once and find nothing. The scan still runs at the latest 1 s after the first trigger of the
burst, so that a process that opens and closes a node non-stop cannot postpone it for ever.

Date: 2026-10-20 · Source: privacy camera spec § 1 ("The debounce"; the 1 s cap, `SCAN_MAX_WAIT`, was amended at execution); privacy plan 2, "Decisions written down", item 3 (the 300 ms push-back only)

Status: awaiting confirmation at the trial (privacy plan 2, "Decisions written down": handed over to be read over at the trial, see the trial note)

### PipeWire's own hold on a camera adds no name

A holder whose `comm` is `pipewire` or `wireplumber`, and the daemon itself, are left out of the
scan. PipeWire's hold on the device is already named by the client that reads the camera through it,
which delivery 1's graph names.

Date: 2026-10-20 · Source: privacy camera spec § 2; privacy plan 2, "Global Constraints" (the dropped holders)

### A video node udev has not opened to the user yet is watched again when its attributes change

A node udev has just created may belong to root alone, so watching it is refused. The refusal is
kept, and the watch is tried again when the name's attributes change (the group and ACL arriving), or
when it is created again, and forgotten when the name goes. A refusal because of a permission is not
logged.

Date: 2026-10-20 · Source: privacy camera spec § 1; privacy plan 2, "Decisions written down", item 5, and "Review Focus", item 1; `CameraWatch::watch_node` (the permission refusals not logged)

Status: awaiting confirmation at the trial (privacy plan 2, "Decisions written down": handed over to be read over at the trial, see the trial note)

### The camera scan runs on the event loop

The scan is synchronous on the bar's event loop and not on a thread of its own. The spec's 10 ms
rule is judged on a harness of 120 processes of 30 descriptors (the shape of the workstation's own
user: 93 processes, 2,726 descriptors), where the median is 4.0 to 4.1 ms; beyond 10 ms the spec
would have moved the scan to a short-lived thread. A 500 by 20 tree, printed only, reads a median of
14 to 15 ms and a maximum near 20 ms (`camera_scan_duration`).

Date: 2026-10-20 · Source: privacy camera spec § 2 (the 10 ms rule); CLAUDE.md, "Privacy indicators, delivery 2" ("Scan cost", the controller's ruling)

### The camera watch is a second inotify instance

The watch on the video devices has its own inotify instance, apart from the theme watch's. This keeps
the theme watch untouched and the two lifetimes apart, at the cost of one file descriptor. Delivery
1's spec had planned to use the existing instance.

Date: 2026-10-20 · Source: privacy camera spec § 4 (amendment of delivery 1's § 10)

### An unknown Shield state is never read as off

The mode is on while the flag file exists, off when it does not, and unknown when its existence cannot
be told (any error but "not found"). Unknown is not off: the switch reads `·`, the bar draws nothing,
Space does nothing and `toggle` answers an error (exit 2), and the detail says `Shield state unknown` instead of `Shield is off`.
The spec gives the rule as "never "off" on a guess", the rule of `[quick]`'s `file:` states.

Date: 2026-11-21 · Source: shield spec § 3 and § 14, items 5 and 16

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### A Shield switch succeeds when the flag reaches its target

A switch to on or off is judged by the flag file, not by the command's exit code: it succeeds when the
flag reaches its target, before or after the command exits. A non-zero exit fails at once, after one
look at the flag; an exit 0 gives the flag 5 s; a command still running after 60 s fails the switch
(without being killed). The flag is the truth: a change that arrives late clears `failed` and is
announced like any change.

Date: 2026-11-21 · Source: shield spec § 7 and § 14, item 7

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### A running Shield command is never killed at its deadline

The 60 s deadline judges a switch and kills nothing. A command's output goes to `/dev/null` and it is
waited for with no bound of its own: a pipe that alpheus closed at a deadline or at shutdown would
kill a privileged helper on its next write, half way through its work (a setuid `pkexec` cannot be
signalled, but SIGPIPE reaches it). At shutdown the daemon stops only the commands it can still
reach; one already running as root finishes.

Date: 2026-11-21 · Source: shield spec § 7 and § 14, item 6 (final fix wave)

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### Shield commands and reads run on a short-lived thread

`on`, `off` and `allow` (through `run_silent`) and the listing of blocked devices (through `capture`)
each run on a thread that lives only while the job runs, and mail the outcome back. For the three
commands: a `failed` on a non-zero exit is wanted, and only a command that is waited for gives an exit
code; a detached spawn never sees one. For the listing: `capture` blocks up to its deadline, so it runs
off the event loop (the comment on `only_the_runners_capture_and_never_through_a_shell`).

Date: 2026-11-21 · Source: shield spec § 6, § 14, item 6; § 13 (the rejected detached spawns)

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### A Shield verb for the state already held runs nothing

`alpheus-launch --shield on` while the mode is on (and `off` while off) answers `Done` and runs no
command. Running the configured `on` again could repeat a real effect (the spec's example is
regenerating the USB policy), which a stray key should not cause.

Date: 2026-11-21 · Source: shield spec § 14, item 9; § 9

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### The picker line starts with the checked vendor and product ids

A blocked device is shown as `<vid:pid>  ·  <name>`, with no USBGuard id. The ids are checked text and
come first, so a hostile name ending in `(1234:5678)` cannot pose as them; the device id means nothing
to the user and stays inside the daemon. `status` prints the same line.

Date: 2026-11-21 · Source: shield spec § 14, item 13 (final fix wave)

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### The blocked USB devices are read when asked, never watched

The list is read once each time the detail opens, once after an allow, for `status` and for the
picker; never when a device is plugged, and nothing is read while the mode is off. Reopening the
detail is the refresh. The design rejects a notice on every plug: that is background watching, and
allowing stays a deliberate gesture.

Date: 2026-11-21 · Source: shield spec § 11, § 13 and § 14, items 12 and 15 (`status` reads the list only while the mode is on)

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### The Shield flag may not share a directory with alpheus's configuration

The flag's directory and its parent may equal neither the configuration directory nor its parent;
otherwise Shield mode is turned off with one warning at start (spec § 2; plan 1, item 8). The theme target and the flag target
would then share inotify watch descriptors (one inode, one descriptor), and re-arming one would drop
the other. An alias the paths do not show (a symlink, a bind mount) is caught when the watch arms: the
flag is then not watched and the unwatched mode applies.

Date: 2026-11-21 · Source: shield spec § 2 and § 14, item 3; shield plan 1, "Decisions written down", items 8 and 9

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### The Shield flag is watched in the theme watch's inotify instance

The flag's directory is a second target of the theme watch's one inotify instance, so Shield adds no
descriptor, thread or timer (the spec's wording is "no new fd, thread or timer"). The instance is
created when either target needs it, and one target failing to arm leaves the other working. This is
the opposite of the camera watch, which has an instance of its own.

Date: 2026-11-21 · Source: shield spec § 3 and § 14, item 4

Status: awaiting confirmation at the trial (shield spec § 14: its decisions are confirmed at the trial)

### The displays are changed by the daemon, and its trial lives there

The resident daemon, not the control center, speaks the compositor's output protocol and holds the
15 s "keep these settings?" trial. The hotplug behaviour needs the daemon resident anyway, the control
center must stay free of Wayland (a boundaries test), and a control center that dies mid-trial can no
longer leave a broken layout, because the daemon reverts at the deadline.

Date: 2026-10-30 · Source: displays protocol spec, "Designer's calls", item 1

### displays.toml is read at each evaluation of the profiles, never watched

The daemon reads `displays.toml` each time the controller evaluates the profiles (`displays_tick`): when
the debounce that follows a change of screens ends (the first one follows the first `done` at start), and
when the re-check of a profile skipped by the 3-a-minute limit comes due. It does not watch the file and
does not keep it between evaluations (reason not recorded).

Date: 2026-10-30 · Source: displays protocol spec § 1, "The daemon applies profiles on hotplug"

### A DisplayLink screen is told from the DRM directory, once per screen name

A screen is DisplayLink when its connector's DRM card is driven by `evdi`: the `card*-<name>` entry of
`[displays] drm_dir` (default `/sys/class/drm`) whose driver link ends in `evdi`. The answer is read once
for each new screen name and never watched, and nothing under `/dev` is opened (`is_displaylink` reads
only `drm_dir`). The directory is configurable so that tests can point elsewhere (reason for reading once:
not recorded).

Date: 2026-10-30 · Source: displays protocol spec § 1, "DisplayLink detection" (with its amendment at the final review); the code (`is_displaylink`)

### A screen's identity and a profile file's errors are never logged

A profile's name is the user's own word and may be logged (`displays: profile "<name>" does not resolve`);
a screen's identity is device data and never is, and an invalid `displays.toml` is logged as one fixed line
with no error text, because the error may quote a screen's identity.

Date: 2026-10-30 · Source: displays protocol spec § 1 ("Match": profile names may be logged, screen identities never are); the comment on `LOG_INVALID_FILE` in `outputs/app.rs`

### The Tools menu reaches the clipboard, the shield and the diagnostics tab through one file

The three features were built in parallel with the menu. `launcher/tools/join.rs` is the only file of the
menu that calls their own functions and types, with one function each for "does its row show" and "open
it" (the diagnostics tab is a constant). `app.rs` has a `Join` struct carrying the three answers and
reads that constant. The guardrail `the_parallel_branches_are_named_only_in_tools_join` looks in the
folder's other files for `clipboard_availability`, `launcher_open_clipboard`, `shield_open_detail`,
`ShieldConfig` and the `"diagnostics"` slug. All three are real since the merges.

Date: 2026-11-21 · Source: tools menu spec § 8 and "Decisions written down", item 19

Status: awaiting confirmation at the trial (tools menu spec, "Decisions written down": to be confirmed at the trial)

### A built-in replays the daemon's own verb after the box has closed

A built-in that opens a feature (the quick panel, Notes, Timer, the power menu) calls the daemon's own
verb internally, with no process and no socket round trip, and does so after the launcher's box has
closed, in the same pass (the reason for replaying after the close is not recorded). The verbs open
their popups with the keyboard taken as a socket verb would, which is why a held Enter does nothing in
the Tools mode: the repeat of the key that ran a built-in must not act in the popup it opened.

Date: 2026-11-21 · Source: tools menu spec, "Decisions written down", items 6 and 23; tools menu plan 1, "Decisions written down", item 5

Status: awaiting confirmation at the trial (tools menu spec and plan 1, "Decisions written down": to be confirmed at the trial)

### Every action of the Tools menu closes the box, toggles included

Running an entry, opening a built-in's feature and flipping a toggle all close the box; the switch is not
redrawn in place. Only opening a level, going up, a confirmation question and a missing program (said in
the box) keep it open. The reason is not recorded.

Date: 2026-11-21 · Source: tools menu spec, "Decisions written down", item 8

Status: awaiting confirmation at the trial (tools menu spec, "Decisions written down": to be confirmed at the trial)

### A tab opens through `[actions] open_tab`, else in the terminal

A built-in for a control center tab runs `[actions] open_tab` with `{tab}` replaced by the tab's name, when
its program is on the `$PATH` of the open. When it is not, it runs the launcher's terminal with
`alpheus --tab <name>`. When neither program is there, the box says `program not found` and stays open
(the launcher's own rule for a missing program, tools menu spec § 5). An empty `open_tab` never reaches
this point: `bar.toml` refuses it. Why the terminal is the fallback is not recorded.

Date: 2026-11-21 · Source: tools menu spec, "Decisions written down", item 5; tools menu plan 1, "Decisions written down", item 9

Status: awaiting confirmation at the trial (tools menu spec and plan 1, "Decisions written down": to be confirmed at the trial)

### Everything a built-in spawns runs in its own scope

A command spawned for a built-in (a tab, a toggle, Lock, Reload the bar) runs in its own
`systemd-run --user --scope`, bare when there is no systemd, and its program is looked up first. The plan's
reason is that `[tools] reload` (for example `shell-ctl reload-bar`) must survive the daemon's own control
group being killed.

Date: 2026-11-21 · Source: tools menu plan 1, "Decisions written down", item 10

Status: awaiting confirmation at the trial (tools menu plan 1, "Decisions written down": to be confirmed at the trial)

### The Tools menu reads states and choices one job at a time on a short-lived thread

The state of each entry and the lines of a `choices_cmd` are read on a thread of their own that lives only
during a read, one job at a time, shaped like the launcher's scan thread. The `--tools` open runs no apps
scan, so the reader is its own thread. States are delivered one entry at a time; a choices read that
a drill-in asks for goes first, the states read stops after its current entry and its unread entries wait
behind it; a delivery for a closed open still frees the slot.

Date: 2026-11-21 · Source: tools menu spec, "Decisions written down", items 9 and 22; tools menu plan 1, "Decisions written down", item 4

Status: awaiting confirmation at the trial (tools menu spec and plan 1, "Decisions written down": to be confirmed at the trial)

### The notification server queues behind another server and never replaces it

alpheus asks for the name `org.freedesktop.Notifications` with flags 0: it waits in the queue while
another server (mako, in the spec) owns the name, and it is neither allowed to replace that server nor
replaceable by one. While the name is not held, alpheus answers no call and shows no bubble; the bell
shows only the history it already has. The spec says that if alpheus exits, mako's D-Bus activation
brings mako back on the next `Notify`, and that this fallback is intended.

Date: 2026-10-09 · Source: notifications spec § 1, "Name"

### A bubble's timeout follows its urgency

A critical notification has no timeout (its bubble stays until clicked). For any other, an `expire_ms`
of -1 (or any negative number) means `[notifications] timeout` (5 s), 0 means `timeout_max` (20 s), and
any other value is clamped to `timeout_min..=timeout_max` (2 to 20 s). The reason for these numbers is
not recorded.

Date: 2026-10-09 · Source: notifications spec § 2, "Timeout"; notifications plan 1, Task 1, "Decisions written down" (Timeouts)

### A bubble is given by guards in a fixed order, and a critical one always shows

A notification that no mute rule matches gets a bubble when (a) it replaces a live bubble (also under
do-not-disturb), or (b) it is critical, or (c) do-not-disturb is off, fewer than 32 notifications are
live and its app's token bucket gives a token (10 tokens, one more per second). Under (c) the checks run
in that order, so that do-not-disturb and a full screen spend no token. A critical one always gets a
bubble: at the live cap of 32 the oldest bubble that is not critical goes to the history, closed as
expired, and at most `max_visible` × 2 critical bubbles stay live, the oldest going to the history past
that, so that a flood never hides a real one. A notification with no bubble is history only: it is not
live, has no timer, sends no signal, and is still answered with its id. A mute that replaces a live
bubble ends it, sending `NotificationClosed` with reason 1.

Date: 2026-10-09 · Source: notifications plan 1, Task 1, "Decisions written down" (Bubbles, the bucket and `MAX_LIVE`); notifications spec § 2, "Spam", and § 3, "Look"; commits 9e9cf74 (a replaced live bubble stays live under do-not-disturb), 49342ff (a critical one at the live cap), 53a68a8 (the cap on criticals)

### A bubble's timeout starts when it first shows

A bubble beyond `max_visible` waits with no timer and gets its whole timeout when it first shows. One
that the host has drawn keeps its expiry when newer bubbles push it back: a steady flow of notifications
used to push every shown bubble back before it expired, so nothing expired (commit 49342ff). Under
do-not-disturb a live bubble that is hidden still has its timer and closes on time. Notifications plan
1, Task 1 (Timeouts) had waiting bubbles count down too; commit 53a68a8 changed that, and the spec was
amended to match.

Date: 2026-10-09 · Source: notifications spec § 3, "Look" (amended) and "Do-not-disturb"; commits 53a68a8 and 49342ff

### An identical notification within one second is merged, unless it would add a bubble

With no `replaces_id` and no tag match, a notification with the same identity, summary, body and
urgency as an entry that arrived less than 1 s before is the same notification: its id is returned and
nothing else changes, and each merge slides the window (so an app looping on one text stays one entry).
A merge spends no token. It does not swallow a bubble the first one never got (do-not-disturb, an empty
bucket) when this one would get one.

Date: 2026-10-09 · Source: notifications spec § 2, "Spam" (same identity, summary and body); notifications plan 1, Task 1, "Decisions written down" (Merge); commit 9e9cf74 (the same urgency, and a merge never swallows a bubble)

### A notification's text is cleaned in one linear pass

Every raw field is first cut to 16 KiB, so the rest of a hostile body is never looked at, and markup is
stripped in one pass: a tag left unclosed is text and so is every `<` after it, so no second scan is
ever made. Notifications plan 1's list said `<br>` is not a line break; commit 9e9cf74 changed that
(test `a_br_with_spaces_or_attributes_is_a_line_break`). Only the summary and the body are stripped;
`<br>` is a line break in the body only (in the summary, a space).

Date: 2026-10-09 · Source: notifications plan 1, Task 1, "Decisions written down" (Cleaning, `strip_markup`); notifications spec § 2; commit 9e9cf74

### An unknown `replaces_id` is accepted as that id

A non-zero `replaces_id` that matches no entry is taken as the new notification's id, because Electron's
tag ids rely on it.

Date: 2026-10-09 · Source: notifications spec § 1, "Ids"; notifications plan 1, Task 1, "Decisions written down" (Ids)

### A rule is matched on the whole app name, any case

A `[[notifications.rules]]` entry matches when its `app` equals, in full and ignoring case, the sender's
app name or its desktop entry. `drop` answers the sender (with its `replaces_id` when non-zero, else a
fresh id) and stores nothing; `mute` with any urgency still means no bubble. Battery alerts go through
the same path, so a rule on `alpheus` applies to them.

Date: 2026-11-15 · Source: small features plan 1, "Decisions written down" 9

Status: awaiting confirmation at the trial (small features plan 1, "Decisions written down": to be confirmed at the trial)

### The do-not-disturb file holds one of three forms

`$XDG_STATE_HOME/alpheus/dnd` is empty (on, untimed), `until <unix seconds>` or `sharing`. Any file still
means "on" to the shell scripts that read only whether it exists. At start, an end already passed reads
as off, `sharing` reads as off (a capture still running turns it back on when evaluated again), and
contents that cannot be read read as on, because an existing file was always "on". When what was read
differs from what is saved, the file is rewritten or removed at once.

Date: 2026-11-15 · Source: small features plan 1, "Decisions written down" 6

Status: awaiting confirmation at the trial (small features plan 1, "Decisions written down": to be confirmed at the trial)

### The user's choice wins over a capture

Every user path clears "by sharing". The bell's middle click, the panel's row and the quick row's Space
set an untimed state; `--dnd on` on a timed state makes it untimed; `--dnd on --for`, `--dnd on --until`
and the quick panel's duration detail make it timed; `toggle` on a timed one turns it off.
`dnd_while_sharing` reacts to edges only (a capture starts, a capture ends) and only with the `privacy`
module. A timed end reached while a capture is shown and the option is on hands over to "by sharing", so
the capture's end turns it off as usual; this was settled at the final review of that delivery (it used
to turn off).

Date: 2026-11-15 · Source: small features plan 1, "Decisions written down" 7

Status: awaiting confirmation at the trial (small features plan 1, "Decisions written down": to be confirmed at the trial)

### A timed do-not-disturb ends on the wall clock

The end's timer is monotonic and does not count the time a machine is suspended, so the clock's minute
tick also checks the wall-clock end: after a resume, do-not-disturb ends at most one clock tick late. When
the timer fires and the wall clock says the end is not reached yet, the timer is armed again for the rest.

Date: 2026-11-15 · Source: small features plan 1, "Decisions written down" 8

Status: awaiting confirmation at the trial (small features plan 1, "Decisions written down": to be confirmed at the trial)

### A battery alert is given only while the server serves

An alert is sent only while alpheus's own server holds the name; before that nothing is logged and the
threshold stays armed, so the alert is offered again, with the last reading, when the server starts.

Date: 2026-11-15 · Source: small features plan 1, "Decisions written down" 10; `BatteryAlerts::feed`

Status: awaiting confirmation at the trial (small features plan 1, "Decisions written down": to be confirmed at the trial)

### The bubbles are an Overlay surface, opened again once per stack

The bubbles are one `Overlay` layer surface, not `Top`, because Hyprland draws `Top` under a fullscreen
window (spec § 3). When the compositor closes the surface it is opened again once for that stack of
bubbles (their ids and revisions); closed again for the same stack, it stays closed until the stack
changes, so there is never a create-and-close loop. A close followed in the same dispatch batch by an
output going is that output's loss (an unplug) and does not spend the one reopen (commit 4c1e923).

Date: 2026-10-09 · Source: notifications spec § 3, "Placement"; notifications plan 1, task table row 6 ("reopen once per stack"); commit 4c1e923

### The bubbles move to the focused output only when the stack changes

When the focus moves to another output, the bubbles stay where they are until the stack itself changes
(a bubble arrives, expires or is replaced); only then are they placed on the focused output, and only if
that output is known by name and is not the one they are on. The code gives the reason: a bubble must
never jump under the pointer because the focus followed the mouse.

Date: 2026-10-09 · Source: notifications plan 1, Task 6; `bubbles::moves`

### A slider sends one level per 150 ms window and keeps showing it

The volume and the brightness send absolute levels, at most one command per 150 ms window: a key, a
wheel notch or a click inside a window only moves the level shown, and the latest level goes when the
window ends. After a quiet window the level sent stays shown, and is stepped from, until pw-dump
reports it or 1 s has passed, because a slow `wpctl` or `brightnessctl` would otherwise make the next
press step from a stale level (commit 0a69573). A default sink switch drops a held or waiting volume
level: it is never sent to the new sink. Spec § 5 sets the pacing for a held key; why 150 ms is not
recorded.

Date: 2026-10-11 · Source: quick settings spec § 5; commits 9347a3c and 0a69573; `Pace`, `COALESCE`, `SETTLE`

### The quick panel never sets the backlight below 1 %

A slider move on the brightness row stops at 1 %, so the panel can never black the screen. The
guardrail on the commands the panel may run pins the same floor.

Date: 2026-10-11 · Source: quick settings spec § 5; commit 9347a3c; `BRIGHTNESS_MIN`

### A click on a slider's bar sets the level of the cell

The bar is 22 cells. A press on a cell sets `(column − bar start + 1) × 100 / 22` percent: the first
cell is 4 % and the last 100 %. Why the first cell is not 0 % is not recorded.

Date: 2026-10-11 · Source: quick settings plan 1, "Decisions written down" 10; `bar_value`

Status: awaiting confirmation at the trial (the plan's "Decisions written down" are to be confirmed at the trial, CLAUDE.md)

### The selected slider row is banded across the whole row

The selection band covers a slider's whole row, the bar included; the bar's cells keep their own
foreground colours on it (accent, dim, warning), so the level stays readable. A band that skipped the
bar looked broken (the plan's note on its amendment at the final review).

Date: 2026-10-11 · Source: quick settings plan 1, "Decisions written down" 6 (amended at the final review); `slider` in `draw.rs`

Status: awaiting confirmation at the trial (the plan's "Decisions written down" are to be confirmed at the trial, CLAUDE.md)

### Only the arrow keys repeat in the quick panel

Held down, only ↑ ↓ ← → (and Ctrl+P, Ctrl+N) repeat, and Backspace in the Wi-Fi password field. A held
Space or Enter acts once, so a held key never flaps a mute or confirms a reboot. Two later cases
follow from it: a held ← or → on the headset row sends nothing more, because it would send
`wpctl set-profile` again before pw-dump reports the switch, and a held key never answers a
Bluetooth code question, which a key pressed before the question arrived would otherwise reject.

Date: 2026-10-11 · Source: quick settings spec § 3; plan 1, "Decisions written down" 8 and "Review Focus" 5; commits 1f922b6 (Backspace), b627888 (headset) and 33da8c6 (code question); `Quick::key`

Status: the base rule (plan decision 8) awaits confirmation at the trial; the three exceptions came later

### The quick panel's outputs detail stays open after a choice

The outputs detail opens on the default sink and stays open after Enter: the `●` moves when pw-dump
reports the switch. Why it does not close is not recorded.

Date: 2026-10-11 · Source: quick settings plan 1, "Decisions written down" 5; `Quick::choose_sink`

Status: awaiting confirmation at the trial (the plan's "Decisions written down" are to be confirmed at the trial, CLAUDE.md)

### A session button closes the quick panel

A session button (lock, log out, and reboot or power off after the second activation) closes the panel
once its command is spawned. Why is not recorded; the plan's closing note lists it for confirmation.

Date: 2026-10-11 · Source: quick settings plan 1, "Decisions written down" 7; `Quick::activate`

Status: awaiting confirmation at the trial (quick settings plan 1, closing note: decision 7 listed for confirmation)

### Do not disturb shows in the quick panel only with the notifications module

The do-not-disturb row exists only when the `notifications` module runs: without it there is no bell,
no server and nothing to mute.

Date: 2026-10-11 · Source: quick settings plan 1, "Decisions written down" 4; `quick::live`

Status: awaiting confirmation at the trial (quick settings plan 1, closing note: decision 4 listed for confirmation)

### A verb-opened quick panel takes the keyboard without a grab

`alpheus-launch --quick` opens the panel as a popup without a grab, and the bar's layer surface goes
`Exclusive` while the panel is open, on every compositor. A popup grab needs the serial of a press,
and a key bound in the compositor gives the bar none. Esc, the verb again, and a click on the bar or
on a module close the panel; a click on a window does not, because nothing tells the bar about it. On
Hyprland older than 0.56.2 a click inside a verb-opened panel may reach the bar and close it
(hyprwm/Hyprland#15729). A panel opened by a click keeps its grab, like every other popup. A verb
that arrives within 250 ms of the verb's own open is ignored (`quick: toggle debounced`), because
some compositors fire a key bind twice (Hyprland re-fires a press bind, CLAUDE.md; the launcher's
`TOGGLE_DEBOUNCE`).

Date: 2026-10-11 · Source: quick settings spec § 1 (amended 2026-10-11); plan 1, "Decisions written down" 1; CLAUDE.md, "Quick settings, delivery 1" (the debounce and its reason); `quick_toggle`, `verb_debounced`

### The wheel moves the quick panel's window only when the lines do not fit

Over a slider, `n` wheel notches (at most 10 counted per event) send one command of `n × step` percent
(at most 100). Anywhere else
the wheel moves the panel's window over its lines only when they do not all fit under the bar;
otherwise it does nothing.

Date: 2026-10-11 · Source: quick settings plan 1, "Decisions written down" 9; `Quick::scroll`

Status: awaiting confirmation at the trial (the plan's "Decisions written down" are to be confirmed at the trial, CLAUDE.md)

### The quick panel reads the sink list through a shared tap

The sink list travels in a `SoundTap`, a `Mutex` around the pw-dump model shared by the sound thread
and the event loop. pw-dump is silent at rest, so a panel that is opening could not wait for the next
publication. The mutex is taken once per pw-dump batch and once per open. A sink list that a message
carries is moved into the open panel, and the bar never keeps one.

Date: 2026-10-11 · Source: quick settings plan 1, "Decisions written down" 15 and 16 (decision 15 listed for confirmation in the plan's closing note); `SoundTap`, `taken_sinks`

Status: awaiting confirmation at the trial (quick settings plan 1, closing note: decision 15 listed for confirmation)

### A Wi-Fi attempt on a new network outlives the quick panel

When the panel closes during an attempt on a network with no saved profile, the attempt is handed to
a follower (`WifiFollow`) that keeps the Wi-Fi read, and the panel's timer stays armed for it alone,
up to 20 s (60 s while the device is still activating). What a failed attempt then deletes is the
rule of "A failed Wi-Fi attempt deletes its new profile only when sure". A panel opened meanwhile
takes the attempt back. This replaces the Wi-Fi plan's rule that closing the panel abandons the attempt.

Date: 2026-10-11 (commit 750810e) · Source: commit 750810e; CLAUDE.md, "Quick settings, delivery 2" ("Connecting"); quick Wi-Fi plan 2, "Decisions written down" 10 (the sentence it replaces); `WifiFollow`, `quick_flush`

### The Wi-Fi password lives in one boxed buffer that is wiped

The password is typed into a fixed buffer of 256 bytes on the heap, so moving the panel never copies
it. The buffer is wiped with volatile writes when the field closes, after the password is handed to
`nmcli`, and on drop; it is not locked in memory (the launcher's `--password` is not either). The
password is written to `nmcli`'s standard input straight from the buffer, in parts, never joined into a
copy and never an argument. A character that would not fit is refused whole, and the field then refuses
to connect rather than send a password cut short. What is not wiped: the per-key heap string that SCTK
(the Wayland toolkit) creates for each typed key, and the `Key::Char` on the stack that carries
each character to the panel.

Date: 2026-10-11 · Source: quick Wi-Fi plan 2, "Decisions written down" 14 and 15; quick Wi-Fi spec § 3; CLAUDE.md, "Quick settings, delivery 2" (the per-key string, "decision 14"); `Secret`

Status: awaiting confirmation at the trial (Wi-Fi plan 2's closing report lists decision 14; that `nmcli --ask` reads the password from a pipe is unverified until the trial, CLAUDE.md)

### An SSID that nmcli would misread is never passed to it

`nmcli device wifi connect <ssid>` takes the SSID as an argument, so the panel refuses one it cannot
pass as it is: empty, holding a NUL, starting with `-` (read as an option), not UTF-8 (an argument is
a string), `h`, `he`, `hel` or `help`, or six groups of hexadecimal digits (`refused`: nmcli reads
those as the word help and as a BSSID; commit 750810e). Enter on such a network moves the selection to
`More…`, as it does on any network the panel does not handle (it handles only open and WPA-personal
networks: spec §§ 2 and 8). A saved or connected network is reached by its uuid, whatever its SSID.

Date: 2026-10-11 · Source: quick Wi-Fi spec § 5 (the SSID is one argument); plan 2, "Decisions written down" 7; commit 750810e; `refused`, `Wifi::kind`

Status: awaiting confirmation at the trial (Wi-Fi plan 2's closing report lists decision 7)

### A failed Wi-Fi attempt deletes its new profile only when sure

When an attempt that can create a profile (`nmcli device wifi connect`, never a `connection up`)
fails, the profile that appeared for its SSID during the attempt is deleted, but only if the saved
list was read whole when the attempt began (so an older profile is never taken for the new one) and
the profile is still saved at the failure. Nothing is deleted when the attempt is still activating
at the 60 s bound (a slow network keeps its profile; commit b2223e0), when the device was last seen
activated on what may be the new profile, or when the state the failure rests on was kept through
another domain's outage (NetworkManager may have connected since; commit 5857c86). The cleanup
also deletes nothing after a NetworkManager restart, for a profile reserved to another user, or
after an outage of the network domain during a followed attempt (CLAUDE.md).

Date: 2026-10-13 (the merge of Wi-Fi delivery 2, CLAUDE.md) · Source: CLAUDE.md, "Quick settings, delivery 2" ("Connecting"); plan 2, "Decisions written down" 9; commits b2223e0 and 5857c86; `Attempt::failure`, `Attempt::expired`

Status: awaiting confirmation at the trial (Wi-Fi plan 2's closing report lists every Decision written down, 9 among them)

### A row under connection reads `…`

A Wi-Fi network being connected, and a VPN profile being brought up, show `…` after their name, not
`connecting…`. It is short, so the tag column can be `failed`'s six cells wide and an SSID keeps 16
cells (commit e711bb8).

Date: 2026-10-12 · Source: CLAUDE.md, "Quick settings, delivery 2" ("Connecting"); commit e711bb8 ("short tags"); quick VPN spec ("What": `…` while activating); `RUNNING_TAG`, `Wifi::tag`, `Vpn::tag`

### alpheus never handles a VPN secret

The VPN detail brings a profile up with `nmcli connection up uuid <uuid>`, never `nmcli --ask`, and
has no password field. `--ask` reads its secrets from standard input, which alpheus never feeds, so
the stored secrets in NetworkManager and any secret agent already registered (a desktop's) answer as
they usually do. A profile whose settings mark a secret as agent-owned or not saved (and not "not
required") does not fail because of it; when its attempt fails, the reason is the hedged
`the connection may need a secret` instead of `the activation failed`, and `More…` is the way.
`GetSecrets` is never called.

Date: 2026-11-15 · Source: quick VPN plan 1, "Rulings" 1 (it replaces the spec's "What" line on `nmcli --ask`); quick VPN spec ("Constraints": `GetSecrets` never)

Status: awaiting confirmation at the trial (CLAUDE.md, "Quick settings, delivery 4 (VPN)": does `nmcli connection up` find the VPNs' stored secrets without `--ask`?)

### Space on the VPN row takes the active profile down, else brings the last used one up

Space takes the first activating or activated profile (in the detail's order) down. With none, it
brings up the last used one: the highest `last_used` (NetworkManager's `connection.timestamp`) over
every profile read, even one past the 16 the detail lists; a tie or no timestamp: the first in the
detail's order. Nothing happens while an attempt runs.

Date: 2026-11-15 · Source: quick VPN spec ("What"); plan 1, "Rulings" 2 and 3; CLAUDE.md, "Quick settings, delivery 4 (VPN)"; `Vpn::toggle`

Status: awaiting confirmation at the trial (quick VPN plan 1: its Rulings are to be confirmed at the trial)

### A VPN attempt lives in the panel only, and taking a profile down is no attempt

`nmcli connection up` creates no profile, so a VPN attempt needs no cleanup after the panel closes:
closing drops it and `nmcli` carries on (the Wi-Fi attempt on a new network, which may leave a
profile behind, is followed after the close instead; see "A Wi-Fi attempt on a new network outlives
the quick panel"). A `connection down` is not followed: an active connection that NetworkManager
reports as activating or deactivating reads `…` from the state itself.

Date: 2026-11-15 · Source: quick VPN plan 1, "Rulings" 5 and 6; `Vpn::act`, `Vpn::value`

Status: awaiting confirmation at the trial (quick VPN plan 1: its Rulings are to be confirmed at the trial)

### Several active connections for one VPN profile count by the most advanced state

After a down then an up, NetworkManager may still list the old active connection, deactivated, beside
the new one. A profile's state is the most advanced of its entries (activated, then activating, then
deactivating, then deactivated), so a stale deactivated entry no longer fails a fresh attempt once the
new entry is listed. A known limit (CLAUDE.md, parked minors): a snapshot holding only the stale
deactivated entry still fails the attempt at once; the parked fix is to treat that state as waiting
until `activating` was seen.

Date: 2026-11-16 · Source: CLAUDE.md, "Quick settings, delivery 4 (VPN)" ("Final fix wave"); `active_state`

### A toggle's state that cannot be read shows `·`, never off

A configured toggle's state is read when the panel opens and 300 ms after its toggle ran, never
watched. A state that is empty, of a form `bar.toml` refuses, a path that cannot be expanded, or a
file whose existence cannot be told (any error but "not found") is unknown, and its row shows `·`:
the panel does not say "off" on a guess. The same rule holds for Shield: see "An unknown Shield
state is never read as off".

Date: 2026-10-11 · Source: quick settings spec § 4 (`state`); plan 1, "Decisions written down" 11; commit eab1ed5 (any error but "not found" is unknown); `state::read`

Status: awaiting confirmation at the trial (the plan's "Decisions written down" are to be confirmed at the trial, CLAUDE.md)

### The do-not-disturb detail marks and opens by the current state

The ● sits on `Off` when do-not-disturb is off, on `Until I turn it off` when it is on and untimed
(sharing included), and on `Until HH:MM` when a timed end's local time equals `[quick] dnd_until`;
otherwise on nothing. The detail opens on the ● entry, else on `30 minutes`, also when the ● entry is
`Off` (nothing to turn off). Why `30 minutes` is the fallback is not recorded.

Date: 2026-11-15 · Source: small features 2 plan 1, "Decisions written down" 8; commit b06e745 (the `Off` case and its reason); `marked`, `DndDetail::open`

Status: awaiting confirmation at the trial (small features 2 plan: its "Decisions written down" are to be confirmed at the trial, CLAUDE.md)

### Space on a blocked Bluetooth radio unblocks it, then powers it on

When rfkill holds the adapter off (`blocked` on the row), Space first runs `rfkill unblock bluetooth`
through the executor, and the adapter is powered on once BlueZ reports it unblocked, as soon as no
other action runs. The spec makes this choice (the question was left open at its review); its reason
is that GNOME does the same.

Date: 2026-10-22 · Source: quick Bluetooth spec § 1 (the `rfkill` sentence); `Bt::toggle`, `power_after_unblock`, `Bt::set_view`

### Nearby Bluetooth devices are inserted once, by signal strength

A device found while the detail is open is inserted by its signal strength at that moment and never
moves; later changes of signal are not followed. A device with no signal reading during this open, or
with no name of its own, is not listed. The spec's reason is that a scan then does not wake the loop
several times a second, and nothing moves under the selection. The cost, recorded in the plan, is that a device
advertising without a signal reading is never listed.

Date: 2026-10-20 · Source: quick Bluetooth spec § 2 (the `Nearby` rows); plan 3, "Decisions written down" 3; `BtReader`, `Bt::devices`

### A paired Bluetooth device is trusted, and a connect that is already done counts as done

After a pairing succeeds, the device's `Trusted` property is set, then a connect follows unless the
device is already connected. The spec's reason for trusting is that the device can then reconnect by
itself next time (the usual behaviour of GNOME and blueman). A Bluetooth LE device is often connected
by the pairing itself, so BlueZ's `AlreadyConnected` answer to a connect counts as a success. A failed
`Trusted` step shows `failed` and stops, with no connect; the device stays paired but untrusted, and
the next Enter connects it.

Date: 2026-10-20 · Source: quick Bluetooth spec § 2 (Enter on a nearby device); plan 3, "Decisions written down" 17; `Bt::answered`, `ALREADY_CONNECTED`

### Forgetting a Bluetooth device asks first, as the power menu does

Delete, or a right click, on a paired device turns its tag into `Forget? ⏎`; Enter then removes the
device. The timing and the cancel rules are those of "Reboot and power off ask first, and a second
activation confirms from 0.4 s to 3 s" (`session::CONFIRM_AFTER`, `session::CONFIRM`), through the
panel's one confirmation state, which holds either a session question or a forget question. A nearby
device cannot be forgotten. The asked form was chosen from mockups when the spec was written (spec
Goal, choice A); why it was preferred to other forms is not recorded.

Date: 2026-10-20 · Source: quick Bluetooth spec § 2 ("Forgetting"); plan 3, "Decisions written down" 10; `Bt::ask_forget`, `Bt::activate`, `session::ask`

### Bluetooth deadlines live in the panel, and a late answer is forgotten

Each call of an action has a 30 s deadline (`CALL_DEADLINE`) kept in the panel's `Action`, not on the
acting connection. The plan's reason is the spec's single panel timer, and the suspension of the
deadline while a pairing code shows. The cost is that an answer arriving after the deadline has to be
dropped explicitly, which the panel does by sending the connection a `BtOrder::Forget` for that call.

Date: 2026-10-22 · Source: plan 3, "Decisions written down" 7; quick Bluetooth spec § 2 ("Failure") and § 5 ("Timers"); `Action::deadline`, `Bt::due`

### A pairing is answered by alpheus's own agent, for that device only

alpheus registers its own `org.bluez.Agent1` with the `KeyboardDisplay` capability (`RegisterAgent`,
never `RequestDefaultAgent`). The spec's reason is that BlueZ routes a pairing to the agent of the
client that asked for it, so alpheus answers only the pairings it starts and never takes over another
tool's. The agent answers BlueZ's unique name only, and for the device being paired only. The spec
puts incoming pairings and codes to type into alpheus (very old devices) out of scope, and alpheus
authorizes no incoming service. Every agent call goes to the panel for its reply, and only the
acting connection refuses strangers and unknown members, so each reply is a pure decision that a unit
test checks; the cost is that a `Display*` reply waits one loop turn.

Date: 2026-10-20 · Source: quick Bluetooth spec §§ 3 and 5 ("Served"); plan 3, "Decisions written down" 9; `Bt::agent`, `CAPABILITY`

Status: awaiting confirmation at the trial (CLAUDE.md, "Quick settings, delivery 3 (Bluetooth)": BlueZ's D-Bus policy on NixOS and a real pairing are unverified)

### A pairing code has no timeout, and leaving the detail cancels the pairing

A code to compare (`Code 482 913? ⏎`) is not a forget-style question: it has no 0.4 s wait and no 3 s
expiry, since comparing a code takes longer, and it stays until it is answered, BlueZ cancels, or the
panel closes. It is the answer to the running pairing, not a new action. Closing the panel or leaving
the detail during a pairing cancels it (`Device1.CancelPairing`), because the code could no longer be
shown; unlike a Wi-Fi attempt, a pairing does not outlive the panel.

Date: 2026-10-20 · Source: quick Bluetooth spec § 3 (the paragraph after the table); `Bt::key`, `Bt::leaving`

### The Bluetooth acting connection opens with one blocking connect and asks for BlueZ's owner itself

The acting connection's connect and `Hello` block the event loop once each time the connection is
opened (when the detail opens or a call goes out). Reads never block; writes flush libdbus's queue,
blocking as the tray's session connection does (`BtAct::flush`, called from `bt_run`, and
`BtAct::close`). This departs from the spec's "Non-blocking, on the event loop", which the plan
accepts, as it accepts the same cost for the tray. The connection also sends its own `GetNameOwner` for
`org.bluez` at each open, and holds the calls made meanwhile until the answer; the plan's reason is
that the read thread's idea of the owner is not shared with the loop, and the cost is one extra round
trip per open. It asks again after a `NoReply` (commit 97fbbd0, `BtAct::recheck`): the same owner
keeps the failure; no owner or a new one answers `Gone` and forgets the owner, so nothing later goes
to an old unique name. When BlueZ goes or its owner changes, the connection is let go without a word.

Date: 2026-10-22 · Source: plan 3, "Decisions written down" 8 and 19; quick Bluetooth spec § 5 ("Non-blocking, on the event loop", "Lifetime", "Calls"), § 6; commit 97fbbd0; `BtAct::system`, `BtAct::flush`, `BtAct::recheck`, `App::bt_open`, `App::bt_let_go`

### UPower is the one service the daemon may start

The D-Bus source reads the system services through their unique names so that no read ever starts
one. The exception is UPower: on some systems (NixOS) it is activated by D-Bus only when a client
asks, so nothing has started it when the daemon comes up and the battery would stay missing. When
the battery is wanted and UPower has no owner at a session's start, the session sends one
`StartServiceByName("org.freedesktop.UPower", 0)` to the bus driver after its first publish, never
retried (a reconnection asks once more); the name appearing brings the battery in through
`NameOwnerChanged`. `dbusrs_bus::message` refuses any other name, and a guardrail lists the method.

Date: 2026-11-21 · Source: CLAUDE.md, "Bar delivery 3" (fix `fix/battery-upower`); `upower::activate_if_absent`, `start_service_by_name_names_upower_alone`

### Dvorak navigation keys are off by default

The control center moves with the arrow keys alone until a user sets `[keys] dvorak_nav = true` in
config.toml: a shareable product does not assume one keyboard layout.

Date: 2026-11-23 · Source: CLAUDE.md (Portability); `KeysConfig::default`, `Keymap::default`

### alpheus never handles sleep or the screens' power

alpheus has no code for sleep, suspend, `dpms` or turning a screen off. On a DisplayLink output
(evdi) a `dpms off` can freeze the machine, and the shells that arm it by default do exactly that;
idle handling stays with the idle daemon (hypridle, swayidle) that the user already runs. The one
write to the compositor is the screens' layout through the output protocol, which carries no power
state.

Date: 2026-10-02 · Source: SCOPE.md, "Non-negotiable guardrails" (amended 2026-10-30 by the displays spec § 2)

### The daily core is polished, and no more system pieces are replaced

alpheus works on what is used every day (the bar, the launcher, notifications, the control center,
centralised settings, finishing touches) and stops taking over system pieces: the lock screen, the
portal's file chooser and settings backend, the screensaver. The lock screen was written and
reviewed but left unmerged; the file chooser was dropped during its design. A new feature must
serve the core, not extend alpheus into another piece of the system.

Date: 2026-10-11 · Source: CLAUDE.md, "Refocus" and "Dropped 2026-10-11"; SCOPE.md
