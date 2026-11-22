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

This first batch holds the focus timer's decisions only (`docs/features/timer.md`). The rest of
the project's decisions are added as each feature's code is brought to the same standard.

---

### The timer is drawn at rest

The `timer` module is in the default modules, and when nothing runs it still shows one dim
glyph. Without it the mouse would have nowhere to click to open the popup. `[timer] idle =
"hidden"` draws nothing at rest instead (the `alpheus-launch --timer popup` verb still opens the
popup).

Date: 2026-11-15 · Source: timer spec § 2; timer plan 1, decision 1

### The timer counts wall-clock time

A timer keeps its end (or its start) as a Unix time in milliseconds, not as a count of ticks. So
time that passes while the laptop sleeps counts: a countdown rings when the laptop wakes, and a
stopwatch has counted the sleep. A clock set backwards can make a countdown's time left look
longer than the countdown itself, so the time left is cut to the total.

Date: 2026-11-15 · Source: timer spec § 1; timer plan 1, decision 5

### One deadline, armed only while a timer runs

The timer uses one `DeadlineTimer` (a one-shot timer on the event loop), set to the next moment
anything about the timer changes: the end, or the next minute of the chip's text. An idle or
paused timer arms nothing, so the daemon sleeps. A one-second tick would wake the daemon 60 times
a minute for a chip that changes once.

Date: 2026-11-15 · Source: timer spec "Constraints" and "Approaches"; timer plan 1, decision 4

### The clock's minute tick re-checks the timer

The deadline is a monotonic timer on the event loop, and monotonic timers do not run while the
laptop sleeps (timer spec § 1), so after a suspend it fires late. The clock module's tick is also
a timer on the loop; it is re-aimed at each wall-clock minute (at each second when the clock's
format shows seconds), so it is at most a minute away. It calls `timer_check_clock` as well, so an
end that passed during sleep is seen within a minute of waking.
A deadline that fires early only arms the next one.

Date: 2026-11-15 · Source: timer spec § 1; timer plan 1, decision 4; guardrail `the_timer_is_wired_through_one_path`

### A late end is applied once

If the end of a Pomodoro phase passed long ago (a sleep, a stalled loop), it is applied once, and
the next phase starts at the moment the end is noticed, not at the old end. Otherwise one long
sleep would make several phases end one after another with a notification each.

Date: 2026-11-15 · Source: timer plan 1, decisions 5 and 6, Review Focus 1

### A change applies a due end first

Before any change (a click, a key, a verb) the timer first applies an end that is already due. A
Pause or an Add after a resume therefore finds nothing to pause or extend, instead of pausing a
timer that has in fact already ended. `change_step` does this in one place, and `timer_do` must go
through it.

Date: 2026-11-15 · Source: timer plan 1, Review Focus 1; guardrail `the_timer_is_wired_through_one_path`

### Under do-not-disturb the end goes to the history only

When do-not-disturb is on, a timer's end sends its notification to the server, which keeps it in
the history and counts it on the bell, but shows no bubble. The optional sound does not play. The
chip's `done` shows either way. The notification is sent only while alpheus's own server owns its
name, and is never kept for later: a late timer notice is noise.

Date: 2026-11-15 · Source: timer spec § 5; timer plan 1, decision 8

### The 24-hour stopwatch stop is silent

A stopwatch stops itself, paused, after a day. That writes one log line and notifies nothing.

Date: 2026-11-15 · Source: timer plan 1, decisions 9 and 13

### `done` is cleared by looking, and is never saved

After a countdown or a Pomodoro set ends, the chip reads `done` until someone looks at it:
opening the popup, a middle click, any `--timer` verb or starting a new timer clears it. An end
that the popup showed counts as seen when the popup closes. `done` is not written to the state
file, so it does not come back after a restart.

Date: 2026-11-15 · Source: timer spec §§ 2 and 6; timer plan 1, decision 7

### The state file is hostile input

The timer's state file is read back at start as if someone else had written it: exact fields,
digits only, every length within its bounds, a running end at most 25 hours ahead, a round from 1
to 12. Anything else is dropped with one log line, and the timer starts idle. The code that reads
it never panics (guardrail `the_timer_has_no_panicking_path`).

Date: 2026-11-15 · Source: timer spec § 6; timer plan 1, decision 13

### The popup lists what the state allows

The popup offers only the actions that make sense now: an idle timer offers to start one of the
three kinds, a countdown offers Pause or Resume, Add 1 min and Stop, a Pomodoro offers Skip
instead of Add, a stopwatch only Pause or Resume and Stop. The first action is selected when the
popup opens, and again whenever the list changes. It stays open after an action. Stop asks no
question and a new start replaces whatever runs.

Date: 2026-11-15 · Source: timer spec § 3; timer plan 1, decisions 10 and 11

### The popup's one-second tick exists only while it is open

While the popup is open and a timer runs, the time in it counts each second, through the same one
deadline. When the popup closes, by any way, the tick goes with it (the popup's `closed` hook
re-sets the deadline). With the popup closed the chip changes at most once a minute.

Date: 2026-11-15 · Source: timer spec § 3 and "Constraints"; timer plan 1, decision 4; guardrail `the_timer_popup_closing_drops_its_tick_and_sees_the_end`

### The verb opens the popup under the chip

`alpheus-launch --timer popup` opens the popup under the chip on the focused output's bar, or
under the bar's right end when the chip is hidden, taking the keyboard without a grab. A second
verb within 250 ms of the one that opened it is ignored, because some compositors fire a key bind
twice.

Date: 2026-11-15 · Source: timer spec § 4; timer plan 1, decision 17

### Logs carry fixed words and numbers only

The timer's log lines are fixed words (`timer: paused`, `timer: countdown ended`) and numbers. They
never hold the sound's command, a path, or an error's own text, since any of these can name the
user. A failed start of the sound logs a kind word only (`not found`, `timeout`, `io`).

Date: 2026-11-15 · Source: timer spec "Constraints"; timer plan 1, decision 18

### A source that is down is not a state

A count or a value that becomes unknown because its source went away (the system bus closing,
NetworkManager or BlueZ leaving the bus, a battery reading `unknown`) changes nothing: the last
known value stays, the module does not hide, the notice is not re-armed and no hook fires. The next
real answer replaces the value. Only a count that was never known is absent, and the `failed`
module hides then rather than showing a placeholder.

Date: 2026-11-15 · Source: hooks spec § 2; hooks plan 1, decision 6; diagnostics spec § 6 and "Decisions written down" 13

### The failed-units count is read on triggers, and `Subscribe` is off by default

systemd sends the Manager's `PropertiesChanged` only while some client has called `Subscribe()`, so
without it no signal about the count arrives. `[failed] subscribe = true` makes the icon instant,
and its cost lands on systemd and dbus-daemon (a signal per unit change to every subscriber), not
on alpheus. The default is `false` until it is chosen after use. Without a subscription the count
is read on triggers: session start, resume, `alpheus-launch --failed-refresh`, and a read added to
another domain's wake-up at most once a minute. No timer is added for it.

Date: 2026-11-22 · Source: diagnostics spec §§ 6 and 9; diagnostics plan 1, decision 1 and its final fix wave

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

Date: 2026-11-21 · Source: diagnostics spec § 6 ("Notification"); diagnostics plan 1, decision 11

### The bar has no ignore list for failed units

A unit that fails at every boot keeps `󰀦 1` on the bar until it is fixed or reset (the Diag tab's
`c`). An ignore list would need the daemon to list units and read their names, which it never
does (see "The failed-units count is asked for one property, never the whole interface").

Date: 2026-11-22 · Source: diagnostics spec "Decisions written down" 9

### A hook fires on a change, never for the state at start

Each event's first observed value is a seed and never fires, and for the first 3 seconds after the
daemon starts (`QUIET`) any change only moves the seed, so that the state at start never fires a
hook. The window covers the portal's first answer, the first PipeWire graph, a camera held across a
restart and a profile applied at start. `started` is the one event that ignores the window. The window is a timestamp
compared when an edge arrives, never a timer.

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

Date: 2026-10-05 · Source: tray plan 2, decision 3

### The calendar's first day comes from the locale, read at each open

The first day of the week is the locale's `LC_TIME` setting. The `C` and `POSIX` locales, or a
locale that cannot be read, give Monday (ISO 8601). The locale is read each time the calendar
opens, through `newlocale`, `nl_langinfo_l` and `freelocale`: this is thread-safe, changes no
global locale, and nothing is kept while the calendar is closed.

Date: 2026-10-05 · Source: tray plan 2, decisions 4 and 11; tray spec "Calendar"

### Calendar week numbers are ISO 8601

When `[clock] week_numbers` is on, the numbers are ISO 8601 weeks. In a calendar whose week starts
on Sunday, a row shows the ISO week of its Monday. A sixth row with no day of the month shows no
number.

Date: 2026-10-05 · Source: tray plan 2, decision 5 and "Controller reconciliation"
