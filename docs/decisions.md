# Decisions

This file lists decisions that are settled: things alpheus does a certain way on purpose, and
the reason. It is not a history of how alpheus was built. Code comments point here when a rule
would otherwise look arbitrary to someone reading the code.

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
`docs/features/`, and the older decisions of the rest are still in `docs/specs/` and `docs/plans/`.

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
twice.

Date: 2026-11-15 · Source: timer spec § 4; timer plan 1, decision 17

Status: awaiting confirmation at the trial (timer plan 1, "Decisions written down": every one is to be confirmed at the trial)

### Logs carry fixed words and numbers only

The timer's log lines are fixed words (`timer: paused`, `timer: countdown ended`) and numbers. They
never hold the sound's command, a path, or an error's own text, since any of these can name the
user. A failed start of the sound logs a kind word only (`not found`, `timeout`, `io`). The power
menu and the resources popup's `Monitor…` row follow the first half of the rule: they log the
entry's word (`power: power off`, `resources: monitor`), never the command. They differ on a failed
start: they add the executor's error (`could not start (NotFound)`; for an I/O error, the system's
own message), where the timer logs the kind word alone. The quick panel names the program in its
own start failures, and the right click on the resources module goes through `actions::run`,
which logs the whole argv of a failed start; neither follows this rule, and both are known
follow-ups.

Date: 2026-11-15 · Source: timer spec "Constraints"; timer plan 1, decision 18; power spec § 3 and plan 1, decision 10; resources popup spec "Constraints"

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
closed. A second verb closes the menu, except within 250 ms of its own open (some compositors fire
a key bind twice).

Date: 2026-11-21 · Source: tools menu spec § 7; tools menu plan 1, `power_verb`

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
position is read with one call when the popup opens, on `Seeked`, and when the status or the track
changes while it is open; with the popup closed it stays unknown. The `Seeked` rule is on the bus
only while the popup is open, and the 1 s redraw timer exists only while the popup (or the
notification panel's media card) is shown and the active player plays. This keeps the module to
the spec's lightness rules (no polling, no wake-up at rest).

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

Date: 2026-10-08 · Source: media spec § 1 ("Content"); media plan 1, Task 2, "Decisions written down", item "Paused", and the list of confirmed choices at the end of "Controller reconciliation" (first bullet)

### Media titles are cut by display width, never inside a cluster

The title is cut to `title_max` cells and ends with `…`. Width is counted per character as the grid
counts it, after putting the text in NFC form, and a cut never falls inside a cluster (a combining
mark, a variation selector or a skin tone after its base, a ZWJ sequence, a flag's two regional
indicators). It is written by hand on the `unicode-width` crate the daemon already uses, so that no
dependency is added.

Date: 2026-10-08 · Source: media spec § 1 (amended 2026-10-08); media plan 1, Task 1, "Decisions written down", item "`truncate_title`"

### The microphone pill shows only when its mute flips

The OSD shows the default source only when its mute state flips, with the level the source has
then. A change of its level never shows: WebRTC apps (a browser or a chat client in a call) move the
capture gain on their own, and the pill would pop all through a call. The sound source no longer
publishes a microphone level change alone, so a call wakes nothing. A mute flip shows even right
after a sink pill (an earlier 500 ms rule, made for level noise, was dropped as it had nothing left
to guard), and a message that changes both the sink and the microphone shows the sink.

Date: 2026-10-09 · Source: OSD spec § 1 and § 2 (amendments of 2026-10-09)

### A sound change within a second of a start or a new default device shows nothing

The first state after `pw-dump` (re)starts is a baseline and never shows. For 1 s after it, and for
1 s after a default sink or source appears, goes or is switched, a change shows nothing: the levels of
a new default settle in silence (WirePlumber restores them). The end of the second counts as outside
it. One exception: while the sink's name card is on screen, a change inside that second updates the
card (a second switch shows the new name, a key press the new level); and a switch to a sink whose
volume has not arrived shows the card once it does.

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
alpheus never writes it: the user's brightness key keeps changing it (through `brightnessctl`) and
then calls the verb. A watcher would pop the pill each time hypridle dims and restores the
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

Date: 2026-10-05 · Source: tray plan 2, "Mae's decisions (2026-10-05)", item 8; tray spec § 1 ("Menus": no answer within 2 s)

### A tray menu opens with its first entry selected

The first selectable entry is selected before the popup is first drawn, so Enter or a key press has
something to act on at once.

Date: 2026-10-05 · Source: tray plan 2, "Controller reconciliation", "Mae's decisions added after the skeleton"

### A submenu opens in the same popup

A submenu replaces the menu's current level inside the same popup (→ or Enter enters it, ← goes back
one level), with the first row showing `◂ <parent label>`. No cascading popups are opened.

Date: 2026-10-05 · Source: tray plan 2, "Mae's decisions (2026-10-05)", item 2

### A failed Activate opens the menu of the icon that was pressed

A left click sends `Activate`; when the item answers with a D-Bus error (Steam's libappindicator has
no such method) the bar opens its menu instead. That answer comes later, so the bar remembers the icon,
the cells it occupies and the button that pressed it, and the menu belongs to that press, not to
whatever is under the pointer when the answer arrives. A press that lands on no icon forgets the
remembered one, so a late answer never opens a menu for an icon the user has left. A timeout is not a
failure, because a slow application may have acted.

Date: 2026-10-05 · Source: tray spec § 2 (left click row); tray plan 2, "Controller decisions", item 5; commit 40be9a6 (the last icon cleared by a press on no icon)

### A tray menu is closed by the button that opened it, or by any right click

A press on the icon whose menu is open only closes that menu, and does nothing else, when it is made
with the button that opened it, or with the right button whatever opened it. So Steam's menu, opened
by a left click, closes with the same left click, with no new Activate and no flicker.

Date: 2026-10-06 · Source: commit 40be9a6 (the 2026-10-06 ruling quoted in its message); tray plan 2, "Controller reconciliation", "Mae's decisions added after the skeleton" (a second right click on the icon whose menu is open closes it)

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

The microphone indicator lights for a link from an audio source to a `Stream/Input/Audio` that is
active or whose stream runs. It is not lit for a stream that is a meter (`stream.monitor`, such as
pavucontrol's), one that reads a sink's monitor (`stream.capture.sink`, the visualisers), a member of
a `node.link-group` (filter chains, loopbacks and echo cancellation, which forward sound and do not
record) or a passive stream.

Date: 2026-10-16 · Source: privacy spec § 2 ("The rules", Microphone)

### A muted app stream still lights the microphone

The model has no per-stream mute: a stream that is linked and running holds the device, so the
indicator stays lit even when the app muted its own stream, since the app still holds the
microphone. Only the default source's mute changes how it is drawn: the glyph is dim, and so is the
popup's heading.

Date: 2026-10-16 · Source: privacy spec § 1 ("Muted microphone"); privacy plan 1, "Review Focus", item 2; privacy plan 1, "Decisions written down", item 1; CLAUDE.md, "Privacy indicators, delivery 1" (the dim glyph also in the popup's heading)

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

Date: 2026-10-20 · Source: privacy camera spec § 1 ("The debounce", amended at execution); privacy plan 2, "Decisions written down", item 3

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

### The camera scan runs on the event loop

The scan, a few milliseconds over a few hundred processes, is synchronous on the bar's event loop and
not on a thread of its own. The measured median is 4.0 to 4.1 ms on 120 processes of 30 descriptors,
under the 10 ms bound beyond which the spec would have moved it to a short-lived thread.

Date: 2026-10-20 · Source: privacy camera spec § 2 (the 10 ms rule); CLAUDE.md, "Privacy indicators, delivery 2" ("Scan cost", the controller's ruling)

### The camera watch is a second inotify instance

The watch on the video devices has its own inotify instance, apart from the theme watch's. This keeps
the theme watch untouched and the two lifetimes apart, at the cost of one file descriptor. Delivery
1's spec had planned to use the existing instance.

Date: 2026-10-20 · Source: privacy camera spec § 4 (amendment of delivery 1's § 10)
