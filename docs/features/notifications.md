# Notifications

alpheus's own notification server: bubbles, a bell in the bar, a panel with the history, and
do-not-disturb. This page says what it does, how to use it, how to configure it, how it is built, which
rules the tests enforce on it, and what it costs. It describes the code as it is today. The panel also
shows the player of the media module in a card, with the same rows and keys as the media popup; the card
is described on the [media page](media.md).

## What it does

With `notifications` in `modules`, the daemon becomes the `org.freedesktop.Notifications` server (the
D-Bus service apps use to send "notify" messages), so apps such as Firefox, Chromium, Electron apps,
`notify-send` and portal apps send their notifications to it. It replaces mako: the spec measured mako at
6.7 MB PSS on one session. Without the module the name is never requested.

**The server.** It asks for the name with flags 0: while another server owns it (mako, for example),
alpheus waits in the queue and takes over when that server lets go; a server started later cannot take
the name over (mako, which asks with `DO_NOT_QUEUE`, exits). See "The notification server queues behind
another server and never replaces it" in [decisions.md](../decisions.md). It answers `Notify`, `CloseNotification`, `GetCapabilities` and
`GetServerInformation`, and broadcasts `NotificationClosed` and `ActionInvoked`. It decodes calls raw:
`image-data` and the other image and sound hints are stepped over, never read.

**What it keeps of a notification.** The app name is cut to 64 bytes, the summary to 256, the body to
1 KiB, labels to 64 bytes, at most 6 actions. In the summary and the body, markup is removed (the five
XML entities are decoded; in the body `<br>` is a line break), control and invisible characters are removed. The identity of a notification is
its desktop entry, else its app name; it keys the spam guards, the tags and the glyph. The glyph comes
from the bar's icon table, then the category (`email.*`, `im.*`, `network.*`, `device.*`,
`transfer.complete`), then the first letter, then a generic one.

**The bubbles.** One `Overlay` layer surface, top centre under the bar, on the focused output. A bubble
shows the glyph, the app name and the time, the summary in bold, and up to three body lines. At most
`max_visible` bubbles show at once (critical ones first, then the others, newest first); the rest wait.
A bubble stays for its timeout and then goes to the history; a critical one stays until clicked.

**The bell.** A segment of the bar's status block, never dropped on a narrow output and with no hue:
`󰂚` then ` N` in the accent when N entries are unread (`99+` from 100), the crossed bell `󰂛` under
do-not-disturb, and a dim bell when alpheus does not hold the name (another server does, or there is no
session bus).

**The panel.** A popup under the bell: the media card when a player is shown, the history newest first
(two lines an entry, 50 entries by default), `Do not disturb ○/●` and `Clear all`. Opening it marks
everything read.

**Do-not-disturb.** Only critical notifications get a bubble; everything else goes to the history. It can
be untimed or timed, and it is kept in a file across restarts.

**Rules and alerts.** `[[notifications.rules]]` mute or drop an app, or change its urgency. Low-battery
alerts (`[battery] warn`) and the failed-units notice (`[failed] notify`) go through this server too, and
wait while it does not serve; a Shield notice is dropped while it does not serve (one log line).

## Using it

**Bubbles.** A left click runs the notification's `default` action if it has one (then the bubble
closes unless the notification is resident), otherwise it only dismisses. A right click dismisses
(`NotificationClosed` with reason 2). A late click on a bubble that was replaced does nothing.

**The bell.** A left click opens the panel (a second click closes it); a middle click toggles
do-not-disturb.

**The panel.** ↑ ↓ (Ctrl+P, Ctrl+N), Home and End move; Enter runs the `default` action of a live entry
(on an entry that has expired it does nothing); Delete removes an entry; Escape closes. A left press on an
entry acts like Enter and a right press removes it; the wheel moves the selection. On the media card the
media popup's keys apply. At open the newest entry is selected, else the card, else `Do not disturb`.

**From a script or a key.**

- `alpheus-launch --dnd on|off|toggle` sets do-not-disturb; `--dnd on --for 30m` or
  `--dnd on --until 22:30` times it (1 minute to 24 hours).
- `alpheus-launch --clear-notifications` clears everything.
- Both answer an error when the `notifications` module is not in `modules`, and work with no session bus
  (the file is still written).
- In the quick panel the do-not-disturb row opens a duration detail (Enter or →); Space still toggles.

**Do-not-disturb and a screen capture.** With `dnd_while_sharing = true` (needs the `privacy` and
`notifications` modules), a screen capture turns do-not-disturb on and its end turns it off, unless you
changed it meanwhile: your own choice always wins.

## Configuration

`[notifications]` in `bar.toml` (the struct is `NotificationsConfig` in `config.rs`); the server takes
it when it starts, so a change takes effect when `alpheus-demon` restarts (bar.toml is read once at start). Unknown keys are
refused.

| Key | Default | What it does |
| --- | --- | --- |
| `timeout` | `5` | Seconds a bubble stays when the app leaves the choice to alpheus (`expire_timeout` -1) |
| `timeout_min` | `2` | An app's own timeout is clamped up to this many seconds |
| `timeout_max` | `20` | ... and down to this many; 0 from an app means this too. Needs `1 ≤ timeout_min ≤ timeout ≤ timeout_max ≤ 600` |
| `max_visible` | `4` | Bubbles shown at once, 1 to 8 |
| `width` | `40` | A bubble's width in cells, 24 to 80 |
| `history` | `50` | Entries kept for the panel, 0 to 200 (0 keeps none; bubbles still show) |
| `dnd_while_sharing` | `false` | Do-not-disturb while a screen capture shows |

`[[notifications.rules]]` (at most 32, first match wins):

| Key | What it does |
| --- | --- |
| `app` | The whole app name or desktop entry, any case; trimmed, not empty, at most 64 bytes, no control or invisible character |
| `action` | `"mute"`: no bubble, kept in the history and counted unread; `"drop"`: answered and forgotten |
| `urgency` | `"low"`, `"normal"` or `"critical"`: replaces the app's own (not with `drop`) |

A rule needs `action` or `urgency`.

Related keys elsewhere: `[battery] warn = [20, 10, 5]` (1 to 99, sorted and de-duplicated; the lowest is
critical; `[]` turns the alerts off; needs `battery` and `notifications`), `[quick] dnd_until` (the quick
panel's "Until" entry, `HH:MM`), and `[hooks] dnd` (a command run when do-not-disturb changes).

```toml
[notifications]
timeout = 8
max_visible = 3

[[notifications.rules]]
app = "Chatty"
action = "mute"
```

A `modules` list of your own needs `"notifications"` for the bell and the server.

## How it is built

Files in `crates/alpheus-daemon/src/notify/`, and the parts of other files that carry it:

- `mod.rs`: the pure model. The cleaners (`app_text`, `summary_text`, `body_text`, `hint_text`,
  `action`, `strip_markup`), `Incoming` (what a call gives), `Entry` (what the bubbles and the panel
  see) and `Center`: ids, tags and revisions, `notify`, `close`, `invoke`, `expire`, `next_expiry`,
  `bubbles`, `history`, the token buckets, the rules and the unread count. The tests are in `tests.rs`.
- `app.rs`: the wiring on the daemon's state. `start_notifications` picks where the server lives,
  `notify_view` gives the bell its view, `dnd_apply` is the one path for every do-not-disturb change,
  `notifications_verb` answers the two verbs, and `battery_deliver`, `failed_deliver` and
  `shield_notice` hand a notice to the server only while it serves.
- `dnd.rs`: do-not-disturb as a pure model (`Dnd`, the three forms of the state file).
- `battery_alert.rs`: the alerts as a pure model (`BatteryAlerts`, `incoming`).
- `bubbles.rs`: the pure layout of the bubbles (rows, spans, the hit map, the click rule).
- `bubbles_surface.rs`: the layer surface, its buffers, the pointer and the expiry timer.
- `panel.rs` and `panel_app.rs`: the panel's pure rows, selection and keys, and its wiring.
- Elsewhere: `session_bus/notifications.rs` (the server's D-Bus side), `session_bus/codec.rs` (decoding
  the calls and calling the cleaners), `state_file.rs` (the do-not-disturb file), `segments.rs` (the
  bell), `quick/dnd.rs` (the duration detail), `tray/bridge.rs` (`notify_do`, the one way the daemon
  reaches the server), `startup.rs` (the clock tick that re-checks a timed end).

**Where the server lives.** It uses the tray's connection or the bar's link (opened for it if nothing
else has), so it adds no connection of its own, and no thread.

**One path each.** Every change the daemon makes to the server goes through `notify_do` (and, with the
bubbles and the panel following, `notify_act`); every do-not-disturb change goes through `dnd_apply`; the
bubbles' surface is changed only by `bubbles_sync`.

**One timer each.** The bubbles' timer is armed for the earliest finite expiry of a live bubble, shown or
hidden by do-not-disturb, and for nothing else. The timed do-not-disturb has its own, armed only while an
end is set; the clock's minute tick re-checks the wall clock too.

**The path of one notification.** A client calls `Notify`. `session_bus/codec.rs` reads the arguments and
calls the cleaners; the server asks `Center::notify`, which applies the rules, finds the entry it replaces
or merges into, decides whether it gets a bubble, and answers an id. The host reads
`take_bubbles_change`, and `bubbles_sync` builds the stack (`bubbles::stack`), creates or updates the
surface, and `bubble_timer_sync` arms the timer. A click comes back as `bubbles_pointer`, then
`notify_activate`, then `Center::invoke` or `close`, whose effects leave as `ActionInvoked` and
`NotificationClosed`.

**The path of a do-not-disturb change.** The bell's middle click, the panel, the quick row, a verb or a
capture reach `dnd_apply`: it keeps the state, logs a fixed line, writes the file, tells the server when
on or off changed, repaints the quick panel's row and moves the timer. A state read at start goes through
`dnd_load`.

## Rules it keeps

- `the_notification_server_has_no_panicking_path`: `notify/mod.rs` (the model), the server's consumer
  (`session_bus/notifications.rs`), `notify/app.rs`, `notify/dnd.rs`, `notify/bubbles.rs`,
  `notify/bubbles_surface.rs`, `notify/panel.rs`, `notify/panel_app.rs`, `state_file.rs`,
  `session_bus/codec.rs` and the signal builders of `session_bus/bus.rs` hold no `.unwrap()`, `.expect(`
  or `panic!(` in their production code (everything but the items after `#[cfg(test)]`).
- `the_notification_rules_log_nothing`: `notify/mod.rs` writes no log line, so a rule never names an app
  or a text.
- `the_tray_spawns_no_thread`: `notify/mod.rs`, `app.rs`, `dnd.rs`, `battery_alert.rs`, `bubbles.rs`,
  `bubbles_surface.rs` and `panel.rs` start no thread and run no command (`panel_app.rs` is not on its
  list).
- `every_dnd_change_goes_through_dnd_apply`, `the_dnd_tick_fires_before_checking` and
  `the_clock_rechecks_a_timed_dnd`: one path for the flag, a timer's callback clears its own record before
  anything arms it again, and the clock's tick calls `dnd_check_clock`.
- `the_osd_and_the_bubbles_are_created_on_the_overlay_layer`: the bubbles' surface is on `Overlay`.
- `the_panel_marks_read_only_after_it_opened`: the panel marks the history read only once its popup opened
  (spec § 4, "State"; a refused open leaves the count on the bell).
- `on_bar_message_feeds_the_battery_alerts` and `a_new_server_gets_the_last_battery_reading`: the alerts
  are fed before the bar keeps a message, and a server that starts serving gets the last reading again.
- `nothing_resident_while_the_launcher_is_closed`: counts the timers the daemon builds, one in
  `bubbles_surface.rs` and one in `app.rs`.
- `tests/notify_inbound.rs` proves with a counting allocator that an 8 MiB `image-data` costs the decode
  less than 4 KiB more than none; `tests/notifications_private_bus.rs` runs the server against a fake
  client and a fake mako on a private bus.
- The headless tests that wait for a bubble, a click, an expiry or the panel
  (`a_bubble_appears_top_centre_on_the_focused_output`,
  `an_expired_bubble_sends_closed_1_and_its_surface_goes`, `do_not_disturb_shows_only_critical_bubbles`,
  `the_bell_opens_the_panel_with_the_card_and_the_history`,
  `dnd_from_alpheus_launch_is_saved_shown_and_restored`, `a_timed_dnd_is_saved_restored_and_ends`,
  `a_past_or_sharing_dnd_file_is_cleaned_at_start`, `the_users_choice_wins_over_sharing`,
  `mako_holding_the_name_dims_the_bell_until_it_stops`) run against a private bus and a headless
  compositor; no test uses the real session or system bus.
- The reasons are in [decisions.md](../decisions.md): "The notification server queues behind another
  server and never replaces it", "A bubble's timeout follows its urgency", "A bubble is given by guards in a
  fixed order, and a critical one always shows", "A bubble's timeout starts when it first shows", "An identical notification within one second is
  merged, unless it would add a bubble", "A notification's text is cleaned in one linear pass", "An
  unknown `replaces_id` is accepted as that id", "A rule is matched on the whole app name, any case",
  "The do-not-disturb file holds one of three forms", "The user's choice wins over a capture", "A timed
  do-not-disturb ends on the wall clock", "A battery alert is given only while the server serves", "The
  bubbles are an Overlay surface, opened again once per stack" and "The bubbles move to the focused
  output only when the stack changes".

## What it costs

Measured in release, back to back, 3 runs (the [memory tables](../measurements.md) have the figures and the method):

- **Linked in:** +122 to +248 KB at start over the build before the notifications; binary +140 KiB
  stripped (5.34 to 5.47 MiB). The rules, four battery thresholds and `dnd_while_sharing` configured
  read −420 to +64 KB at start (one base run read high; −56 to +64 KB in the other two; bound 128 KB,
  `small_features_cost_nothing_at_start`).
- **Module on, serving:** 0 to 4 KB in mode `auto`, where the portal's link is already open. A 50-entry
  history 192 to 198 KB; four bubbles 326 to 774 KB, and −84 to +76 KB over the history before them once they are closed
  (`notifications_pss_idle_history_and_bubbles`).
- **At rest:** 0 loop wake-ups with a sticky bubble shown and once every bubble is gone
  (`at_rest_no_wakeups_with_notifications`), and with a timed do-not-disturb running
  (`at_rest_no_wakeups_with_a_timed_dnd`, 10 s); no thread is added.
