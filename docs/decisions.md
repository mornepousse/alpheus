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

Date: <YYYY-MM-DD> · Source: <the tests or code that pin it>
```

An entry may carry one more line, `Status: awaiting confirmation at the trial`. It means the
decision was taken while the feature was built, it is to be confirmed once the feature has been
tried, and that has not been done yet; the line is removed when it has. An entry
with no `Status` line is settled.

Each feature's decisions are added when its code is brought to the house standard
(`docs/STYLE.md`); the features it covers have a page in `docs/features/` (listed in
[features/README.md](features/README.md)).

---

### The timer is drawn at rest

The `timer` module is in the default modules, and when nothing runs it still shows one dim
glyph. Without it the mouse would have nowhere to click to open the popup. `[timer] idle =
"hidden"` draws nothing at rest instead (the `alpheus-launch --timer popup` verb still opens the
popup).

Date: 2026-11-15 · Source: tests `the_chip_reads_minutes` and `timer_defaults_and_bounds`; headless `timer_a_click_opens_the_popup_and_enter_starts_a_countdown`

Status: awaiting confirmation at the trial

### The timer counts wall-clock time

A timer keeps its end (or its start) as a Unix time in milliseconds, not as a count of ticks. So
time that passes while the laptop sleeps counts: a countdown rings when the laptop wakes, and a
stopwatch has counted the sleep. A clock set backwards can make a countdown's time left look
longer than the countdown itself, so the time left is cut to the total.

Date: 2026-11-15 · Source: tests `the_time_left_is_clamped_and_each_kind_has_its_word` and `the_stopwatch_counts_and_caps`; headless `timer_survives_a_daemon_restart`

Status: awaiting confirmation at the trial

### One deadline, armed only while a timer runs

The timer uses one `DeadlineTimer` (a one-shot timer on the event loop), set to the next moment
anything about the timer changes: the end, or the next minute of the chip's text. An idle or
paused timer arms nothing, so the daemon sleeps. A one-second tick would wake the daemon 60 times
a minute for a chip that changes once.

Date: 2026-11-15 · Source: tests `next_change_follows_the_chip_or_the_seconds`, `the_wait_for_a_target_is_zero_when_past_and_never_overflows` and `a_deadline_already_armed_for_that_instant_stays`; guardrail `the_timer_is_wired_through_one_path`

Status: awaiting confirmation at the trial

### The clock's minute tick re-checks the timer

The deadline is a monotonic timer on the event loop, and monotonic timers do not run while the
laptop sleeps, so after a suspend it fires late. The clock module's tick is also
a timer on the loop; it is re-aimed at each wall-clock minute (at each second when the clock's
format shows seconds), so it is at most a minute away. It calls `timer_check_clock` as well, so an
end that passed during sleep is seen within a minute of waking.
A deadline that fires early only arms the next one.

Date: 2026-11-15 · Source: test `the_clock_check_applies_one_end_and_says_to_save`; guardrail `the_timer_is_wired_through_one_path`

Status: awaiting confirmation at the trial

### A late end is applied once

If the end of a Pomodoro phase passed long ago (a sleep, a stalled loop), it is applied once, and
the next phase starts at the moment the end is noticed, not at the old end. Otherwise one long
sleep would make several phases end one after another with a notification each.

Date: 2026-11-15 · Source: test `a_late_end_is_applied_once_and_the_next_phase_starts_now`

Status: awaiting confirmation at the trial

### A change applies a due end first

Before any change (a click, a key, a verb) the timer first applies an end that is already due. A
Pause or an Add after a resume therefore finds nothing to pause or extend, instead of pausing a
timer that has in fact already ended. `change_step` does this in one place, and `timer_do` must go
through it.

Date: 2026-11-15 · Source: test `a_due_end_is_applied_before_the_change`; guardrail `the_timer_is_wired_through_one_path`

Status: awaiting confirmation at the trial

### Under do-not-disturb the end goes to the history only

When do-not-disturb is on, a timer's end sends its notification to the server, which keeps it in
the history and counts it on the bell, but shows no bubble. The optional sound does not play. The
chip's `done` shows either way. The notification is sent only while alpheus's own server owns its
name, and is never kept for later: a late timer notice is noise.

Date: 2026-11-15 · Source: test `effects_follow_dnd_serving_and_the_sound`; headless `timer_under_dnd_ends_without_bubble_or_sound`

Status: awaiting confirmation at the trial

### The 24-hour stopwatch stop is silent

A stopwatch stops itself, paused, after a day. That writes one log line and notifies nothing.

Date: 2026-11-15 · Source: tests `the_stopwatch_counts_and_caps` and `each_end_has_its_fixed_log_line`

Status: awaiting confirmation at the trial

### `done` is cleared by looking, and is never saved

After a countdown or a Pomodoro set ends, the chip reads `done` until someone looks at it:
opening the popup, a middle click, any `--timer` verb or starting a new timer clears it. An end
that the popup showed counts as seen when the popup closes. `done` is not written to the state
file, so it does not come back after a restart.

Date: 2026-11-15 · Source: tests `a_new_start_replaces_whatever_runs_and_seen_clears_done`, `closing_the_popup_dismisses_a_done_chip_only` and `the_saved_line_round_trips`; headless `timer_an_end_seen_in_the_open_popup_is_seen`

Status: awaiting confirmation at the trial

### The state file is hostile input

The timer's state file is read back at start as if someone else had written it: exact fields,
digits only, every length within its bounds, a running end at most 25 hours ahead, a round from 1
to 12. Anything else is dropped with one log line, and the timer starts idle. The code that reads
it never panics (guardrail `the_timer_has_no_panicking_path`).

Date: 2026-11-15 · Source: tests `parse_saved_refuses_what_is_out_of_bounds` and `loading_reads_a_good_file_drops_a_bad_one_and_ends_silently`; guardrail `the_timer_has_no_panicking_path`

Status: awaiting confirmation at the trial

### The popup lists what the state allows

The popup offers only the actions that make sense now: an idle timer offers to start one of the
three kinds, a countdown offers Pause or Resume, Add 1 min and Stop, a Pomodoro offers Skip
instead of Add, a stopwatch only Pause or Resume and Stop. The first action is selected when the
popup opens, and again whenever the list changes. It stays open after an action. Stop asks no
question and a new start replaces whatever runs.

Date: 2026-11-15 · Source: tests `the_items_follow_the_state` and `follow_resets_the_selection_when_the_items_change` (`timer/popup/tests.rs`)

Status: awaiting confirmation at the trial

### The popup's one-second tick exists only while it is open

While the popup is open and a timer runs, the time in it counts each second, through the same one
deadline. When the popup closes, by any way, the tick goes with it (the popup's `closed` hook
re-sets the deadline). With the popup closed the chip changes at most once a minute.

Date: 2026-11-15 · Source: guardrail `the_timer_popup_closing_drops_its_tick_and_sees_the_end`; headless `timer_the_popups_second_tick_stops_at_close`

Status: awaiting confirmation at the trial

### The verb opens the popup under the chip

`alpheus-launch --timer popup` opens the popup under the chip on the focused output's bar, or
under the bar's right end when the chip is hidden, taking the keyboard without a grab. A second
verb within 250 ms of the one that opened it is ignored, because some compositors fire a key bind
twice (Hyprland re-fires a press bind).

Date: 2026-11-15 · Source: headless `timer_popup_verb_opens_and_closes_it`; `TOGGLE_DEBOUNCE` in `launcher/host.rs`

Status: awaiting confirmation at the trial

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
client's reply, the errors that cross a thread and become a word before a `log(` sees them, and the
panic messages the hook never prints). The media lines follow it: `media popup shown` and the
"players already followed" line name neither the track, the artist nor the player; the headless
tests read the popup from the grid.

Four exceptions are kept, approved by the maintainer on 2026-11-24, because each is the user's own
or is not private: the output's connector name in `bar placed on` and `bar removed from` (a
connector such as `DP-1`, never a monitor's make or model); the config parser's message for
`bar.toml` and `launcher.toml` (the user's own file, which can quote a value of it); the user's own
labels for launcher sources and sensor patterns; and UPower's validated D-Bus error name.

Date: 2026-11-15 (extended 2026-11-24: the whole daemon) · Source: guardrail `no_log_line_names_a_program_an_error_text_or_a_panic_payload`; tests `each_end_has_its_fixed_log_line`, `change_lines_are_fixed_words`, `the_sound_spawns_its_argv_and_logs_no_command`, `an_entry_spawns_its_command_and_logs_its_word_only` and `spawn_monitor_logs_without_the_command`

Status: awaiting confirmation at the trial (the timer part)

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

Date: 2026-11-15 · Source: tests `a_source_down_is_not_a_state`, `the_sound_source_going_down_fires_nothing`, `the_compositor_going_down_keeps_only_the_screen` (`hooks/tests.rs`) and `an_unknown_total_changes_nothing` (`failed/tests.rs`)

### The failed-units count is pushed by a subscription, and read on triggers when it is off

systemd sends the Manager's `PropertiesChanged` only while some client has called `Subscribe()`, so
without it no signal about the count arrives. `[failed] subscribe` is `true` by default: the icon is
instant, and the cost lands on systemd and dbus-daemon (a signal per unit change to every
subscriber), not on alpheus. While subscribed, the read added to another domain's wake-up is
skipped, since systemd pushes the change. With `subscribe = false` the count is read on triggers:
session start, resume, `alpheus-launch --failed-refresh`, and a read added to another domain's
wake-up at most once a minute. No timer is added for it, either way.

Date: 2026-11-24 · Source: the maintainer's choice at the trial of the diagnostics work: an instant icon is worth a signal per unit change on systemd and dbus-daemon. It replaces the 2026-11-22 entry "The failed-units count is read on triggers, and `Subscribe` is off by default"; tests `the_client_subscribes_once_unless_subscribe_is_off`, `an_already_subscribed_manager_counts_as_subscribed` and `the_managers_signal_reads_the_count_again`; headless `failed_refresh_rereads_the_user_count_unsubscribed`

### The failed-units count is asked for one property, never the whole interface

The count is a `Get` of `NFailedUnits` on the Manager object, on the owner's unique name. A
`GetAll` of the Manager would also carry its environment, which may hold secrets, so the call
builder refuses any other interface and property pair, and the daemon never reads a unit name: it
counts, it does not list.

Date: 2026-11-21 · Source: guardrails `the_user_manager_is_asked_for_one_property_and_subscribed_to` and `no_acting_systemd1_method_in_the_daemon`; test `the_client_asks_for_the_owner_then_reads_the_count`

### The failed-units notice waits for both managers

`[failed] notify` sends one notice when the total rises above the last total notified, and a total
of 0 re-arms it. The notice waits until the system manager and the user manager have each answered
once (a manager nobody will ask, with `user_units = false` or no session bus, is not waited for),
so the first notice carries the whole total and not half of it. Like the battery alerts, it counts
as given only while alpheus's own notification server is serving, and the latest total is offered
again when the server starts.

Date: 2026-11-21 · Source: tests `a_notice_waits_until_both_managers_answered_once`, `a_notice_goes_on_a_rise_and_is_rearmed_at_zero`, `a_notice_waits_for_the_server` and `a_notice_offered_again_after_settling_carries_the_total`

### The bar has no ignore list for failed units

A unit that fails at every boot keeps `󰀦 1` on the bar until it is fixed or reset (the Diag tab's
`c`). An ignore list would need the daemon to list units and read their names, which it never
does (see "The failed-units count is asked for one property, never the whole interface").

Date: 2026-11-22 · Source: the count is a number and never a list: test `the_total_adds_the_known_counts`; guardrails `the_user_manager_is_asked_for_one_property_and_subscribed_to` and `no_acting_systemd1_method_in_the_daemon`

### A hook fires on a change, never for the state at start

Each event's first observed value is a seed and never fires, and for the first 3 seconds after the
daemon starts (`QUIET`) any change only moves the seed, so that the state at start never fires a
hook. The window covers the portal's first answer, the first PipeWire graph, a camera held across a
restart and a profile applied at start. `started` is the one event that ignores the window. The
window is a timestamp compared when an edge arrives, never a timer.

Date: 2026-11-15 · Source: tests `the_first_value_of_each_event_is_a_seed_and_never_fires`, `edges_inside_the_quiet_window_only_move_the_seed`, `started_ignores_the_quiet_window` and `the_quiet_window_is_marked_once_when_it_ends`

### A hook's data reaches it through the environment, never the arguments

The event's data is given as `ALPHEUS_*` environment variables. No placeholder is expanded in the
command, and `~` and `$VAR` are not expanded either (a hook that wants a shell is `["sh", "-c",
"…"]`). Placeholders such as `{device}` were rejected as an injection surface: a device or profile
name comes from outside and must never become an argument or a shell word. The names handed over
are cleaned and cut to 255 bytes, and no hook log line ever holds a command, a variable or a name.

Date: 2026-11-15 · Source: tests `the_environment_of_each_event`, `the_argv_never_holds_event_data`, `a_hostile_device_name_is_cleaned_and_cut` and `a_hook_never_inherits_a_variable_its_event_does_not_set`; guardrail `hook_logs_carry_fixed_words_only`

### Each hook has a token bucket

Per event, 5 runs may happen at once, then one more every 3 seconds. A run without a token is
dropped, not queued, and the first drop of an episode logs one `throttled` line. A source that
flaps (a dock plugging in a loop, a radio bouncing) therefore costs at most that. The bucket is a
timestamp compared when the event arrives, not a timer.

Date: 2026-11-15 · Source: tests `the_bucket_gives_five_then_one_every_three_seconds` and `a_throttled_event_is_said_once_per_episode`; headless `hooks_a_flapping_event_is_throttled`

### A hook never adds a source

Events come from what the configured modules already read. An event whose module is not in
`modules` never fires, and the daemon says so once at start (`hooks: <event> never fires (needs
<modules>)`). A hook on `bluetooth` with no Bluetooth module would otherwise make the daemon read
BlueZ: a cost hidden behind a hook.

Date: 2026-11-15 · Source: tests `an_event_is_read_only_when_its_modules_are_in_modules` and `the_start_lines_name_each_hook_that_never_fires`; headless `hooks_say_what_never_fires`

### A hook runs in its own systemd scope

A hook is started like the launcher starts an app: in `systemd-run --user --scope --quiet
--collect --` when a user manager runs, and detached on its own otherwise, or when `systemd-run`
is missing. The hook therefore never dies with the bar, and a restart of the bar never kills it.
The choice is made once at start. alpheus never waits for a hook, reads its output or kills it.

Date: 2026-11-15 · Source: tests `a_hook_runs_in_a_systemd_scope`, `a_hook_runs_detached_when_asked_to` and `a_hook_runs_detached_when_systemd_run_is_missing`

### The calendar is written in English, like the clock

The month title (`October 2026`) and the weekday initials (`Mo Tu We`) are in English, as the
clock's text is.

Date: 2026-10-05 · Source: test `the_day_and_month_names_are_english` (`clock.rs`)

### The calendar's first day comes from the locale, read at each open

The first day of the week is the locale's `LC_TIME` setting. The `C` and `POSIX` locales, or a
locale that cannot be read, give Monday (ISO 8601). The locale is read each time the calendar
opens, through `newlocale`, `nl_langinfo_l` and `freelocale`: this is thread-safe, changes no
global locale, and nothing is kept while the calendar is closed.

Date: 2026-10-05 · Source: tests `the_first_day_comes_from_the_locale_fields_and_unreadable_ones_give_monday` and `locale_first_day_reads_lc_time` (`tests/calendar_locale.rs`)

### Calendar week numbers are ISO 8601

When `[clock] week_numbers` is on, the numbers are ISO 8601 weeks. In a calendar whose week starts
on Sunday, a row shows the ISO week of its Monday.

Date: 2026-10-05 · Source: tests `week_numbers_show_the_iso_week_of_each_rows_monday` and `iso_week_numbers_are_right_at_the_edges_of_a_year`

### The power menu runs the `[quick]` commands and has no setting of its own

The ⏻ menu runs `[quick]`'s `lock`, `logout`, `reboot` and `poweroff`, the commands the quick
panel's session buttons run, so one setting serves both. A `[power]` table would only repeat them.
An entry whose command is empty is not shown, and with all four empty the menu and its button do not
exist. alpheus never handles sleep, so there is no suspend or hibernate entry.

Date: 2026-10-13 · Source: tests `the_rows_follow_the_commands_in_order_and_none_means_no_menu`, `an_entry_spawns_its_command_and_logs_its_word_only` and `the_verb_is_refused_only_without_any_session_command`

### Reboot and power off ask first, and a second activation confirms from 0.4 s to 3 s

Lock and Log out run at once. Reboot and Power off turn their row into `Reboot? ⏎` or
`Power off? ⏎` in the warning colour; a second activation runs the command only from 0.4 s after the
question appeared and until 3 s after it, so a double click or a double Enter never confirms. Moving
the selection, hovering or pressing another entry, any other key, or 3 s passing cancels the
question. The rule is two functions, `session::activate` and `session::expire`, shared with the
quick panel, so the panel and the menu cannot drift apart.

Date: 2026-10-13 · Source: tests `power_off_asks_and_a_second_activation_confirms_it`, `moving_another_entry_a_key_or_3_s_cancel_the_question` and `reboot_and_power_off_ask_then_confirm_from_0_4_s_until_3_s` (`session.rs`); headless `power_two_enters_a_tenth_of_a_second_apart_run_nothing`

Status: awaiting confirmation at the trial for one part, any other key cancelling the question; the 0.4 s to 3 s rule is settled

### Nothing is selected when the power menu opens

As in a tray menu, no entry is selected at open: ↓ or Tab selects the first, ↑ or Shift+Tab the
last, and Enter with nothing selected does nothing; this is also how the headless test reads
(`power_a_click_opens_the_menu_and_down_enter_locks`: Down, then Enter, runs Lock). The selection is clamped and never wraps, as in every alpheus list.

Date: 2026-10-13 · Source: tests `the_keys_move_clamped_from_nothing_selected` and `the_pointer_selects_and_presses_entries_only`; headless `power_a_click_opens_the_menu_and_down_enter_locks`

Status: awaiting confirmation at the trial

### A held key repeats only to move, in the power menu

The keyboard's auto-repeat moves the selection but never activates: a held Enter acts once. Without
this the repeat would arrive inside the 0.4 s to 3 s window and confirm the question that the
first Enter had just asked. The wheel and the right and middle buttons do nothing in the menu.

Date: 2026-10-13 · Source: tests `a_held_enter_acts_once` and `the_pointer_selects_and_presses_entries_only`

Status: awaiting confirmation at the trial

### The power menu has its own timer

The question's 3 s expiry uses a `DeadlineTimer` of the menu's own, armed only while a question is
pending and released whatever closes the menu. The quick panel's timer is not reused, because that
one may still serve a followed Wi-Fi attempt after its panel has closed.

Date: 2026-10-13 · Source: guardrail `the_power_menu_lets_go_of_its_timer_and_runs_only_its_entry`; headless `power_escape_closes_it_with_its_question_and_nothing_stays_awake`

### `alpheus-launch --power` opens the menu without a grab

The verb opens the menu under the bar's right end on the focused output and takes the keyboard
without a grab: a verb has no pointer press to grab with, exactly as `--quick` does. It works
whether or not the `power` module is in `modules`, and is refused (exit 2) only when no `[quick]`
session command is set or no bar can anchor the menu; both are decided before an open launcher is
closed. A second verb closes the menu, except within 250 ms of its own open (some compositors fire a key bind twice:
Hyprland re-fires a press bind).

Date: 2026-11-21 · Source: tests `the_verb_is_refused_only_without_any_session_command` and `the_verb_is_refused_without_a_bar_to_open_under`; headless `power_verb_opens_and_closes_the_menu`; `TOGGLE_DEBOUNCE` in `launcher/host.rs`

### The resources history is kept always, as whole samples

While the `resources` module is on, the bar keeps a ring of the last 60 samples (about 3 KB, written
in place on the existing tick, with no allocation after start), whether or not the popup is open.
The popup therefore opens on a graph that is already full. A ring filled only while the popup is
open would save those 3 KB but open on an empty graph that takes a minute or two to say anything.
Every pass is stored, including one with no reading, so a missing reading is a gap in the graph and
the time axis never shifts.

Date: 2026-11-15 · Source: tests `the_ring_keeps_the_last_sixty_and_push_reports_last_only` and `the_history_stays_small` (`system.rs`); memory `resources_popup_pss_idle_open_and_closed`

Status: awaiting confirmation at the trial

### The resources popup spans 60 times the interval

The popup's samples come at the same cadence, `[resources] interval`, whether it is open or closed,
so its 60 samples span `60 × interval` seconds and the header reads `last {interval} min` (two
minutes at the default of 2 s, one at `interval = 1`). A faster tick while the popup is open would
mix two spacings in one ring and need the timer re-armed on every open and close.

Date: 2026-11-15 · Source: test `the_header_span_follows_the_interval`

Status: awaiting confirmation at the trial

### Every resources item keeps its block in the popup

Each item of `[resources] show` has its block in the popup, in the order written, whether or not it
has a figure at that moment (a dash and an empty graph stand in), so the popup keeps its size while
it is open and a reading that appears or goes never resizes it.

Date: 2026-11-15 · Source: tests `an_item_with_no_figure_keeps_its_block_so_the_size_never_changes` and `the_default_items_give_a_header_three_blocks_and_the_monitor_row`

Status: awaiting confirmation at the trial

### Each resources graph has a set scale; the network's follows its peak, with a floor

CPU and disk graphs run from 0 to 100 %, memory from 0 to the machine's total, temperature and GPU
from 0 to 100 °C (clamped). The network graph shares one scale between download and upload, the
highest rate of either in the ring, never below 128 KiB/s, so that an idle link does not fill the
graph with noise. A point is in permille of the scale, rounded down, at most 1000.

Date: 2026-11-15 · Source: tests `each_item_scales_to_permille_and_the_edges_never_overflow_or_divide_by_zero` and `the_network_top_skips_a_sample_with_no_reading_and_goes_on`

Status: awaiting confirmation at the trial

### The resources popup has no timer of its own

The popup repaints on each tick of the bar's resources timer, and only while it is the open popup;
a tick with the popup closed paints nothing. It owns no timer, so opening it adds no wake-up, and
the reading is applied to the bar before the popup is repainted.

Date: 2026-11-15 · Source: guardrail `a_resources_tick_repaints_only_an_open_popup_after_applying_the_reading`; memory `at_rest_no_wakeups_after_the_resources_popup`

Status: awaiting confirmation at the trial

### The left click opens the resources popup and the right click runs `[actions] resources`

A left click on the resources module opens the popup, which has a `Monitor…` row that runs
`[actions] resources`; the right click runs the same command directly. It was the left click's job
before the popup existed. With `[actions] resources` empty the popup has no `Monitor…` row.

Date: 2026-11-15 · Source: tests `resources_left_opens_the_popup_right_runs_the_monitor`, `no_monitor_row_without_a_command` and `actions_and_resources_defaults_and_settings`; headless `resources_escape_closes_and_right_click_runs_the_monitor`

Status: awaiting confirmation at the trial

### The active media player is the pinned one, else the best status

A player chosen in the media popup stays the active one while it exists and is not stopped.
Otherwise the active player is the one with the best status (playing over paused; a stopped player
is never shown), and a tie goes to the player whose status changed last. Players the `ignore` list
names are never queried, so they never count. (The reason for the order is not recorded.)

Date: 2026-10-08 · Source: tests `a_pinned_player_stays_active_until_it_stops_or_leaves`, `playing_beats_paused_and_a_tie_goes_to_the_latest_change` and `playerctld_is_always_ignored_then_the_substrings`; `ignored_names_and_playerctld_are_never_queried` (`tests/media_private_bus.rs`)

### One connection owning two MPRIS names keeps the first

Players are matched to signals by the unique name of their connection, because a signal carries
only that. So when one connection owns two MPRIS names, only the first is followed, and the second
is refused like a name already known. A 17th player is refused too.

Date: 2026-10-08 · Source: test `the_17th_player_is_refused_and_a_name_counts_once`; `Players::add`

### The media popup computes the position, and reads it only while open

MPRIS does not signal the playback position. The popup works it out as the last position read, plus
the time since, times the rate, while the player plays, and cuts it to the track's length. The
position is read with one call when the popup or the notification panel opens, on `Seeked`, and
when the status, the track or the rate changes while one is open; closed, it stays unknown. The
`Seeked` rule is on the bus only while one is open, and the 1 s redraw timer exists only while one
is shown and the active player plays: no polling, no wake-up at rest.

Date: 2026-10-08 · Source: tests `the_position_runs_at_the_rate_while_playing_and_is_clamped`, `a_status_track_or_rate_change_forgets_the_position`, `the_tick_runs_only_while_open_playing_and_before_the_end` and `an_unknown_position_never_ticks`

### A player is labelled by its identity, else by its name

The popup's list, and the bar's last fallback for a title, name a player by its `Identity` when it
has one, and otherwise by the first label after `org.mpris.MediaPlayer2.` (`vlc` for
`org.mpris.MediaPlayer2.vlc.instance42`). (The reason is not recorded.)

Date: 2026-10-08 · Source: tests `identity_is_cleaned_and_a_blank_one_is_none` and `the_title_falls_back_to_the_url_then_the_name`

### A paused player's module is fully dim

On the bar, `▶ Title` takes the module's purple (as `module_colors` says) while the player plays;
`⏸ Title` is dim as a whole, whatever `module_colors` says. A state wins over the hue, as it does
for the other modules.

Date: 2026-10-08 · Source: test `media_takes_the_purple_by_module_colours_and_paused_stays_dim` (`segments/tests.rs`)

### Media titles are cut by display width, never inside a cluster

The title is cut to `title_max` cells and ends with `…`. Width is counted per character as the grid
counts it, after putting the text in NFC form, and a cut never falls inside a cluster (a combining
mark, a variation selector or a skin tone after its base, a ZWJ sequence, a flag's two regional
indicators). It is written by hand on the `unicode-width` crate the daemon already uses, so that no
dependency is added.

Date: 2026-10-08 · Source: tests `truncation_counts_cells_like_the_grid` and `truncation_never_splits_a_cluster`; `truncate_title`

### The microphone pill shows only when its mute flips

The OSD shows the default source only when its mute state flips, with the level the source has
then. A change of its level never shows: WebRTC apps move the capture gain during calls, and the
pill would pop all through one. The sound source publishes no microphone level change alone, so a
call wakes nothing. A message that changes both the sink and the microphone shows the sink.

Date: 2026-10-09 · Source: tests `the_microphone_shows_only_when_its_mute_flips` and `the_microphone_and_the_brightness_pills`; headless `osd_the_microphone_and_a_default_sink_switch_show_their_pills`

### A sound change within a second of a start or a new default device shows nothing

The first state after `pw-dump` (re)starts is a baseline and never shows. Each stream (sink,
source) has its own 1 s grace, started by that baseline and by its default appearing, going or being
switched: a change inside it shows nothing, since the levels of a new default settle in silence
(WirePlumber restores them). The end of the second counts as outside it. While the sink's name card
is on screen, a change inside it updates the card. The switch itself shows the card, unless an
earlier grace is running; a card waiting for a volume that arrives inside the second shows then.

Date: 2026-10-09 · Source: tests `nothing_shows_within_the_grace_and_a_change_at_its_end_does`, `the_first_state_is_the_baseline_and_a_restart_takes_a_new_one`, `a_shown_card_follows_a_switch_and_a_level_inside_the_grace` and `a_switch_before_its_volume_shows_the_card_once_the_volume_is_known`; `side` in `osd/mod.rs`

### The OSD counts a volume change in whole percent

A change is worth a sink pill when the mute flipped or the integer percent differs. The sound model
publishes whole percents (as wpctl and the bar show them), so a level that moved by less than half
a point never reaches the OSD. Carrying tenths would publish, and wake the loop, for changes nobody
can see.

Date: 2026-10-09 · Source: test `only_a_moved_level_or_a_flipped_mute_shows`

### Brightness is shown on request, read once

The brightness pill comes only from `alpheus-launch --osd brightness`, which reads the backlight's
`brightness` and `max_brightness` once, then shows the pill. Nothing watches the backlight and
the OSD never writes it (the quick panel's slider changes it, through `brightnessctl`): the user's
brightness key keeps changing it the same way and then calls the verb. A watcher would pop the pill each time hypridle dims and restores the
screen.

Date: 2026-10-09 · Source: test `the_backlight_level_is_a_small_integer_or_nothing`; headless `osd_the_brightness_verb_reads_the_fake_backlight_and_shows_the_pill` and `osd_without_a_backlight_or_the_module_the_verb_exits_0_and_shows_nothing`; guardrail `the_osd_only_reads`

### The OSD pill is an Overlay surface with an empty input region, made on show

The pill is one layer surface on the Overlay layer (Hyprland draws the Top layer under a fullscreen
window), anchored at the bottom with an exclusive zone of 0, taking no keyboard. Its input region is
empty, so it never takes a click; if the empty region cannot be made, nothing is shown, since a
surface without one would take every click. It is created when a pill is shown and dropped when it
goes, with its own pool, so that nothing of it stays at rest.

Date: 2026-10-09 · Source: guardrail `the_osd_and_the_bubbles_are_created_on_the_overlay_layer`; headless `osd_the_pill_never_takes_a_click` and `osd_pss_idle_shown_and_gone` (`tests/memory.rs`)

### The pill follows the focused output only when its content changes

A pill is created on the focused output. While it is shown it moves to a newly focused output only
when its content changes, as the notification bubbles do (`bubbles::moves`); a change of focus alone
leaves it where it is.

Date: 2026-10-09 · Source: headless `osd_the_pill_stays_put_when_the_focus_moves_until_its_content_changes`

### The OSD logs once when its surface appears and once when it goes

One log line when the surface appears (with what it shows) and one when it goes (with how many
changes it showed), never one per change: a held key would write 25 lines a second to the journal.
No line names a device (a card's label stays out of the journal).

Date: 2026-10-09 · Source: test `each_pill_is_described_for_the_log`; `describe` in `osd/mod.rs`

### A tray menu is shown only once its layout has arrived

Pressing an icon asks for its menu, but nothing is drawn until the menu's first layout is back, and
if it takes more than 2 s nothing is drawn at all. A half-built menu is never shown.

Date: 2026-10-05 · Source: test `no_answer_within_2_s_closes` (`menu_session/tests.rs`); headless `a_menu_answering_after_2_s_is_not_shown` and `a_silent_menu_never_opens_and_leaves_nothing_pending`

### A tray menu opens with its first entry selected

The first selectable entry is selected before the popup is first drawn, so Enter or a key press has
something to act on at once.

Date: 2026-10-05 · Source: tests `arrows_skip_separators_and_disabled` and `relayout_keeps_selection_by_id` (`session_bus/menu/tests.rs`); headless `a_fake_apps_menu_opens_runs_an_item_and_closes`

### A submenu opens in the same popup

A submenu replaces the menu's current level inside the same popup (→ or Enter enters it, ← goes back
one level), with the first row showing `◂ <parent label>`. No cascading popups are opened.

Date: 2026-10-05 · Source: tests `submenu_level_with_its_header`, `enter_or_right_on_a_submenu_asks_for_it` and `left_goes_back_one_level_escape_closes` (`session_bus/menu/tests.rs`); headless `a_submenu_opens_with_right_and_back_with_left`

### A failed Activate opens the menu of the icon that was pressed

A left click sends `Activate`; when the item answers with a D-Bus error (Steam's libappindicator has
no such method) the bar opens its menu instead, if the item has a `Menu`, else nothing. That answer comes later, so the bar remembers the icon,
the cells it occupies and the button that pressed it, and the menu belongs to that press, not to
whatever is under the pointer when the answer arrives. A press that lands on no icon forgets the
remembered one, so a late answer never opens a menu for an icon the user has left. A timeout is not a
failure, because a slow application may have acted.

Date: 2026-10-05 · Source: tests `a_failed_activate_opens_the_menu`, `a_failed_activate_without_menu_does_nothing`, `a_timed_out_activate_does_not_open_the_menu` and `the_remembered_press_is_consumed_by_the_menu_it_opens`; headless `a_late_failed_activate_after_clicking_elsewhere_opens_nothing`

### A tray menu is closed by the button that opened it, or by any right click

A press on the icon whose menu is open only closes that menu, and does nothing else, when it is made
with the button that opened it, or with the right button whatever opened it. So Steam's menu, opened
by a left click, closes with the same left click, with no new Activate and no flicker.

Date: 2026-10-06 · Source: headless `a_second_left_click_on_steams_icon_only_closes_its_menu` and `a_second_right_click_closes_the_menu`

### A press right after the compositor closed a menu only closes it

A compositor may deliver `xdg_popup.done` before the press that follows it reaches the bar. For 300 ms
after such a close, a press on the same icon that would have closed the menu (its opening button, or a
right click) still only closes: it does not reopen what was just closed.

Date: 2026-10-06 · Source: `RECENT_CLOSE_WINDOW` and `recently_closed` (`tray/menu_app.rs`); headless `a_quick_right_click_after_closing_reopens_the_menu` (the case where the press itself closed the menu)

### A late submenu graft is refused once the selection has moved

A submenu's children are fetched when it is opened, and entered when they arrive. If by then the
user has moved the selection, gone back, asked for another submenu or run an entry, the fetch is
dropped. Steam and Electron send `LayoutUpdated` often enough that a late graft could otherwise jump
into "More" long after the user looked elsewhere.

Date: 2026-10-06 · Source: tests `a_submenu_fetch_grafts_only_while_still_selected` and `a_menu_already_asked_for_an_icon_is_not_asked_again` (`tray/menu_app/tests.rs`); `submenu_still_selected`

### A burst of tray changes is redrawn once per frame callback

A signal such as `NewIcon` only marks a property dirty. The bar asks for one frame callback on the
first bar that can take one, and re-reads every dirty property once when it comes. The request is
committed with one pixel of buffer damage on every compositor, because an undamaged commit may get no
frame callback at all (Hyprland) or one throttled to about a second (niri).

Date: 2026-10-05 · Source: tests `a_burst_marks_dirty_and_rereads_once_per_frame` and `a_storm_of_layout_updates_is_one_read_per_frame`; `a_burst_of_signals_gives_one_reread_and_one_redraw` (`tests/tray_private_bus.rs`)

### A closed session bus is never reconnected

When the session bus closes, the tray is dropped and left empty, one line is logged, and no
reconnection is tried: a retry loop would wake the bar at rest. The media and notification
consumers that live on the tray's connection go with it.

Date: 2026-10-05 · Source: tests `a_call_after_the_bus_closed_is_disconnected_with_no_timer` and `the_bus_going_away_is_closed_once` (`tests/tray_private_bus.rs`); headless `a_closed_session_bus_empties_the_tray_once`; `tray_output`

### The microphone lights for a recording stream, never a meter or a filter

The microphone indicator lights for a link from an audio source to a `Stream/Input/Audio` where the
link is active or the stream runs. It is not lit for a stream that is a meter (`stream.monitor`, such as
pavucontrol's), one that reads a sink's monitor (`stream.capture.sink`, the visualisers), a member of
a `node.link-group` (filter chains, loopbacks and echo cancellation, which forward sound and do not
record) or a passive stream.

Date: 2026-10-16 · Source: tests `the_microphone_is_a_source_linked_to_a_recording_stream` and `meters_visualisers_filters_and_passive_streams_never_light_it`; headless `privacy_a_meter_never_lights_it`

### A muted app stream still lights the microphone

The model has no per-stream mute: a stream that is linked and running holds the device, so the
indicator stays lit even when the app muted its own stream, since the app still holds the
microphone. Only the default source's mute changes how it is drawn: the glyph is dim, and so is the
popup's heading.

Date: 2026-10-16 · Source: tests `an_app_muted_stream_still_lights_the_microphone`, `the_microphone_dims_with_the_default_source_mute` and `the_popups_width_counts_the_glyph_of_a_heading` (`privacy/tests.rs`; the note "The model has no per-stream mute" is in the first)

Status: awaiting confirmation at the trial

### A running camera with no link lights it unnamed

When a device video source (a v4l2 or libcamera device, or a source with the Camera role) is running
and no link from it is seen, the camera indicator is lit with no app name. The same holds for a
running screen producer, as long as no link has named a screen holder.

Date: 2026-10-16 · Source: tests `a_running_camera_with_no_link_lights_it_unnamed` and `a_camera_is_a_device_video_source_linked_to_a_video_stream`; the screen case is `capture` in `privacy/mod.rs`

### An indicator going off stays shown for half a second

An indicator that is no longer live stays shown for 0.5 s, and one that comes back within that time
never blinked: a stream that renegotiates (a call changing codec, a share switching window) drops
for a few batches. Lighting up is immediate. One deadline timer, armed only while an indicator is
pending off, serves every indicator.

Date: 2026-10-16 · Source: tests `lighting_up_is_immediate_and_going_off_waits_the_delay`, `coming_back_within_the_delay_never_blinks` and `a_later_setter_never_pushes_the_deadline_back`; guardrail `the_privacy_timer_is_bar_level_and_fired_before_due` (the figure is GNOME's)

### A compositor's screencast release counts after a one-second hold

Hyprland sends a release about 500 ms after the last frame of a cast, and a screenshot gives a short
blip, so its release counts only after a hold of 1 s, after which the 0.5 s off-delay applies (1.5 s
in all). niri's hold is zero. A cast that comes back during the hold never blinks the screen.

Date: 2026-10-16 · Source: tests `hyprland_releases_count_after_the_hold`, `a_zero_hold_goes_off_at_release_plus_the_delay` and `a_cast_coming_back_during_the_hold_never_blinks`; headless `privacy_hyprland_screencast_lights_the_screen`

Status: awaiting confirmation at the trial

### A screen lit by the compositor alone reads `(unnamed)`

The compositor's cast events say that a screen is captured and what is captured, never who captures
it. The screen indicator is lit when PipeWire or the compositor says so, and the popup's names come
from PipeWire; a screen lit by the compositor alone shows `(unnamed)`.

Date: 2026-10-16 · Source: tests `the_screen_merges_pipewire_and_the_compositor`, `a_portal_or_niri_producer_is_the_screen` and `a_release_adds_no_deadline_while_pipewire_holds_the_screen`

### The privacy popup lists apps and selects nothing

The popup has one heading per lit indicator and one row per app, and no row can be selected: Escape
closes it and every other key, the wheel and a click inside do nothing. While open it follows what is
shown, and it closes (`nothing captured`) once nothing is shown, after the off-delay. It is at most 32
cells wide; a longer name is cut.

Date: 2026-10-16 · Source: tests `the_popup_lists_each_indicator_and_its_apps` and `the_popup_closes_when_nothing_is_captured_after_the_delay`; headless `privacy_the_popup_closes_when_the_capture_ends` and `privacy_left_click_lists_the_app_and_middle_click_mutes`

Status: awaiting confirmation at the trial

### The camera is read from `/proc` after a trigger, never counted

An open or a close of a `/dev/video*` node, a node appearing or going, or a lost event queue only
means "look again". What is shown comes from one scan of `/proc`, because a counter of opens drifts
(descriptors inherited by `fork` or `dup`, opens made before the daemon started, an overflowing event
queue). The scan reads the links of `<pid>/fd` of the daemon's own user, never opens anything under
`/dev`, and names a holder by its `comm`.

Date: 2026-10-20 · Source: tests `a_holder_is_named_by_its_comm`, `a_process_of_another_user_is_skipped`, `an_open_and_a_close_of_a_node_are_triggers` and `a_node_touched_again_is_not_counted_twice` (`privacy/camera.rs`); guardrail `the_camera_watch_opens_nothing_under_dev`

### A burst of camera triggers is scanned at most one second after its first

A trigger moves the scan 300 ms ahead, so that an open and its close (a browser listing the cameras)
scan once and find nothing. The scan still runs at the latest 1 s after the first trigger of the
burst, so that a process that opens and closes a node non-stop cannot postpone it for ever.

Date: 2026-10-20 · Source: test `a_steady_stream_of_triggers_is_scanned_within_the_max_wait` (`SCAN_MAX_WAIT`); headless `privacy_a_camera_blip_lights_nothing`

Status: awaiting confirmation at the trial

### PipeWire's own hold on a camera adds no name

A holder whose `comm` is `pipewire` or `wireplumber`, and the daemon itself, are left out of the
scan. PipeWire's hold on the device is already named by the client that reads the camera through it,
which the PipeWire graph names.

Date: 2026-10-20 · Source: test `pipewire_wireplumber_and_the_daemon_itself_are_not_holders`

### A video node udev has not opened to the user yet is watched again when its attributes change

A node udev has just created may belong to root alone, so watching it is refused. The refusal is
kept, and the watch is tried again when the name's attributes change (the group and ACL arriving), or
when it is created again, and forgotten when the name goes. A refusal because of a permission is not
logged.

Date: 2026-10-20 · Source: tests `a_refused_node_is_retried_on_its_attrib` and `a_watched_node_at_the_limit_keeps_its_watch_on_attrib`; `CameraWatch::watch_node` (the permission refusals not logged)

Status: awaiting confirmation at the trial

### The camera scan runs on the event loop

The scan is synchronous on the bar's event loop and not on a thread of its own. The rule is that a scan of 10 ms or less stays on the loop; it is judged on a harness of 120 processes of 30 descriptors (the shape of the workstation's own
user: 93 processes, 2,726 descriptors), where the median is 4.0 to 4.1 ms; beyond 10 ms the scan
would have moved to a short-lived thread. A 500 by 20 tree, printed only, reads a median of
14 to 15 ms and a maximum near 20 ms (`camera_scan_duration`).

Date: 2026-10-20 · Source: guardrail `the_camera_tick_fires_before_scanning`; the measurement `camera_scan_duration` (`tests/memory.rs`)

### The camera watch is a second inotify instance

The watch on the video devices has its own inotify instance, apart from the theme watch's. This keeps
the theme watch untouched and the two lifetimes apart, at the cost of one file descriptor. An earlier
design had planned to use the existing instance.

Date: 2026-10-20 · Source: guardrails `the_theme_and_camera_watches_only_read` and `the_camera_watch_opens_nothing_under_dev`; headless `privacy_camera_pss_and_rest` (`tests/memory.rs`)

### An unknown Shield state is never read as off

The mode is on while the flag file exists, off when it does not, and unknown when its existence cannot
be told (any error but "not found"). Unknown is not off: the switch reads `·`, the bar draws nothing,
Space does nothing and `toggle` answers an error (exit 2), and the detail says `Shield state unknown` instead of `Shield is off`.
The rule is "never "off" on a guess", the one `[quick]`'s `file:` states already follow.

Date: 2026-11-21 · Source: tests `read_flag_tells_missing_from_unreadable`, `an_unknown_state_is_an_error_but_for_status` and `unknown_state_starts_nothing`; `a_click_on_the_row_switches_to_the_other_state_and_does_nothing_when_unknown` (`shield/app/tests.rs`)

Status: awaiting confirmation at the trial

### A Shield switch succeeds when the flag reaches its target

A switch to on or off is judged by the flag file, not by the command's exit code: it succeeds when the
flag reaches its target, before or after the command exits. A non-zero exit fails at once, after one
look at the flag; an exit 0 gives the flag 5 s; a command still running after 60 s fails the switch
(without being killed). The flag is the truth: a change that arrives late clears `failed` and is
announced like any change.

Date: 2026-11-21 · Source: tests `the_attempt_ends_on_the_flag_the_exit_or_the_deadline`, `the_deadline_follows_the_grace` and `a_late_change_clears_failed`

Status: awaiting confirmation at the trial

### A running Shield command is never killed at its deadline

The 60 s deadline judges a switch and kills nothing. A command's output goes to `/dev/null` and it is
waited for with no bound of its own: a pipe that alpheus closed at a deadline or at shutdown would
kill a privileged helper on its next write, half way through its work (a setuid `pkexec` cannot be
signalled, but SIGPIPE reaches it). At shutdown the daemon stops only the commands it can still
reach; one already running as root finishes.

Date: 2026-11-21 · Source: guardrail `a_shield_action_is_never_a_capture_and_shutdown_stops_the_commands`; test `a_late_done_frees_the_slot_only`

Status: awaiting confirmation at the trial

### Shield commands and reads run on a short-lived thread

`on`, `off` and `allow` (through `run_silent`) and the listing of blocked devices (through `capture`)
each run on a thread that lives only while the job runs, and mail the outcome back. For the three
commands: a `failed` on a non-zero exit is wanted, and only a command that is waited for gives an exit
code; a detached spawn never sees one. For the listing: `capture` blocks up to its deadline, so it runs
off the event loop (the comment on `only_the_runners_capture_and_never_through_a_shell`).

Date: 2026-11-21 · Source: tests `the_worker_delivers_once_and_ends`, `a_panicking_worker_still_reports` and `a_worker_starts_only_when_the_loops_mail_channel_exists` (`shield/run/tests.rs`, `shield/app/tests.rs`); guardrail `only_the_runners_capture_and_never_through_a_shell`

Status: awaiting confirmation at the trial

### A Shield verb for the state already held runs nothing

`alpheus-launch --shield on` while the mode is on (and `off` while off) answers `Done` and runs no
command. Running the configured `on` again could repeat a real effect (for example,
regenerating the USB policy), which a stray key should not cause.

Date: 2026-11-21 · Source: test `on_and_off_in_the_asked_state_run_nothing`

Status: awaiting confirmation at the trial

### The picker line starts with the checked vendor and product ids

A blocked device is shown as `<vid:pid>  ·  <name>`, with no USBGuard id. The ids are checked text and
come first, so a hostile name ending in `(1234:5678)` cannot pose as them; the device id means nothing
to the user and stays inside the daemon. `status` prints the same line.

Date: 2026-11-21 · Source: tests `the_picker_line_has_the_name_and_ids_but_not_the_device_id`, `a_serial_cannot_pose_as_the_name` and `the_picker_lines_ids_and_names_go_together`

Status: awaiting confirmation at the trial

### The blocked USB devices are read when asked, never watched

The list is read once each time the detail opens, once after an allow, for `status` and for the
picker; never when a device is plugged, and nothing is read while the mode is off. Reopening the
detail is the refresh. The design rejects a notice on every plug: that is background watching, and
allowing stays a deliberate gesture.

Date: 2026-11-21 · Source: tests `status_reads_the_list_only_while_on_and_listable`, `a_read_does_not_hold_the_action_slot` and `one_read_at_a_time`; headless `shield_the_detail_lists_and_allows` and `shield_at_rest_no_wakeups`

Status: awaiting confirmation at the trial

### The Shield flag may not share a directory with alpheus's configuration

The flag's directory and its parent may equal neither the configuration directory nor its parent;
otherwise Shield mode is turned off with one warning at start. The theme target and the flag target
would then share inotify watch descriptors (one inode, one descriptor), and re-arming one would drop
the other. An alias the paths do not show (a symlink, a bind mount) is caught when the watch arms: the
flag is then not watched and the unwatched mode applies.

Date: 2026-11-21 · Source: tests `a_flag_sharing_the_config_directory_is_refused` and `the_flag_location_is_compared_after_lexical_normalisation` (`startup/tests.rs`); `resolve_shield`

Status: awaiting confirmation at the trial

### The Shield flag is watched in the theme watch's inotify instance

The flag's directory is a second target of the theme watch's one inotify instance, so Shield adds no
descriptor, thread or timer (no new descriptor, thread or timer). The instance is
created when either target needs it, and one target failing to arm leaves the other working. This is
the opposite of the camera watch, which has an instance of its own.

Date: 2026-11-21 · Source: guardrail `the_shield_watch_is_the_theme_watchs_instance`; tests `the_flag_counts_as_watched_only_when_the_theme_watch_armed_its_target` and `an_unconfigured_shield_adds_no_watch`

Status: awaiting confirmation at the trial

### The displays are changed by the daemon, and its trial lives there

The resident daemon, not the control center, speaks the compositor's output protocol and holds the
15 s "keep these settings?" trial. The hotplug behaviour needs the daemon resident anyway, the control
center must stay free of Wayland (a boundaries test), and a control center that dies mid-trial can no
longer leave a broken layout, because the daemon reverts at the deadline.

Date: 2026-10-30 · Source: test `the_trial_reverts_at_the_deadline_without_the_tui` (`outputs/control/tests.rs`); guardrail `the_displays_are_restored_before_the_app_is_dropped`; the boundaries tests of alpheus-common and the control center

### displays.toml is read at each evaluation of the profiles, never watched

The daemon reads `displays.toml` each time the controller evaluates the profiles (`displays_tick`): when
the debounce that follows a change of screens ends (the first one follows the first `done` at start), and
when the re-check of a profile skipped by the 3-a-minute limit comes due. It does not watch the file and
does not keep it between evaluations (reason not recorded).

Date: 2026-10-30 · Source: tests `a_burst_of_dones_evaluates_once_after_the_debounce`, `a_skipped_profile_is_retried_when_the_window_ends` and `displays_toml_is_read_capped_from_a_regular_file_only`; `displays_tick`

### A DisplayLink screen is told from the DRM directory, once per screen name

A screen is DisplayLink when its connector's DRM card is driven by `evdi`: the `card*-<name>` entry of
`[displays] drm_dir` (default `/sys/class/drm`) whose driver link ends in `evdi`. The answer is read once
for each new screen name and never watched, and nothing under `/dev` is opened (`is_displaylink` reads
only `drm_dir`). The directory is configurable so that tests can point elsewhere (reason for reading once:
not recorded).

Date: 2026-10-30 · Source: tests `displaylink_detection_reads_the_driver_link`, `displaylink_is_read_once_per_new_head_name` and `a_driver_name_must_end_in_evdi_and_an_unreadable_name_is_not_displaylink`; `is_displaylink`

### A screen's identity and a profile file's errors are never logged

A profile's name is the user's own word and may be logged (`displays: profile "<name>" does not resolve`);
a screen's identity is device data and never is, and an invalid `displays.toml` is logged as one fixed line
with no error text, because the error may quote a screen's identity.

Date: 2026-10-30 · Source: tests `an_invalid_displays_toml_is_logged_once_per_change` and `a_profile_that_does_not_resolve_names_itself`; the comment on `LOG_INVALID_FILE` in `outputs/app.rs`

### The Tools menu reaches the clipboard, the shield and the diagnostics tab through one file

The three features were built in parallel with the menu. `launcher/tools/join.rs` is the only file of the
menu that calls their own functions and types, with one function each for "does its row show" and "open
it" (the diagnostics tab is a constant). `app.rs` has a `Join` struct carrying the three answers and
reads that constant. The guardrail `the_parallel_branches_are_named_only_in_tools_join` looks in the
folder's other files for `clipboard_availability`, `launcher_open_clipboard`, `shield_open_detail`,
`ShieldConfig` and the `"diagnostics"` slug. All three are real since the merges.

Date: 2026-11-21 · Source: guardrails `the_parallel_branches_are_named_only_in_tools_join` and `the_join_hooks_are_filled_in`

Status: awaiting confirmation at the trial

### A built-in replays the daemon's own verb after the box has closed

A built-in that opens a feature (the quick panel, Notes, Timer, the power menu) calls the daemon's own
verb internally, with no process and no socket round trip, and does so after the launcher's box has
closed, in the same pass (the reason for replaying after the close is not recorded). The verbs open
their popups with the keyboard taken as a socket verb would, which is why a held Enter does nothing in
the Tools mode: the repeat of the key that ran a built-in must not act in the popup it opened.

Date: 2026-11-21 · Source: headless `tools_opens_and_a_builtin_runs_after_the_box_closed`, `tools_wifi_builtin_opens_the_quick_detail_and_the_repeat_stays_out` and `tools_a_held_enter_drills_once_and_runs_nothing`; test `a_held_enter_does_nothing_in_tools`

Status: awaiting confirmation at the trial

### Every action of the Tools menu closes the box, toggles included

Running an entry, opening a built-in's feature and flipping a toggle all close the box; the switch is not
redrawn in place. Only opening a level, going up, a confirmation question and a missing program (said in
the box) keep it open. The reason is not recorded.

Date: 2026-11-21 · Source: tests `enter_on_a_plain_tool_runs_it_and_a_builtin_is_handed_out` and `confirm_asks_waits_runs_and_expires`; headless `tools_choices_cmd_filters_and_runs`

Status: awaiting confirmation at the trial

### A tab opens through `[actions] open_tab`, else in the terminal

A built-in for a control center tab runs `[actions] open_tab` with `{tab}` replaced by the tab's name, when
its program is on the `$PATH` of the open. When it is not, it runs the launcher's terminal with
`alpheus --tab <name>`. When neither program is there, the box says `program not found` and stays open
(the launcher's own rule for a missing program). An empty `open_tab` never reaches
this point: `bar.toml` refuses it. Why the terminal is the fallback is not recorded.

Date: 2026-11-21 · Source: tests `a_tab_opens_through_open_tab_or_the_terminal` and `a_tab_with_no_program_to_open_it_names_the_open_tab_program_else_the_terminal`; headless `tools_tab_builtin_runs_open_tab_or_falls_back`

Status: awaiting confirmation at the trial

### Everything a built-in spawns runs in its own scope

A command spawned for a built-in (a tab, a toggle, Lock, Reload the bar) runs in its own
`systemd-run --user --scope`, bare when there is no systemd, and its program is looked up first. The reason
is that `[tools] reload` (for example `shell-ctl reload-bar`) must survive the daemon's own control
group being killed.

Date: 2026-11-21 · Source: tests `a_built_in_that_spawns_runs_in_its_own_scope`, `the_glues_spawn_follows_the_runtime_dirs_systemd` and `builtin_argv_is_only_the_configured_command`; guardrail `the_tools_glue_spawns_only_through_launch`

Status: awaiting confirmation at the trial

### The Tools menu reads states and choices one job at a time on a short-lived thread

The state of each entry and the lines of a `choices_cmd` are read on a thread of their own that lives only
during a read, one job at a time, shaped like the launcher's scan thread. The `--tools` open runs no apps
scan, so the reader is its own thread. States are delivered one entry at a time; a choices read that
a drill-in asks for goes first, the states read stops after its current entry and its unread entries wait
behind it; a delivery for a closed open still frees the slot.

Date: 2026-11-21 · Source: tests `one_job_at_a_time`, `states_arrive_one_by_one` and `preempt_stops_after_the_current_entry_and_returns_the_rest` (`launcher/tools/read/tests.rs`); guardrail `the_tools_reads_belong_to_the_tools_mode_and_stop_with_the_box`

Status: awaiting confirmation at the trial

### The notification server queues behind another server and never replaces it

alpheus asks for the name `org.freedesktop.Notifications` with flags 0: it waits in the queue while
another server (mako, for example) owns the name, and it is neither allowed to replace that server nor
replaceable by one. While the name is not held, alpheus answers no call and shows no bubble; the bell
shows only the history it already has. If alpheus exits, mako's D-Bus activation
brings mako back on the next `Notify`; this fallback is intended.

Date: 2026-10-09 · Source: tests `queued_behind_mako_nothing_is_served_until_it_releases_the_name`, `a_mako_started_late_is_refused_and_alpheus_keeps_the_name` and `mako_disconnecting_hands_the_name_over_too` (`tests/notifications_private_bus.rs`); headless `mako_holding_the_name_dims_the_bell_until_it_stops`

### A bubble's timeout follows its urgency

A critical notification has no timeout (its bubble stays until clicked). For any other, an `expire_ms`
of -1 (or any negative number) means `[notifications] timeout` (5 s), 0 means `timeout_max` (20 s), and
any other value is clamped to `timeout_min..=timeout_max` (2 to 20 s). The reason for these numbers is
not recorded.

Date: 2026-10-09 · Source: test `a_bubbles_timeout_follows_its_urgency_and_the_apps_own_choice`

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

Date: 2026-10-09 · Source: tests `ten_bubbles_then_one_per_second_and_critical_bypasses_the_bucket`, `a_critical_one_gets_a_bubble_at_the_live_cap`, `a_flood_of_criticals_never_blocks_a_real_one`, `replacing_a_live_bubble_under_do_not_disturb_keeps_it_live_with_a_new_expiry`, `a_mute_replacing_a_live_bubble_closes_it` and `do_not_disturb_filters_before_it_takes_the_visible_ones`

### A bubble's timeout starts when it first shows

A bubble beyond `max_visible` waits with no timer and gets its whole timeout when it first shows. One
that the host has drawn keeps its expiry when newer bubbles push it back: a steady flow of notifications
used to push every shown bubble back before it expired, so nothing expired. Under
do-not-disturb a live bubble that is hidden still has its timer and closes on time. Waiting bubbles
used to count down too; they no longer do.

Date: 2026-10-09 · Source: tests `a_waiting_bubble_gets_its_full_timeout_once_shown`, `a_bubble_pushed_back_after_it_showed_still_expires_on_time` and `a_steady_flow_keeps_expiring_and_a_critical_still_shows`; headless `under_dnd_a_hidden_replaced_bubble_still_expires_on_time`

### An identical notification within one second is merged, unless it would add a bubble

With no `replaces_id` and no tag match, a notification with the same identity, summary, body and
urgency as an entry that arrived less than 1 s before is the same notification: its id is returned and
nothing else changes, and each merge slides the window (so an app looping on one text stays one entry).
A merge spends no token. It does not swallow a bubble the first one never got (do-not-disturb, an empty
bucket) when this one would get one.

Date: 2026-10-09 · Source: tests `an_identical_notification_within_one_second_merges`, `a_merge_never_swallows_a_higher_urgency_nor_a_bubble_owed` and `a_muted_duplicate_still_merges`

### A notification's text is cleaned in one linear pass

Every raw field is first cut to 16 KiB, so the rest of a hostile body is never looked at, and markup is
stripped in one pass: a tag left unclosed is text and so is every `<` after it, so no second scan is
ever made. A `<br>`, with spaces or attributes too, is a line break (test
`a_br_with_spaces_or_attributes_is_a_line_break`). Only the summary and the body are stripped;
`<br>` is a line break in the body only (in the summary, a space).

Date: 2026-10-09 · Source: tests `hostile_markup_never_hides_text_nor_panics`, `a_100_kb_field_keeps_its_cap_cut_on_a_character`, `markup_is_stripped_and_the_five_entities_decoded` and `a_br_with_spaces_or_attributes_is_a_line_break`

### An unknown `replaces_id` is accepted as that id

A non-zero `replaces_id` that matches no entry is taken as the new notification's id, because Electron's
tag ids rely on it.

Date: 2026-10-09 · Source: test `ids_count_from_1_and_an_unknown_replaces_id_makes_the_counter_jump`

### A rule is matched on the whole app name, any case

A `[[notifications.rules]]` entry matches when its `app` equals, in full and ignoring case, the sender's
app name or its desktop entry. `drop` answers the sender (with its `replaces_id` when non-zero, else a
fresh id) and stores nothing; `mute` with any urgency still means no bubble. Battery alerts go through
the same path, so a rule on `alpheus` applies to them.

Date: 2026-11-15 · Source: tests `rules_match_the_whole_name_in_any_case`, `rules_match_the_desktop_entry` and `a_dropped_app_is_answered_but_never_stored`

Status: awaiting confirmation at the trial

### The do-not-disturb file holds one of three forms

`$XDG_STATE_HOME/alpheus/dnd` is empty (on, untimed), `until <unix seconds>` or `sharing`. Any file still
means "on" to the shell scripts that read only whether it exists. At start, an end already passed reads
as off, `sharing` reads as off (a capture still running turns it back on when evaluated again), and
contents that cannot be read read as on, because an existing file was always "on". When what was read
differs from what is saved, the file is rewritten or removed at once.

Date: 2026-11-15 · Source: tests `loaded_drops_a_past_end_and_a_sharing_state`, `saved_forms_round_trip` and `an_end_with_a_sign_or_no_space_after_until_is_unreadable` (`notify/dnd.rs`); headless `a_past_or_sharing_dnd_file_is_cleaned_at_start`

Status: awaiting confirmation at the trial

### The user's choice wins over a capture

Every user path clears "by sharing". The bell's middle click, the panel's row and the quick row's Space
set an untimed state; `--dnd on` on a timed state makes it untimed; `--dnd on --for`, `--dnd on --until`
and the quick panel's duration detail make it timed; `toggle` on a timed one turns it off.
`dnd_while_sharing` reacts to edges only (a capture starts, a capture ends) and only with the `privacy`
module. A timed end reached while a capture is shown and the option is on hands over to "by sharing", so
the capture's end turns it off as usual; this was settled at the final review of that delivery (it used
to turn off).

Date: 2026-11-15 · Source: tests `the_user_clears_the_end_and_sharing`, `sharing_turns_on_only_from_off_and_turns_off_only_its_own`, `an_end_during_a_share_hands_over_to_sharing` and `sharing_reacts_to_edges_only`; headless `a_timed_dnd_ending_during_a_share_hands_over_to_sharing` and `sharing_without_the_notifications_module_leaves_dnd_alone`

Status: awaiting confirmation at the trial

### A timed do-not-disturb ends on the wall clock

The end's timer is monotonic and does not count the time a machine is suspended, so the clock's minute
tick also checks the wall-clock end: after a resume, do-not-disturb ends at most one clock tick late. When
the timer fires and the wall clock says the end is not reached yet, the timer is armed again for the rest.

Date: 2026-11-15 · Source: tests `a_timed_state_goes_off_once_its_end_is_reached` and `the_wait_for_an_end_is_clamped_so_it_never_overflows_an_instant`; headless `a_timed_dnd_is_saved_restored_and_ends`

Status: awaiting confirmation at the trial

### A battery alert is given only while the server serves

An alert is sent only while alpheus's own server holds the name; before that nothing is logged and the
threshold stays armed, so the alert is offered again, with the last reading, when the server starts.

Date: 2026-11-15 · Source: test `an_alert_while_not_serving_waits_for_the_server`; `BatteryAlerts::feed`

Status: awaiting confirmation at the trial

### The bubbles are an Overlay surface, opened again once per stack

The bubbles are one `Overlay` layer surface, not `Top`, because Hyprland draws `Top` under a fullscreen
window. When the compositor closes the surface it is opened again once for that stack of
bubbles (their ids and revisions); closed again for the same stack, it stays closed until the stack
changes, so there is never a create-and-close loop. A close followed in the same dispatch batch by an
output going is that output's loss (an unplug) and does not spend the one reopen.

Date: 2026-10-09 · Source: guardrail `the_osd_and_the_bubbles_are_created_on_the_overlay_layer`; tests `a_compositor_close_reopens_once_per_stack` and `a_close_from_an_unplug_is_not_the_stacks_one_reopen` (`notify/bubbles_surface.rs`)

### The bubbles move to the focused output only when the stack changes

When the focus moves to another output, the bubbles stay where they are until the stack itself changes
(a bubble arrives, expires or is replaced); only then are they placed on the focused output, and only if
that output is known by name and is not the one they are on. The code gives the reason: a bubble must
never jump under the pointer because the focus followed the mouse.

Date: 2026-10-09 · Source: test `the_stack_moves_to_the_focused_output_only_when_it_changes`; headless `the_bubbles_stay_put_when_the_focus_moves_until_the_stack_changes`; `bubbles::moves`

### A slider sends one level per 150 ms window and keeps showing it

The volume and the brightness send absolute levels, at most one command per 150 ms window: a key, a
wheel notch or a click inside a window only moves the level shown, and the latest level goes when the
window ends. After a quiet window the level sent stays shown, and is stepped from, until pw-dump
reports it or 1 s has passed, because a slow `wpctl` or `brightnessctl` would otherwise make the next
press step from a stale level. A default sink switch drops a held or waiting volume
level: it is never sent to the new sink. The pacing is for a held key; why 150 ms is not
recorded.

Date: 2026-10-11 · Source: tests `a_held_slider_key_sends_the_latest_level_once_per_window`, `a_default_sink_switch_drops_the_held_volume` and `brightness_shows_the_expected_level_until_its_re_read`; `Pace`, `COALESCE`, `SETTLE`

### The quick panel never sets the backlight below 1 %

A slider move on the brightness row stops at 1 %, so the panel can never black the screen. The
guardrail on the commands the panel may run pins the same floor.

Date: 2026-10-11 · Source: test `brightness_never_goes_below_one_percent`; `BRIGHTNESS_MIN`

### A click on a slider's bar sets the level of the cell

The bar is 22 cells. A press on a cell sets `(column − bar start + 1) × 100 / 22` percent: the first
cell is 4 % and the last 100 %. Why the first cell is not 0 % is not recorded.

Date: 2026-10-11 · Source: tests `volume_steps_mutes_and_takes_a_click_on_its_bar`, `a_click_beside_the_volume_bar_sets_no_level` and `the_drawn_bar_spans_bar_start_and_its_bar_cells`; `bar_value`

Status: awaiting confirmation at the trial

### The selected slider row is banded across the whole row

The selection band covers a slider's whole row, the bar included; the bar's cells keep their own
foreground colours on it (accent, dim, warning), so the level stays readable. A band that skipped the
bar looked broken.

Date: 2026-10-11 · Source: test `the_rows_style_their_switches_and_tracks_in_warn_accent_and_dim`; `slider` in `draw.rs`

Status: awaiting confirmation at the trial

### Only the arrow keys repeat in the quick panel

Held down, only ↑ ↓ ← → (and Ctrl+P, Ctrl+N) repeat, and Backspace in the Wi-Fi password field. A held
Space or Enter acts once, so a held key never flaps a mute or confirms a reboot. Two later cases
follow from it: a held ← or → on the headset row sends nothing more, because it would send
`wpctl set-profile` again before pw-dump reports the switch, and a held key never answers a
Bluetooth code question, which a key pressed before the question arrived would otherwise reject.

Date: 2026-10-11 · Source: tests `a_held_space_or_enter_acts_once`, `backspace_repeats_in_the_password_field_only` and `a_held_key_never_answers_a_code_question`; `Quick::key`

Status: the base rule awaits confirmation at the trial; the three exceptions came later

### The quick panel's outputs detail stays open after a choice

The outputs detail opens on the default sink and stays open after Enter: the `●` moves when pw-dump
reports the switch. Why it does not close is not recorded.

Date: 2026-10-11 · Source: tests `the_outputs_detail_lists_every_sink_and_sets_the_default` and `the_detail_keeps_its_sink_by_id`; `Quick::choose_sink`

Status: awaiting confirmation at the trial

### A session button closes the quick panel

A session button (lock, log out, and reboot or power off after the second activation) closes the panel
once its command is spawned. Why is not recorded.

Date: 2026-10-11 · Source: test `reboot_and_power_off_ask_first_and_the_question_expires` (`quick/tests.rs`); `Quick::activate`

Status: awaiting confirmation at the trial

### Do not disturb shows in the quick panel only with the notifications module

The do-not-disturb row exists only when the `notifications` module runs: without it there is no bell,
no server and nothing to mute.

Date: 2026-10-11 · Source: test `a_row_whose_tool_hardware_or_command_is_missing_is_not_shown`; `quick::live`

Status: awaiting confirmation at the trial

### A verb-opened quick panel takes the keyboard without a grab

`alpheus-launch --quick` opens the panel as a popup without a grab, and the bar's layer surface goes
`Exclusive` while the panel is open, on every compositor. A popup grab needs the serial of a press,
and a key bound in the compositor gives the bar none. Esc, the verb again, and a click on the bar or
on a module close the panel; a click on a window does not, because nothing tells the bar about it. On
Hyprland older than 0.56.2 a click inside a verb-opened panel may reach the bar and close it
(hyprwm/Hyprland#15729). A panel opened by a click keeps its grab, like every other popup. A verb
that arrives within 250 ms of the verb's own open is ignored (`quick: toggle debounced`), because
some compositors fire a key bind twice (Hyprland re-fires a press bind; the launcher's
`TOGGLE_DEBOUNCE`).

Date: 2026-10-11 · Source: headless `quick_the_verb_toggles_the_panel_and_its_keys_work_without_a_grab` and `quick_a_verb_right_after_the_verb_opened_it_is_debounced`; `quick_toggle`, `verb_debounced`, `TOGGLE_DEBOUNCE`

### The wheel moves the quick panel's window only when the lines do not fit

Over a slider, `n` wheel notches (at most 10 counted per event) send one command of `n × step` percent
(at most 100). Anywhere else
the wheel moves the panel's window over its lines only when they do not all fit under the bar;
otherwise it does nothing.

Date: 2026-10-11 · Source: tests `the_wheel_takes_ten_notches_at_most` and `the_wheel_elsewhere_moves_the_window_only_when_the_lines_do_not_fit`; `Quick::scroll`

Status: awaiting confirmation at the trial

### The quick panel reads the sink list through a shared tap

The sink list travels in a `SoundTap`, a `Mutex` around the pw-dump model shared by the sound thread
and the event loop. pw-dump is silent at rest, so a panel that is opening could not wait for the next
publication. The mutex is taken once per pw-dump batch and once per open. A sink list that a message
carries is moved into the open panel, and the bar never keeps one.

Date: 2026-10-11 · Source: guardrail `the_quick_panel_lets_go_of_the_sound_and_its_timer_whatever_closes_it`; headless `quick_closed_the_sound_thread_wakes_nothing_for_the_sink_list`; `SoundTap`, `taken_sinks`

Status: awaiting confirmation at the trial

### A Wi-Fi attempt on a new network outlives the quick panel

When the panel closes during an attempt on a network with no saved profile, the attempt is handed to
a follower (`WifiFollow`) that keeps the Wi-Fi read, and the panel's timer stays armed for it alone,
up to 20 s (60 s while the device is still activating). What a failed attempt then deletes is the
rule of "A failed Wi-Fi attempt deletes its new profile only when sure". A panel opened meanwhile
takes the attempt back. An attempt used to be abandoned when the panel closed.

Date: 2026-10-11 · Source: tests `closing_the_panel_keeps_the_detail_read_while_an_attempt_is_followed`, `a_panel_reopened_on_the_wifi_takes_the_attempt_back_and_opens_the_detail` and `a_follower_ends_with_its_attempt_and_deletes_only_a_failed_new_profile`; headless `quick_wifi_closed_during_a_failing_new_network_attempt_still_deletes_its_profile`; `WifiFollow`, `quick_flush`

### The Wi-Fi password lives in one boxed buffer that is wiped

The password is typed into a fixed buffer of 256 bytes on the heap, so moving the panel never copies
it. The buffer is wiped with volatile writes when the field closes, after the password is handed to
`nmcli`, and on drop; it is not locked in memory (the launcher's `--password` is not either). The
password is written to `nmcli`'s standard input straight from the buffer, in parts, never joined into a
copy and never an argument. A character that would not fit is refused whole, and the field then refuses
to connect rather than send a password cut short. What is not wiped: the per-key heap string that SCTK
(the Wayland toolkit) creates for each typed key, and the `Key::Char` on the stack that carries
each character to the panel.

Date: 2026-10-11 · Source: tests `characters_go_in_whole_and_come_out_whole`, `clear_zeroes_the_whole_buffer` and `a_character_that_does_not_fit_is_refused_whole` (`quick/secret.rs`); `the_password_is_handed_on_stdin_and_the_field_closes`; `Secret`

Status: awaiting confirmation at the trial: that `nmcli --ask` reads the password from a pipe is unverified until then

### An SSID that nmcli would misread is never passed to it

`nmcli device wifi connect <ssid>` takes the SSID as an argument, so the panel refuses one it cannot
pass as it is: empty, holding a NUL, starting with `-` (read as an option), not UTF-8 (an argument is
a string), `h`, `he`, `hel` or `help`, or six groups of hexadecimal digits (`refused`: nmcli reads
those as the word help and as a BSSID). Enter on such a network moves the selection to
`More…`, as it does on any network the panel does not handle (it handles only open and WPA-personal
networks). A saved or connected network is reached by its uuid, whatever its SSID.

Date: 2026-10-11 · Source: tests `an_ssid_nmcli_cannot_take_goes_to_more_unless_saved` and `bssid_and_help_lookalikes_are_refused`; `refused`, `Wifi::kind`

Status: awaiting confirmation at the trial

### A failed Wi-Fi attempt deletes its new profile only when sure

When an attempt that can create a profile (`nmcli device wifi connect`, never a `connection up`)
fails, the profile that appeared for its SSID during the attempt is deleted, but only if the saved
list was read whole when the attempt began (so an older profile is never taken for the new one) and
the profile is still saved at the failure. Nothing is deleted when the attempt is still activating
at the 60 s bound (a slow network keeps its profile), when the device was last seen
activated on what may be the new profile, or when the state the failure rests on was kept through
another domain's outage (NetworkManager may have connected since). The cleanup
also deletes nothing after a NetworkManager restart, for a profile reserved to another user, or
after an outage of the network domain during a followed attempt.

Date: 2026-10-13 · Source: tests `a_failed_new_network_shows_failed_and_its_new_profile_is_deleted`, `a_slow_activation_keeps_its_profile`, `an_attempt_begun_without_the_whole_saved_list_deletes_nothing`, `an_attempt_expiring_on_a_state_from_before_an_outage_keeps_its_profile` and `a_connection_up_attempt_never_deletes`; `Attempt::failure`, `Attempt::expired`

Status: awaiting confirmation at the trial

### A row under connection reads `…`

A Wi-Fi network being connected, and a VPN profile being brought up, show `…` after their name, not
`connecting…`. It is short, so the tag column can be `failed`'s six cells wide and an SSID keeps 16
cells.

Date: 2026-10-12 · Source: tests `a_connecting_network_is_tagged_until_it_is_connected` and `the_value_is_the_active_name_then_an_ellipsis_while_under_way_else_off`; `RUNNING_TAG`, `Wifi::tag`, `Vpn::tag`

### alpheus never handles a VPN secret

The VPN detail brings a profile up with `nmcli connection up uuid <uuid>`, never `nmcli --ask`, and
has no password field. `--ask` reads its secrets from standard input, which alpheus never feeds, so
the stored secrets in NetworkManager and any secret agent already registered (a desktop's) answer as
they usually do. A profile whose settings mark a secret as agent-owned or not saved (and not "not
required") does not fail because of it; when its attempt fails, the reason is the hedged
`the connection may need a secret` instead of `the activation failed`, and `More…` is the way.
`GetSecrets` is never called.

Date: 2026-11-15 · Source: test `the_attempt_connects_or_fails` (the hedged reason `NEEDS_SECRET`, `quick/vpn.rs`); headless `quick_vpn_the_verb_opens_the_detail_and_enter_brings_a_profile_up_then_down`

Status: awaiting confirmation at the trial: does `nmcli connection up` find the VPNs' stored secrets without `--ask`?

### Space on the VPN row takes the active profile down, else brings the last used one up

Space takes the first activating or activated profile (in the detail's order) down. With none, it
brings up the last used one: the highest `last_used` (NetworkManager's `connection.timestamp`) over
every profile read, even one past the 16 the detail lists; a tie or no timestamp: the first in the
detail's order. Nothing happens while an attempt runs.

Date: 2026-11-15 · Source: tests `space_takes_the_active_profile_down_else_brings_the_last_used_one_up`, `space_ups_the_last_used_even_past_the_listed_16` and `space_on_the_vpn_row_toggles`; `Vpn::toggle`

Status: awaiting confirmation at the trial

### A VPN attempt lives in the panel only, and taking a profile down is no attempt

`nmcli connection up` creates no profile, so a VPN attempt needs no cleanup after the panel closes:
closing drops it and `nmcli` carries on (the Wi-Fi attempt on a new network, which may leave a
profile behind, is followed after the close instead; see "A Wi-Fi attempt on a new network outlives
the quick panel"). A `connection down` is not followed: an active connection that NetworkManager
reports as activating or deactivating reads `…` from the state itself.

Date: 2026-11-15 · Source: tests `a_new_panel_starts_with_no_vpn_attempt` and `the_value_is_the_active_name_then_an_ellipsis_while_under_way_else_off`; `Vpn::act`, `Vpn::value`

Status: awaiting confirmation at the trial

### Several active connections for one VPN profile count by the most advanced state

After a down then an up, NetworkManager may still list the old active connection, deactivated, beside
the new one. A profile's state is the most advanced of its entries (activated, then activating, then
deactivating, then deactivated), so a stale deactivated entry no longer fails a fresh attempt once the
new entry is listed. A known limit: a snapshot holding only the stale
deactivated entry still fails the attempt at once; the parked fix is to treat that state as waiting
until `activating` was seen.

Date: 2026-11-16 · Source: tests `a_stale_deactivated_entry_never_fails_a_fresh_attempt` and `an_unknown_connection_state_ranks_behind_every_known_one`; `active_state`

### A toggle's state that cannot be read shows `·`, never off

A configured toggle's state is read when the panel opens and 300 ms after its toggle ran, never
watched. A state that is empty, of a form `bar.toml` refuses, a path that cannot be expanded, or a
file whose existence cannot be told (any error but "not found") is unknown, and its row shows `·`:
the panel does not say "off" on a guess. The same rule holds for Shield: see "An unknown Shield
state is never read as off".

Date: 2026-10-11 · Source: tests `an_unreadable_file_state_is_unknown_never_off` and `a_file_state_is_on_while_it_exists_and_expands_the_runtime_dir_and_home` (`quick/state.rs`); `state::read`

Status: awaiting confirmation at the trial

### The do-not-disturb detail marks and opens by the current state

The ● sits on `Off` when do-not-disturb is off, on `Until I turn it off` when it is on and untimed
(sharing included), and on `Until HH:MM` when a timed end's local time equals `[quick] dnd_until`;
otherwise on nothing. The detail opens on the ● entry, else on `30 minutes`, also when the ● entry is
`Off` (nothing to turn off). Why `30 minutes` is the fallback is not recorded.

Date: 2026-11-15 · Source: tests `the_mark_sits_on_the_entry_the_state_matches` and `the_six_choices_and_their_labels` (`quick/dnd/tests.rs`); `marked`, `DndDetail::open`

Status: awaiting confirmation at the trial

### Space on a blocked Bluetooth radio unblocks it, then powers it on

When rfkill holds the adapter off (`blocked` on the row), Space first runs `rfkill unblock bluetooth`
through the executor, and the adapter is powered on once BlueZ reports it unblocked, as soon as no
other action runs. The reason is that GNOME does the same.

Date: 2026-10-22 · Source: test `space_powers_the_adapter_or_unblocks_a_blocked_radio_first`; `Bt::toggle`, `power_after_unblock`, `Bt::set_view`

### Nearby Bluetooth devices are inserted once, by signal strength

A device found while the detail is open is inserted by its signal strength at that moment and never
moves; later changes of signal are not followed. A device with no signal reading during this open, or
with no name of its own, is not listed. The reason is that a scan then does not wake the loop
several times a second, and nothing moves under the selection. The cost is that a device
advertising without a signal reading is never listed.

Date: 2026-10-20 · Source: test `the_detail_lists_the_paired_devices_then_the_nearby_ones`; `BtReader`, `Bt::devices`

### A paired Bluetooth device is trusted, and a connect that is already done counts as done

After a pairing succeeds, the device's `Trusted` property is set, then a connect follows unless the
device is already connected. The reason for trusting is that the device can then reconnect by
itself next time (the usual behaviour of GNOME and blueman). A Bluetooth LE device is often connected
by the pairing itself, so BlueZ's `AlreadyConnected` answer to a connect counts as a success. A failed
`Trusted` step shows `failed` and stops, with no connect; the device stays paired but untrusted, and
the next Enter connects it.

Date: 2026-10-20 · Source: tests `a_pairing_is_a_pair_then_a_trust_then_a_connect` and `a_pairing_that_already_connected_the_device_skips_the_connect`; `Bt::answered`, `ALREADY_CONNECTED`

### Forgetting a Bluetooth device asks first, as the power menu does

Delete, or a right click, on a paired device turns its tag into `Forget? ⏎`; Enter then removes the
device. The timing and the cancel rules are those of "Reboot and power off ask first, and a second
activation confirms from 0.4 s to 3 s" (`session::CONFIRM_AFTER`, `session::CONFIRM`), through the
panel's one confirmation state, which holds either a session question or a forget question. A nearby
device cannot be forgotten. The asked form was chosen from mockups when the feature was designed; why it was
preferred to other forms is not recorded.

Date: 2026-10-20 · Source: tests `the_forget_question_waits_expires_and_is_cancelled_by_other_input`, `a_forget_question_about_another_device_is_not_confirmed_by_enter` and `a_press_on_the_asking_device_confirms_forget`; `Bt::ask_forget`, `Bt::activate`, `session::ask`

### Bluetooth deadlines live in the panel, and a late answer is forgotten

Each call of an action has a 30 s deadline (`CALL_DEADLINE`) kept in the panel's `Action`, not on the
acting connection. The reason is the panel's single timer, and the suspension of the
deadline while a pairing code shows. The cost is that an answer arriving after the deadline has to be
dropped explicitly, which the panel does by sending the connection a `BtOrder::Forget` for that call.

Date: 2026-10-22 · Source: tests `each_call_has_a_30_second_deadline_suspended_while_a_code_shows`, `the_next_deadline_is_the_earliest_of_the_action_and_the_failed_tag` and `an_answer_to_another_call_leaves_the_running_action_alone`; `Action::deadline`, `Bt::due`

### A pairing is answered by alpheus's own agent, for that device only

alpheus registers its own `org.bluez.Agent1` with the `KeyboardDisplay` capability (`RegisterAgent`,
never `RequestDefaultAgent`). The reason is that BlueZ routes a pairing to the agent of the
client that asked for it, so alpheus answers only the pairings it starts and never takes over another
tool's. The agent answers BlueZ's unique name only, and for the device being paired only. Incoming
pairings and codes to type into alpheus (very old devices) are out of scope, and alpheus
authorizes no incoming service. Every agent call goes to the panel for its reply, and only the
acting connection refuses strangers and unknown members, so each reply is a pure decision that a unit
test checks; the cost is that a `Display*` reply waits one loop turn.

Date: 2026-10-20 · Source: tests `each_agent_request_gets_its_state_and_its_reply`, `an_agent_request_for_another_device_is_rejected` and `an_agent_call_with_no_panel_is_rejected`; `Bt::agent`, `CAPABILITY`

Status: awaiting confirmation at the trial: BlueZ's D-Bus policy on NixOS and a real pairing are unverified

### A pairing code has no timeout, and leaving the detail cancels the pairing

A code to compare (`Code 482 913? ⏎`) is not a forget-style question: it has no 0.4 s wait and no 3 s
expiry, since comparing a code takes longer, and it stays until it is answered, BlueZ cancels, or the
panel closes. It is the answer to the running pairing, not a new action. Closing the panel or leaving
the detail during a pairing cancels it (`Device1.CancelPairing`), because the code could no longer be
shown; unlike a Wi-Fi attempt, a pairing does not outlive the panel.

Date: 2026-10-20 · Source: tests `arming_leaves_the_deadline_off_while_a_pairing_code_shows` and `leaving_rejects_then_cancels_pairing_then_stops_discovery`; `Bt::key`, `Bt::leaving`

### The Bluetooth acting connection opens with one blocking connect and asks for BlueZ's owner itself

The acting connection's connect and `Hello` block the event loop once each time the connection is
opened (when the detail opens or a call goes out). Reads never block; writes flush libdbus's queue,
blocking as the tray's session connection does (`BtAct::flush`, called from `bt_run`, and
`BtAct::close`). This is not non-blocking on the event loop, a cost accepted here as it is for the tray. The connection also sends its own `GetNameOwner` for
`org.bluez` at each open, and holds the calls made meanwhile until the answer; the reason is
that the read thread's idea of the owner is not shared with the loop, and the cost is one extra round
trip per open. It asks again after a `NoReply` (`BtAct::recheck`): the same owner
keeps the failure; no owner or a new one answers `Gone` and forgets the owner, so nothing later goes
to an old unique name. When BlueZ goes or its owner changes, the connection is let go without a word.

Date: 2026-10-22 · Source: tests `a_sync_drops_closes_opens_or_keeps_the_connection`, `a_refused_connection_is_not_reopened_by_the_syncs` and `bluez_restarting_closes_the_detail_and_lets_the_connection_go`; `BtAct::system`, `BtAct::flush`, `BtAct::recheck`, `App::bt_open`, `App::bt_let_go`

### UPower is the one service the daemon may start

The D-Bus source reads the system services through their unique names so that no read ever starts
one. The exception is UPower: on some systems (NixOS) it is activated by D-Bus only when a client
asks, so nothing has started it when the daemon comes up and the battery would stay missing. When
the battery is wanted and UPower has no owner at a session's start, the session sends one
`StartServiceByName("org.freedesktop.UPower", 0)` to the bus driver after its first publish, never
retried (a reconnection asks once more); the name appearing brings the battery in through
`NameOwnerChanged`. `dbusrs_bus::message` refuses any other name, and a guardrail lists the method.

Date: 2026-11-21 · Source: tests `start_service_by_name_names_upower_alone`, `upower_absent_and_the_battery_wanted_is_asked_for_once_and_its_appearance_is_followed` and `upower_already_running_is_not_activated`; `upower::activate_if_absent`

### Dvorak navigation keys are off by default

The control center moves with the arrow keys alone until a user sets `[keys] dvorak_nav = true` in
config.toml: a shareable product does not assume one keyboard layout.

Date: 2026-11-23 · Source: test `dvorak_is_off_by_default_and_can_be_enabled` (`app/keymap.rs`); `KeysConfig::default`, `Keymap::default`

### alpheus never handles sleep or the screens' power

alpheus has no code for sleep, suspend, `dpms` or turning a screen off. On a DisplayLink output
(evdi) a `dpms off` can freeze the machine, and the shells that arm it by default do exactly that;
idle handling stays with the idle daemon (hypridle, swayidle) that the user already runs. The one
write to the compositor is the screens' layout through the output protocol, which carries no power
state.

Date: 2026-10-02 · Source: docs/SCOPE.md, "Non-negotiable guardrails" (amended 2026-10-30 when the displays were added); `outputs/wlr.rs`

### The daily core is polished, and no more system pieces are replaced

alpheus works on what is used every day (the bar, the launcher, notifications, the control center,
centralised settings, finishing touches) and stops taking over system pieces: the lock screen, the
portal's file chooser and settings backend, the screensaver. The lock screen was written and
reviewed but left unmerged; the file chooser was dropped during its design. A new feature must
serve the core, not extend alpheus into another piece of the system.

Date: 2026-10-11 · Source: docs/SCOPE.md; the scope rule of CONTRIBUTING.md

### The control center's session actions use bar.toml's [quick] commands

The Power tab's Lock, Log out, Reboot and Power off run `[quick]`'s `lock`, `logout`, `reboot` and
`poweroff`, the commands of the bar's ⏻ menu, and an empty command hides its button. They used to
be `hyprlock` and `wm-set exit`, the author's own session, which broke every other setup. The
control center must not depend on the daemon crate, so the four commands, their neutral defaults
and a lenient reader of `bar.toml`'s `[quick]` keys live in alpheus-common (`SessionCommands`);
the daemon takes its defaults from there and keeps the validation of the whole file: the control
center reads only the four keys, so the two can differ when `bar.toml` has an error elsewhere. The
commands start detached, as the bar starts them, so the TUI's 2 s tool limit never kills a user's
command. Suspend stays
`systemctl suspend`: the bar has no such entry and nothing in the project handles sleep policy.

Date: 2026-11-24 · Source: maintainer's approval before the public release; `SessionCommands`, `App::session`

### The theme has 16 colour roles and 7 hues, and the frame colour is derived

Every surface paints with 16 named roles (backgrounds, text, accents, states) and 7 named hues
(`red`, `orange`, `yellow`, `green`, `cyan`, `blue`, `purple`) that give each module its icon colour.
The frame and border colour is not a 17th role: it is `fg_dim` at about 40 % alpha over the surface it
sits on. A colour file with only the old seven keys stays valid, each missing role derived from them,
and a missing hue is a fixed reference hue mixed 30 % toward the accent, so the theme stays in tune.

Date: 2026-10-07 · Source: tests `role_names_cover_every_field_once_in_order`, `hue_names_cover_every_field_once_in_order`, `a_legacy_file_is_derived_for_dark`, `a_legacy_file_is_derived_for_light` and `missing_hues_are_the_catppuccin_reference_mixed_30_percent_toward_the_accent` (`alpheus-common/src/theme/mod.rs`); `hues_come_from_the_official_palettes` (`theme/presets.rs`)

### A bad theme input falls back one line at a time, and a reload keeps the colours

Every theme failure is one log line and a fallback, never a stop: an unknown theme name gives the default
theme, an invalid `[colors]` value is ignored for that role, an unknown `style` gives `flat`, an
`opacity` out of range is clamped, and each warning is logged once per change. A hot reload that meets an
unreadable or invalid colour file (matugen caught mid-write) or an empty `theme.toml` (an editor
truncating it mid-save) keeps what is in effect instead of falling back: the colours stay, and the
settings stay while the colour files and the portal's light or dark switch are still read. At start an
empty file means the defaults.

Date: 2026-10-07 · Source: tests `an_unknown_theme_warns_and_keeps_the_default`, `an_unknown_style_warns_once_and_falls_back_to_flat` and `an_opacity_out_of_range_is_clamped_with_one_warning` (`theme/settings/tests.rs`); `an_empty_theme_toml_during_a_reload_keeps_the_settings`, `an_empty_theme_toml_still_reads_the_colour_files` and `a_reload_logs_only_new_warnings` (`watch.rs`); headless `a_truncated_theme_toml_keeps_the_style_and_still_reads_a_new_wallpaper`

### The bar has six styles, and `flat` is the bar as it always was

`style` in `theme.toml` is one of `flat` (the default), `islands`, `powerline`, `floating`, `underline`
and `translucent`; `opacity` (0.3 to 1.0) applies to `translucent` only. `flat` writes the same bytes the
bar wrote before styles existed, so a user who sets nothing sees no change, and an unused style costs
only a branch. The layout makes one block per group of modules (workspaces, title, status, resources,
tray, clock, launcher); `flat` and `underline` keep their separators and the block styles drop them.
Under `module_colors = "pills"`, `flat` and `underline` behave like `full`.

Date: 2026-10-08 · Source: tests `paint_bar_under_flat_is_paint_to_the_byte` (`painter/tests.rs`), `every_style_is_read_and_flat_is_the_default` (`theme/settings/tests.rs`), `block_styles_drop_the_separators_and_pad_each_block`, `flat_records_no_group` and `flat_and_underline_separate_with_a_dim_dot` (`layout/tests.rs`) and `pills_paint_nothing_extra_under_flat_and_underline` (`painter/shape_tests.rs`)

### A restyle that moves the bar waits for the compositor's configure

Switching to or from `floating` changes each bar's margin, size and exclusive zone, so the bar is not
painted until the compositor's configure for the new placement arrives (its old buffer would show under
the new placement). A bar never freezes waiting for an exact echo: only the first configure after a
placement, and only one that answers the previous placement, keeps the hold; the second ends it
whatever its height, so a compositor that refuses the resize cannot hold the bar.

Date: 2026-10-08 · Source: tests `a_bar_waiting_for_its_new_placement_is_not_drawn`, `only_the_answer_to_the_previous_placement_keeps_holding` and `the_second_configure_after_a_placement_always_ends_the_hold` (`wayland/tests.rs`); `only_a_switch_to_or_from_floating_reconfigures_the_surfaces` (`watch.rs`); headless `floating_moves_the_bar_down_by_its_margin_and_back`

### The Mango and Sway backends read bounded data

A Mango snapshot is read for at most 16 outputs, 32 tags per output and 512 clients, a Sway tree for at
most 64 workspaces, 512 windows and 64 levels, and labels are cleaned and cut like every backend's.
A Mango line over 1 MiB or a Sway reply over 4 MiB ends the session, which is the usual outage (the bar
shows `–`, then the backoff). No new thread, no wake-up at rest, no child process: Mango is read on two
sockets in one blocking `poll`, and `mmsg` is never spawned.

Date: 2026-10-13 · Source: tests `bounds_and_labels` (`compositor/mango.rs`, `compositor/sway.rs`), `the_walk_stops_at_max_depth` and `a_deep_reply_is_bounded_on_the_compositor_stack`; guardrail `the_mango_and_sway_backends_have_no_panicking_path`; memory `at_rest_no_wakeups_under_sway_and_mango` (`tests/memory.rs`)

### Mango's socket path is read again at each session

The socket a Mango session opens is `$MANGO_INSTANCE_SIGNATURE`, an absolute path that embeds the
compositor's pid, so it is read again each time a session starts, after the first detection and after
every outage and backoff; the detected path is used only when the variable is gone or unusable.

Date: 2026-10-13 · Source: tests `the_mango_path_is_read_again_at_each_session` and `mango_socket_is_the_variable_itself_absolute_only` (`sources/tests.rs`)

### Nothing of the launcher stays resident while it is closed

The launcher is a layer-shell surface of the daemon, made when it opens and dropped when it closes, with
its click-outside scrims and its catalog; only the history file stays, and the apps are scanned on a
short-lived thread started at open. A launcher error closes the launcher and never the bar.

Date: 2026-10-05 · Source: guardrail `nothing_resident_while_the_launcher_is_closed`; memory `at_rest_no_wakeups_after_a_launcher_cycle` (`tests/memory.rs`); headless `thirty_launcher_closes_by_key_then_an_outside_click_keep_the_bar_alive`

### A launched program is never a child of the bar

An app starts in its own `systemd-run --user --scope` when a user manager runs, and fully detached
otherwise or when `systemd-run` is missing (decided once per open), so it never dies with the bar. The
arguments are an argv, never through a shell: the `Exec` field codes are expanded, and a program missing
from `$PATH` is reported in the box before anything is spawned. The command mode (`>`) is the only place
a shell is used, and only for the text the user typed. A window is focused through the configured
template, never in a scope.

Date: 2026-10-05 · Source: tests `an_app_starts_in_its_scope_or_detached`, `without_systemd_run_the_app_is_retried_detached`, `a_missing_program_is_reported_before_systemd_run` and `windows_are_focused_through_the_template_never_in_a_scope` (`launcher/run/tests.rs`); guardrail `every_launcher_activation_stays_on_the_allowlist`

### The launcher reads its command sources after the apps, on the same thread

The `list` commands of `[[sources]]` run at each open, one after another, on the scan thread that read the
apps and the commands, after the apps were delivered: the box has its apps first and the source entries
join the results as a second delivery. Nothing runs between opens or per keystroke, an unconfigured
source costs nothing, and a source that panics loses its own entries, never the apps. Source entries
show in the All mode on a typed query only; at equal score they come after windows, apps and tools and
before commands, and their frecency is kept per source and line.

Date: 2026-10-10 · Source: tests `a_panicking_source_run_loses_its_entries_never_the_apps` (`launcher/scan/tests.rs`), `entries_follow_the_catalog_rule_on_an_app_launcher_only` (`launcher/host.rs`), `entries_arriving_after_typing_join_the_results` (`launcher/model/tests.rs`), `the_kind_order_puts_tools_between_apps_and_source_entries` and `entry_frecency_is_kept_per_source_and_line` (`launcher/matching/tests.rs`); headless `the_empty_query_shows_no_source_entry`

### A command source is bounded and cannot stall the launcher

A source's `list` command runs with its standard input and error on `/dev/null`, gets 1 s in all (then
it is killed and reaped, and the lines read so far are kept) and is read up to 2,000 lines and 256 KiB
(past either it is killed). Each line is cleaned (invalid UTF-8, control characters and bidi overrides
dropped, lines over 512 bytes and empty lines skipped, duplicates kept once) and a failing source is
logged once per open with its name and never its lines. There are at most 8 sources, and a `run`
command holds `{line}` as exactly one whole argument, so a line can never be split or reinterpreted.

Date: 2026-10-10 · Source: tests `a_hang_is_killed_at_one_second_and_its_lines_so_far_are_kept`, `a_flood_stops_at_the_line_cap`, `a_flood_of_long_lines_stops_at_the_byte_cap`, `a_source_s_lines_are_cleaned_and_deduplicated_in_order` and `run_argv_puts_the_whole_line_in_one_argument` (`launcher/sources.rs`); `each_source_rule_refuses_with_its_reason` and `the_source_limits_themselves_are_accepted` (`launcher/config.rs`); headless `a_hung_source_is_killed_and_nothing_stays_awake`

### The tray owns the watcher when it can, and is a host of another's otherwise

The daemon asks for `org.kde.StatusNotifierWatcher` without replacing an existing owner. When it owns the
name it serves the watcher and is its own host. When another bar owns it, the daemon serves nothing and
follows that watcher's list and signals as a host only; if that watcher leaves, it takes the name, and
applications register again on their own. The watcher and host are written in-house, because the
protocol is small and the crates evaluated block on every call, always fetch icon pixmaps and have no
host-only fallback.

Date: 2026-10-05 · Source: tests `request_name_only_ever_asks_the_watcher_name_without_replacing` (`session_bus/bus/tests.rs`), `nothing_is_served_while_another_watcher_owns_the_name` and `the_watcher_answers_what_clients_check` (`session_bus/watcher.rs`)

### The tray asks one property at a time, never icon pixmaps, with a 2 s timeout

Every tray call is asynchronous, matched by serial when the answer arrives, and times out after 2 s
(`session_bus::bus::TIMEOUT`), so a silent application never blocks the loop. Each property is one `Get`,
never `GetAll` (which would carry the icon pixmaps, hundreds of KB for Steam or Discord), every property
is optional and allowed to fail, and a failed one is not asked again until a signal names it. The icons
are glyphs, so the pixmaps are never requested.

Date: 2026-10-05 · Source: tests `each_property_is_one_get_never_getall` and `failed_properties_are_not_asked_again_until_a_signal_names_them` (`session_bus/host/tests.rs`), `tooltip_title_without_decoding_pixmaps` (`session_bus/codec/tests.rs`)

### The clipboard history is off by default, kept in memory, and wiped when removed

`[clipboard] enabled = true` in `bar.toml` turns it on, read at start: a resident reader of everything
the user copies is a privacy choice, so it is asked for and never inherited. Off, nothing of it is
bound, allocated or armed. On, the last 50 plain-text copies of at most 4 KiB are kept in memory only,
never on disk, and text removed (Delete, Clear, eviction, a dropped read, a cancelled source) is
overwritten before it is freed; the history is wiped at shutdown, pins included. A copy that carries a
password-manager hint is never read, and neither is one made while a launcher password prompt is open
or in the 5 s after it closes.

Date: 2026-11-16 · Source: tests `clipboard_is_off_by_default` (`config/tests.rs`), `a_row_wipes_its_first_line_too` and `debug_never_prints_text` (`clipboard/history/tests.rs`), `sensitive_hint_wins_over_text` and `quiet_window_rules` (`clipboard/offer/tests.rs`); guardrails `the_clipboard_shutdown_wipes_pins_too` and `the_clipboard_sensitive_hint_type_is_never_received`; headless `clipboard_verbs_when_off_exit_2` and `clipboard_skips_copies_around_a_password_prompt`

### One file names the clipboard protocol, and only it sets the selection

`clipboard/control.rs` is the only file that names a data-control protocol (`ext-data-control-v1`, else the
wlr one at version 1) and the only one where `set_selection` appears: the history puts an entry back
by owning the selection from the daemon, with a source of its own that carries a marker type, so that
its own copy is not read back. The primary selection is never read or set, and nothing ever clears the
user's clipboard. This is not a write to the compositor's configuration, which `docs/SCOPE.md` forbids;
it is what every application's copy does.

Date: 2026-11-16 · Source: guardrails `the_clipboard_selection_is_set_only_by_the_own_source` and `the_clipboard_offer_is_wrapped_in_destroy_on_drop`; tests `own_marker_is_skipped` (`clipboard/offer/tests.rs`) and `own_offer_is_never_read` (`clipboard/app.rs`)

### The notepad saves on every close and 2 s after the first unsaved edit, and never writes a file it could not read

The pad is written on every close when it was edited, and while it is open 2 s after the first unsaved
edit (one deadline armed only while the pad is open and edited; later keys do not push it back, so at
most 2 s of typing is ever unsaved). A failed save is one log line and `not saved` on the title row, and
the next edit or the close tries again; a close whose save fails keeps the text in memory for the next
open. A file that cannot be read safely (too large, not UTF-8, not a regular file of the user's, in a
directory that is not theirs) opens the pad read only and is never written, so a hand-edited file is
never truncated. The write is private and atomic, and an empty pad removes the file.

Date: 2026-11-15 · Source: tests `the_save_deadline_exists_only_while_open_and_dirty` and `save_is_private_and_atomic` (`notes/store/tests.rs`), `a_refused_file_is_never_written` and `load_refuses_too_large_and_not_utf8_showing_their_start`, `a_failed_save_stays_edited_so_the_close_saves_again` (`notes/app.rs`); headless `notes_a_failed_close_save_is_kept_for_the_next_open` and `notes_autosaves_while_open`

### The control center reads only the visible tab, and a failed domain waits

Only the tab on screen is refreshed (Home every second; Sound, Network and Bluetooth every 2 s; Power
never), a domain that failed is retried at most every 15 s (`FAILED_RETRY`), nothing runs in the
background and nothing when the window is closed.

Date: 2026-10-03 · Source: tests `tick_refreshes_home_every_second`, `tick_refreshes_sound_network_bluetooth_every_two_seconds`, `tick_reads_nothing_on_power`, `tick_does_not_reread_a_failed_domain` and `a_failed_domain_never_read_is_read_at_the_next_tick` (`app/tests.rs`)

### The control center draws its first frame before any read returns

The first render never waits for a system tool: it is drawn at once and filled as the worker thread
answers, so the window opens in under 100 ms even when every tool takes 500 ms to answer.

Date: 2026-10-04 · Source: tests `first_render_under_100_ms_even_with_slow_tools`, `first_render_of_the_dashboard_under_100_ms_even_with_slow_tools` and `first_render_of_diagnostics_under_100_ms_even_with_slow_tools` (`crates/alpheus/tests/first_render.rs`)

### The control center runs a fixed list of commands, and systemctl only to read, or to restart or clear a user unit

Every command the control center launches is on an allowlist that a guardrail test checks against a fake
executor. `systemctl` is limited to `suspend`, `reboot` and `poweroff`, reading the failed units and the
services, `show-environment` and `--version`, and, for the user manager only, restarting a unit without
blocking or resetting its failed state: it never starts or stops a unit or changes the environment.
Actions on a system unit never reach the executor: they run in the control center's own window.

Date: 2026-10-04 · Source: tests `every_launched_command_is_on_the_allowlist`, `systemctl_never_starts_stops_or_changes_the_environment` and `the_systemctl_line_rule_itself_bites` (`crates/alpheus/tests/guardrails.rs`)

### The control center draws its rail from 84 by 26 cells, and its dashboard from 64 by 20

At 84 columns by 26 rows or more the control center draws the left rail with one hue per tab, and the
Home tab as a dashboard when its body is at least 64 by 20; below those sizes it draws the classic tab
bar and rows. The sizes were set after measuring a real 87 by 26 window, and the existing 80 by 24 tests
cover the classic layout.

Date: 2026-11-15 · Source: tests `the_rail_needs_84_columns_and_26_rows`, `an_87_by_26_window_shows_the_rail` and `resizing_across_the_threshold_keeps_hits_honest` (`ui/rail/tests.rs`), `the_dashboard_needs_a_64x20_body` and `an_87_by_26_window_shows_the_whole_dashboard` (`ui/home`)

### Screenshots are encoded by the `png` crate, on a short-lived thread, and nothing of a capture is kept

The only new dependency of the captures is `png` with its default features off (its backend is
`flate2` over `miniz_oxide`; no image-format zoo, no `image` crate): about 78 KB in the stripped
daemon, measured back to back against the commit before it. A capture lives in its own shm pool,
dropped the moment the compositor answers, so the bar's pool never grows by a screenshot and a cycle
leaves the anonymous memory where it was. One file, `captures/screencopy.rs`, speaks the screencopy
protocol.

Date: 2026-11-24 · Source: tests `encode_png_round_trips_a_small_image`, `a_wrong_size_is_refused` (`captures/encode.rs`), `a_capture_cycle_gives_its_memory_back` and `capture_output_returns_a_frame_of_the_output_size` (`tests/screencopy_headless.rs`), `screencopy_only_in_screencopy_rs` (`tests/guardrails.rs`)

### A capture never touches the clipboard unless it succeeded

A cancelled selection is silent and leaves the clipboard as it was; a capture that failed gives one
critical notice and leaves it too; the success notice follows a capture that happened. The clipboard is
only reached by the worker with PNG bytes in hand, and a copy command that cannot start is a failure,
not a success. A custom `copy` command must accept a PNG on its standard input: the default `wl-copy`
alone is told `--type image/png`. The external tools keep the rule: alpheus copies what an
annotation tool wrote back or the text a recognizer printed, only after the tool exited 0 with a
result; the default satty flags turn its own copy into a save (`--copy-command "cat >/dev/null"
--save-after-copy --early-exit`, checked against satty 0.20.1), so it never reaches the clipboard by
itself. The copy command reads the whole image and throws it away: satty saves only once its write
to the command succeeded, and a command that does not read (`true`) breaks that write for any image
past the pipe's 64 KiB, which would lose the annotation as a silent cancel.

Date: 2026-11-24, tools 2026-11-25 · Source: tests `the_default_annotate_copy_command_reads_a_whole_large_image` (`config/tests.rs`), `a_cancel_is_silent_and_never_copies`, `the_notices_say_what_happened_and_only_a_success_is_a_calm_one`, `a_tool_closed_without_output_is_silent_and_a_failed_one_is_critical` (`captures/tests.rs`), `a_tool_failure_is_critical_and_never_copies`, `ocr_copies_stdout_text_and_an_empty_output_is_a_cancel` (`captures/tools/tests.rs`), `captures_default_to_the_external_tools_and_no_folder` (`config/tests.rs`), `an_area_outside_the_frame_is_a_capture_failure_and_nothing_is_copied`, `a_copy_that_cannot_start_is_a_copy_failure_and_no_file_stays`, `a_custom_copy_command_is_run_as_written` (`captures/deliver/tests.rs`), `a_cancelled_capture_is_silent_and_copies_nothing` (`tests/headless_session.rs`)

### A capture waits for the selector's removal

The selector's surface is destroyed before its answer is used, but the compositor must deal with the
removal before a frame is asked, or the dim is in the capture. The flow therefore waits for a
`wl_display.sync` sent after the destruction (`Step::Settle`, then `Flow::settled`); the selector's
answer never asks for the frame itself. On the harness's sway the dim stays out even without the wait
(requests are handled in order), so the rule is pinned on the flow and by a guardrail, and the headless
test checks the pixels.

Date: 2026-11-24 · Source: tests `a_selection_waits_for_the_compositor_before_the_frame_is_asked`, `every_scope_waits_for_the_compositor_before_its_frame` (`captures/tests.rs`), `a_frame_is_asked_only_after_the_selectors_removal_is_settled` (`tests/guardrails.rs`), `an_area_capture_has_no_dim_tint` (`tests/headless_session.rs`)

### A screenshot reaches the copy command as a file on its standard input

`Executor::spawn_with_input` carries at most one page, which a PNG far exceeds. The worker writes the
PNG to a private file (`$XDG_RUNTIME_DIR/alpheus/capture.png`, owner only, atomic), starts the copy
command with that file as its standard input (`Executor::run_with_stdin_file`: no pipe to fill) and waits at most 5 s for the command's own exit,
the foreground of `wl-copy` ending once it has read the file; "copied" means that exit was 0, as
`screenshot.sh` checks `wl-copy`'s. A non-zero exit, a signal or the timeout (the command is killed,
never what it left behind) is a copy failure. The file is removed afterwards, also when the worker
panics (a panic ends the capture as a failure), and a start clears one a killed daemon left. The cut,
the encoding and this run on a short-lived `alpheus-capture` thread, one at a time.

Date: 2026-11-24 · Source: tests `run_with_stdin_file_feeds_a_big_file_and_reports_the_exit`, `run_with_stdin_file_returns_at_the_foreground_exit_and_kills_at_the_limit` (`alpheus-common/src/exec/tests.rs`), `the_png_reaches_the_default_copy_command_as_a_file_on_its_stdin`, `a_copy_command_that_exits_badly_or_hangs_is_a_copy_failure`, `a_panicking_executor_ends_the_capture_as_a_failure_and_leaves_no_file`, `stale_private_files_are_removed_at_start_and_nothing_else` (`captures/deliver/tests.rs`), `the_png_is_encoded_on_the_capture_thread_and_copied_through_a_file`, `the_capture_worker_always_mails_its_ending` (`tests/guardrails.rs`), `a_start_clears_the_capture_files_a_killed_daemon_left` (`tests/headless_session.rs`), `at_rest_no_wakeups_after_a_screen_capture` (`tests/memory.rs`)

### A frame that never comes fails the capture, and a late frame is never the next one's

One capture runs at a time, so a compositor that never answered a frame request would hold the slot
until the daemon restarted. A one-shot timer, armed only while a frame is awaited (5 s), fails the
capture, destroys the frame object and frees the slot. Every answer (the round trip, the frame, the
deadline) carries the number of the capture it is for, so the late answer of a capture that has ended
is dropped unread and never taken for the next one's.

Date: 2026-11-24 · Source: tests `a_frame_that_never_comes_fails_the_capture`, `a_late_frame_from_an_old_generation_is_ignored` (`captures/tests.rs`), `a_capture_that_ends_gives_up_its_frame_and_its_deadline`, `the_frame_deadline_is_armed_only_while_a_frame_is_awaited` and the `Timer::` entry of `nothing_resident_while_the_launcher_is_closed` (`tests/guardrails.rs`)

### The colour picker picks from a frozen frame and copies text through `copy`, as written

The picker captures the focused output once, through the same flow as a whole screen (the settle
round trip first, so a closing menu is not in it), and shows that frame on its own Overlay surface:
moving content stands still, and the lens reads the very pixels the click picks. The frame belongs
to the open picker and goes with it at close; nothing of it stays. The value is text: `{hex}` is
`#RRGGBB` in upper case, `[captures] pick_format` may fill `{r}`, `{g}`, `{b}` and `{hex}` instead.
It reaches `launcher.toml`'s `copy` the way a screenshot does (a private file, `pick.txt`, on its
standard input, from the `alpheus-capture` thread), so "copied" is the command's exit 0; the command
is run as written, with no image type, and a custom one must accept text on its standard input. Esc,
a right click, focus taken or another surface opened is a silent cancel; the compositor taking the
surface away, or no buffer to open it, is a critical failure; the clipboard is touched only by a pick.

Date: 2026-11-24 · Source: tests `a_pick_settles_grabs_the_screen_then_opens_the_picker`, `a_picker_that_fails_ends_the_pick_as_a_critical_failure` (`captures/tests.rs`), `esc_picks_nothing_and_copies_nothing`, `the_value_is_uppercase_hex_by_default`, `pick_format_fills_r_g_b_hex` (`captures/pick/tests.rs`), `a_picked_value_reaches_the_copy_command_as_text`, `a_picked_value_the_copy_command_refuses_is_a_copy_failure` (`captures/deliver/tests.rs`), `the_picker_frame_is_kept_only_while_the_picker_is_open`, `a_picker_that_cannot_open_fails_its_capture` (`tests/guardrails.rs`), `colour_picker_copies_the_pixel_under_the_pointer` (`tests/headless_session.rs`), `at_rest_no_wakeups_after_a_colour_pick` (`tests/memory.rs`)

### Only screencopy.rs names the capture protocol

`zwlr_screencopy_manager_v1` is named in one file, `captures/screencopy.rs`, which captures one
output into its own short-lived shm pool and answers ready or failed; everything else of the
captures sees a `Frame`. The external recorder speaks the protocol itself, in its own process: alpheus
never records through it. A later move to `ext-image-copy-capture` then touches one file.

Date: 2026-11-25 · Source: test `screencopy_only_in_screencopy_rs` (`tests/guardrails.rs`)

### An external capture tool runs as a watched child of the daemon

An annotation editor stays open as long as the user works in it, so it cannot run on the
`alpheus-capture` worker, whose waits are bounded in seconds. The daemon starts the tool as its own
child (`Executor::spawn_watched`: its own process group, standard input `/dev/null`, SIGINT if the
daemon dies) and puts the child's pidfd on the event loop: the descriptor turns readable only when the
process ends, so nothing polls, no thread waits and no timer is armed while it runs, and the watch is
removed with the end. The image is a private `capture-<n>.png` in `$XDG_RUNTIME_DIR/alpheus` (owner
only); the tool's output is a private file made by alpheus before the tool starts (`capture-<n>-out.png`
for the editor to write into, `capture-<n>.txt` as the recognizer's standard output), so it stays
owner-only whatever the tool's umask; `{file}` and `{out}` are whole arguments, never inside a longer
one (`[captures]` is refused otherwise). Exit 0 with an image or a text is the result, exit 0 with
nothing is a silent cancel, anything else (another code, a signal, a tool that cannot start, more
than 256 KiB of text) is a critical failure. Every file goes on every path, and a start clears what a
killed daemon left. One capture at a time includes the tool's run: a screenshot asked while the
editor is open is ignored, as during any capture. A kernel without pidfds (before Linux 5.3) cannot
run the tools: the child is killed at once and the capture fails.

Date: 2026-11-25 · Source: tests `a_watched_program_signals_its_end_on_its_descriptor_and_gives_its_code`, `a_watched_program_writes_its_output_to_the_file_given`, `a_watched_program_is_interrupted_when_its_thread_ends` (`alpheus-common/src/exec/tests.rs`), `annotate_passes_whole_file_and_out_and_copies_the_result`, `ocr_copies_stdout_text_and_an_empty_output_is_a_cancel`, `a_tool_failure_is_critical_and_never_copies`, `temp_files_are_0600_and_removed` (`captures/tools/tests.rs`), `an_annotation_runs_the_tool_then_copies_what_it_wrote` (`captures/tests.rs`), `the_tool_placeholders_are_whole_arguments_where_each_tool_needs_them` (`config/tests.rs`), the `Generic::new` entry of `nothing_resident_while_the_launcher_is_closed` and `a_recording_lights_the_privacy_glyph_while_it_runs_and_its_watch_goes_with_it` (`tests/guardrails.rs`), `annotate_copies_what_the_tool_wrote`, `ocr_copies_the_text_the_tool_printed` (`tests/headless_session.rs`)

### Recording is the external tool's, driven and stopped by alpheus

alpheus encodes no video: `[captures] record` (wf-recorder by default) records, as a watched child
of the daemon. alpheus chooses what and where: the whole focused output or a selected area, given as
`{geometry}` (`x,y wxh` in the compositor's logical pixels, as slurp prints it; the whole output is
its own geometry, since wf-recorder asks on its standard input which output to record when there are
several and it has neither `-g` nor `-o`), and a free `screenrecord-%Y%m%d-%H%M%S.mp4` in
`$XDG_VIDEOS_DIR` (else the videos entry of `user-dirs.dirs`, else `~/Videos`), never over a file.
`--capture stop` or the Stop row sends SIGINT to the recorder's own process group, and to nothing
else: no pattern, no other group, only the child alpheus is still the parent of. A recorder that left
its group (the group signal finds nobody) gets the signal by its pid, still only while it is our
unreaped child; what it started is then out of reach, so the recorder must stay in its group.
The recording ends when the recorder does: exit 0 with a non-empty file is "Recording saved" (the file's name in the
notice, never in a log), anything else is critical. One recording at a time; captures may run while it
records. The privacy module's screen glyph shows `alpheus` while it runs (a recorder that bypasses
PipeWire is seen by no other source on Sway or Mango). At the daemon's shutdown the recorder gets
SIGINT and at most 2 s to finish its file, and the signal guard leaves that time; a recorder still
running then is let go, never killed (a kill would leave its file unplayable), and its ending is
logged as not confirmed. An ending at shutdown is a log word only, never a notice: the notification
server is alpheus's own and stops with the daemon, so nobody would see it. Nothing is persisted: a recording is never adopted after a restart.

Date: 2026-11-25 · Source: tests `record_spawns_with_the_geometry_and_stop_sends_sigint_to_it_only`, `the_daemon_stop_ends_the_recording_with_sigint`, `a_recording_never_writes_over_a_file`, `a_recording_that_fails_or_leaves_nothing_is_a_failure` (`captures/record/tests.rs`), `a_released_watched_program_is_not_killed` (`alpheus-common/src/exec/tests.rs`), `recording_lights_the_privacy_screen_glyph` (`privacy/tests.rs`), `the_shutdown_grace_covers_the_recorders_stop_while_recording` (`startup/tests.rs`), `the_capture_rows_follow_their_tool_and_stop_shows_only_while_recording` (`launcher/tools/catalog/tests.rs`), `interrupt_reaches_the_watched_program_only`, `an_interrupt_is_never_sent_to_a_reaped_pid`, `a_program_outside_its_group_is_signalled_by_its_pid`, `a_watched_program_s_signals_fall_back_to_its_pid` (`alpheus-common/src/exec/tests.rs`), `the_recording_signal_reaches_only_the_recorder_alpheus_started`, `the_shutdown_ends_the_recording_before_anything_goes` (`tests/guardrails.rs`), `record_then_stop_notifies_saved`, `recording_lights_the_privacy_screen_glyph_on_the_bar`, `a_recording_running_at_the_daemon_stop_is_finished` (`tests/headless_session.rs`), `at_rest_no_wakeups_after_a_recording` (`tests/memory.rs`)

### Large blocks are always mapped on their own

glibc raises its mmap threshold to the size of the first large block a program frees, and from then
on serves blocks of that size from an arena's heap. In the daemon the first such block is a
capture's frame (3.6 MB at 1280 × 720): after it, the worker's image and PNG came from the second
arena (`M_ARENA_MAX` is 2), where a freed block at the heap's top is never returned by
`malloc_trim`, and one frame's worth of anonymous memory stayed resident from the third capture on
(1,080 → 4,740 KB over ten captures, three runs out of three). The daemon now sets
`M_MMAP_THRESHOLD` to glibc's own default, 128 KiB, at start, next to `M_ARENA_MAX`: setting it
turns the dynamic adjustment off, so every block of 128 KiB or more has its own mapping and is
unmapped when freed. The cost is one `mmap` and one `munmap` per such block: a few per capture,
and elsewhere only where a block that large is allocated. The memory suite is the check that no
other figure rose with it: `startup_pss_per_source` reads the same on both sides, 3 runs each. A
user's `GLIBC_TUNABLES=glibc.malloc.mmap_threshold=…` is overridden by this call.

Date: 2026-11-24 · Source: `limit_malloc_arenas` (`startup.rs`), test `at_rest_no_wakeups_after_a_screen_capture` (`tests/memory.rs`)

### Only the user's own processes are offered, and a reused pid is never signalled

The kill list (`alpheus-launch --kill`) lists the processes of the user alpheus runs as, and never
PID 1, alpheus, the session leader, the compositor (found by the kernel's peer credentials of its
socket, since a wrapped compositor runs under another name, and by its name), any ancestor of
alpheus or of the compositor (the script that started the session, the shell that started the bar),
or the user manager (`systemd`): ending one of those ends the session. The exclusions are worked
out once per open and checked again when a signal is sent. A pid alone can name another program
by the time Enter is pressed, so the list keys every row on its pid and its start time (field 22 of
`/proc/<pid>/stat`); before a signal, a pidfd is opened on the pid, the start time is read again,
and the signal goes through that pidfd, which names the process it was opened on for ever. A start
time that differs sends nothing; `kill(2)` by pid is the fallback only for a kernel without
`pidfd_open`. Enter sends SIGTERM; SIGKILL takes Ctrl+Enter and a second Enter 0.4 s to 3 s later,
as the power menu asks. The outcome is said in the box only: no log line or notification carries a
name, a pid or a command line.

Date: 2026-11-24 · Source: tests `only_the_users_processes_are_listed`, `pid_1_alpheus_and_the_session_leader_are_never_offered`, `the_compositor_and_the_user_manager_are_never_offered_by_name`, `a_reused_pid_is_never_signalled`, `a_pid_that_could_reach_a_group_or_init_is_refused`, `an_excluded_pid_is_refused_even_when_asked_for` (`launcher/processes/tests.rs`), `ctrl_enter_asks_then_a_second_enter_kills`, `a_reused_pid_says_so_and_sends_nothing`, `every_ancestor_of_the_compositor_and_of_alpheus_is_excluded`, `only_a_kernel_without_pidfd_falls_back_to_kill`, `a_signal_without_a_pinned_process_sends_nothing` (`launcher/processes_app/tests.rs`), `the_pid_is_the_peer_of_the_socket`, `a_wedged_compositor_never_blocks_the_caller` (`sources/peer/tests.rs`), `the_kill_list_has_no_panicking_path_and_logs_fixed_words` (`tests/guardrails.rs`), `kill_list_opens_and_signals_by_enter` (`tests/headless_session.rs`)

### A scheduled shutdown always warns a minute before, even when its time has passed

A power off the user did not see coming is the one outcome the shutdown must never have. So the
step after "scheduled" is always the warning (a critical notice with no timeout, `Click to cancel`,
set apart from the notification rules), and the power off comes only after it, at the later of the
end and the warning + 60 s: a laptop that wakes up past the end, or a bar restarted after it, warns
first. A warning the machine slept through (more wall time than monotonic time since it, by over a
second) or one the clock went back before no longer counts and is given again. At start a due
warning waits for alpheus's notification server to own its name, at most 10 s; with no server the
chip still shows the last minute and one log line says the warning was not shown. A shutdown saved
by an earlier boot is dropped at load (the state directory outlives a reboot), as is one with no
`[quick] poweroff` to run. Dismissing the notice does not cancel; its click does. The shutdown is a
slot of the timer, not a timer: a Pomodoro never cancels it and it never ends one.

A warning under a lock screen is not a warning: a notice sits on an overlay layer, below the
locker, so a user who takes over a minute to unlock would be powered off unwarned (the laptop that
slept past the end and woke under hyprlock). So a step due while the session is locked is held, the
warning given before the lock stops counting, and the first check that finds the session unlocked
gives the warning fresh, with its whole minute. The lock is told by a screen locker's process
(`hyprlock`, `swaylock`, `gtklock`, `waylock`, `i3lock`, `xsecurelock`, the user's own), looked up
when a step is due and, during the warning's minute only, every 5 s (a lock that starts and ends
inside the minute would otherwise be missed, and the user powered off with a few seconds of
warning left), once a minute while held, on the shutdown's existing deadline. Logind's `LockedHint`
was set aside because the hyprlock and hypridle 0.1.7 binaries, the only ones read, never mention
it, and its `Lock`/`Unlock` signals because hyprlock, left by its password, sends no `Unlock` (it
does not call `loginctl unlock-session`): a state kept from them would hold the shutdown for ever.
An unknown locker, a zombie or an unreadable `/proc` reads as unlocked, which is how the shutdown
behaved before; the list is in `timer/lockscreen.rs`, the folder read is `[timer] proc_dir`.

Date: 2026-11-24, amended 2026-11-25 · Source: tests `the_warning_comes_sixty_seconds_before_the_end`, `a_past_shutdown_warns_first_then_powers_off`, `a_warning_holds_only_if_the_wall_clock_did_not_outrun_the_monotonic_one`, `the_warning_is_a_sticky_critical_notice_with_a_cancel_action` (`timer/shutdown/tests.rs`), `a_timer_change_never_touches_the_shutdown` (`timer/tests.rs`), `the_shutdown_check_warns_then_powers_off_and_never_at_once`, `a_suspend_in_the_warning_minute_warns_again_and_never_powers_off_at_once`, `a_short_suspend_in_the_warning_minute_warns_again`, `a_shutdown_from_before_this_boot_is_dropped`, `no_poweroff_command_refuses_the_verb_and_drops_a_pending_shutdown`, `a_warning_waits_for_the_server_but_never_for_long` (`timer/app/tests.rs`), `a_locked_session_is_never_warned_or_powered_off_until_it_unlocks`, `the_lock_is_looked_at_only_when_a_step_is_due_or_a_warning_is_in_force`, `a_lock_inside_the_warning_minute_gives_a_fresh_minute_at_the_unlock` (`timer/app/tests.rs`), `the_lock_is_sampled_only_while_the_warning_minute_runs` (`timer/shutdown/tests.rs`), `the_known_lockers_are_told_by_name`, `a_running_locker_of_the_user_is_found`, `a_zombie_locker_holds_no_lock`, `another_users_locker_is_not_ours`, `only_numeric_folders_are_processes`, `an_unreadable_proc_is_not_locked` (`timer/lockscreen/tests.rs`), `the_shutdown_is_wired_through_one_path` (`tests/guardrails.rs`), `shutdown_warns_then_runs_poweroff`, `a_past_shutdown_at_start_warns_with_a_bubble_before_any_power_off`, `a_shutdown_due_while_locked_waits_for_the_unlock_then_warns`, `a_shutdown_and_a_timer_run_side_by_side` (`tests/headless_session.rs`)

### A typed text is one argv element and is never logged

An entry with `ask` takes what is typed in the box as an answer, not a filter, and hands it to `run`
as the one whole element `{text}`, as `{choice}` is: spaces, quotes, `;` or a leading `-` stay inside
that argument, and no shell is put in front. `{text}` may not be the program, nor appear in `state`
or `choices_cmd`, and `ask` and `confirm` are refused together (the typed text is the
confirmation). The text can be a host, a path or a secret, so it is never in a log line, the history
(which keeps the entry and its fixed choice), a `{:?}` of the action, or another mode's query when
the prompt is left by a click.

Date: 2026-11-24 · Source: tests `the_typed_text_is_one_whole_argv_element`, `text_is_never_recorded_in_history_nor_shown_by_debug`, `a_built_ins_choice_is_recorded_under_its_key_and_its_text_never` (`launcher/run/tests.rs`), `invalid_rules_make_the_file_invalid` (`launcher/tools/config/tests.rs`), `a_typed_scan_target_never_shows_in_debug_output`, `an_empty_text_runs_nothing` (`launcher/tools/nav/tests.rs`), `a_mode_click_from_the_prompt_drops_the_typed_text` (`launcher/model/tests.rs`), `the_tools_logs_are_fixed_words` (`tests/guardrails.rs`)

### The diagnostics tools run in the held terminal, and the scan's target is typed

The network scans, the USB events and the USB reset run in `terminal_hold`: the output stays
readable, and `sudo` (the UDP, OS and aggressive scans, the `modprobe` pair) asks for its password
there, so alpheus itself never runs as root and never sees a password. A scan's target, and the
ports of "Specific ports", are typed in the box through `ask`; each is one whole element, the target
comes after `--` (one starting with `-` is a host, never an option), and ports are digits, commas and
dashes only. The host discovery sweeps the subnet of the default route's interface, read from
`/proc/net/route` (nothing parses `ip`'s output), and only a local net from /16 to /30. The USB reset
asks a second Enter and its command is configurable (`[tools.builtin] usb_reset`, `[]` hides it). The
two shell scripts (the USB events' two streams in one terminal, the reset's chained `modprobe`) are
fixed and pinned; nothing typed goes into them. A journal preset is no diagnostics tool of this kind:
it opens the control center on that preset (`alpheus --tab diagnostics --preset <slug>`) in
`launcher.toml`'s plain `terminal`, never through `[actions] open_tab`, whose command takes a tab and
nothing else.

Date: 2026-11-24 · Source: tests `each_scan_builds_the_scripts_command`, `a_target_starting_with_a_dash_is_a_host_never_an_option`, `only_digits_commas_and_dashes_are_ports`, `host_discovery_without_a_subnet_runs_nothing`, `no_default_route_no_connected_route_or_a_tiny_or_huge_net_is_no_subnet`, `the_usb_events_script_is_the_scripts` (`launcher/tools/diag/tests.rs`), `each_scan_runs_in_the_held_terminal_with_its_target_whole`, `usb_reset_runs_the_configured_command_in_the_terminal_after_confirm`, `the_usb_events_run_the_fixed_script_in_the_held_terminal` (`launcher/tools/app/tests.rs`), `nmap_missing_hides_the_scan_entry`, `an_empty_usb_reset_hides_it` (`launcher/tools/catalog/tests.rs`), `every_builtin_argv_stays_on_the_allowlist` (`tests/guardrails.rs`), `a_diag_preset_entry_opens_the_tab_on_it` (`tests/headless_session.rs`)
