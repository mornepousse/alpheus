# The on-screen display

A small pill that shows a volume, microphone or brightness level, or a state that just changed
(Bluetooth, do-not-disturb, the power profile, the keyboard layout, a new track), for a moment, bottom centre of the screen. This page says what it does, how to use it, how to configure it, how it is built, which
rules the tests enforce on it, and what it costs. It describes the code as it is today.

## What it does

The pill is one row: a glyph, a level bar and the percentage, in the popups' pixel frame, about 32
cells wide. It is shown bottom centre on the focused output, a fixed margin above the edge, and
hides 1.3 s after the last change. One pill at a time: a newer one replaces the content of the one
that is shown. It never takes a click and never takes the keyboard.

- **Volume** (the default sink). The glyph follows the level as the bar's sound module does (muted,
  low below 34 %, medium below 67 %, high above). Muted dims the glyph, the bar and the number and
  keeps the level. Above 100 % the whole bar fills in the warning colour (unless muted) and the real number
  is shown (`130%`).
- **The default sink changes.** The pill shows the new sink's name (cut to fit) in place of the
  bar, with its level.
- **Microphone** (the default source). A glyph for live or muted, with the source's level, shown
  **only when its mute flips**. A change of its level never shows.
- **Brightness** (the laptop's backlight). A sun glyph, the level and the percentage, shown only when
  asked (`alpheus-launch --osd brightness`).

Five more kinds are opt-in, each listed in `[osd] kinds`. Their pill is a glyph, a label and a
word where the percentage sits; "off" dims the glyph:

- **Bluetooth**: `󰂯 Bluetooth … on` or `off`, when the adapter is powered on or off. A device
  connecting or leaving shows nothing, and the pill never names a device.
- **Do not disturb**: `󰂛 Do not disturb … on` or `󰂚 … off`, from the bell's state, and only
  while alpheus serves the notifications (another server's flag is not ours to show). A toggle by
  `alpheus-launch --dnd`, the bell, the quick panel or `dnd_while_sharing` all show it.
- **Power profile**: `󰾅 Power profile … balanced` (the quick panel's word: `saver`, `balanced`,
  `performance`), when power-profiles-daemon's active profile changes.
- **Keyboard layout**: `󰌌 French (AZERTY)`, the layout's full name as the compositor gives it
  (whatever `[keyboard] short` says), when it changes. The `keyboard` bar module is not needed. A
  burst of changes (a keyboard plugged in) shows one pill, replaced in place, with the last name.
- **Track**: `󰐊 <title>` (the bar's media title, cut to the pill with an ellipsis), when the
  active player's title changes while it plays. A track skipped while paused shows nothing when it
  resumes, a pause and a resume of the same track show nothing, and neither does the burst of
  metadata in the second after a player appears. The title is on screen only when `track` is
  listed, and it is never logged (the log says `track changed`).

The same rules hold for each: the first state after its source (re)starts is the baseline and shows
nothing, nor does a change in the second after it; a source going down (BlueZ or the adapter gone,
the system bus down, the notification name lost, no active profile, no layout name, the compositor
or the last player gone) takes a new baseline.

Volume and microphone changes are passive: they come from the PipeWire state the bar already
follows (the `pw-dump` child of the sound source), whatever caused them (a key, pavucontrol, a
Bluetooth headset). Nothing shows for the first state after `pw-dump` starts, for 1 s after it, nor
for 1 s after a default device appears, goes or is switched, and nothing shows for a change under a
whole percent. Each stream (sink, source) has its own grace. The switch itself shows the sink's name
card unless an earlier grace is running; inside the second, a card on screen follows a second
switch or a key press, and a card waiting for the new sink's volume shows once it arrives, if that
is inside the second.

## Using it

There is nothing to click. Change the volume, switch the default sound output, or mute the
microphone, and the pill appears. For brightness, the key that changes it must also run
`alpheus-launch --osd brightness`, after `brightnessctl`, since the OSD never writes brightness (the
quick panel's slider changes it through `brightnessctl`) and never watches it (a watcher would show
the pill when the idle daemon dims the screen). The verb
answers 0 and shows nothing when the `osd` module is off or no backlight is found, and exits 3 when
the daemon cannot be reached. It also shows nothing without `brightness` in `[osd] kinds`.

A held key keeps one surface up and repaints it until the last change.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar). `osd` is in the default
`modules`. A theme change or a restyle repaints a shown pill.

| Key | Default | Meaning |
| --- | --- | --- |
| `[osd] timeout_ms` | `1300` | milliseconds the pill stays after the last change; 600 to 5000 |
| `[osd] width` | `32` | cells of the pill's row; 24 to 60 |
| `[osd] margin` | `48` | logical px between the pill and the output's bottom edge; 0 to 400 |
| `[osd] backlight` | `""` | a device name in `backlight_dir`; empty means the first one in sorted order |
| `[osd] kinds` | `["volume", "mic", "brightness"]` | which pills show, each once; also `"bluetooth"`, `"dnd"`, `"profile"`, `"layout"`, `"track"`. An unknown name, a name twice, or `"dnd"` without `notifications` in `modules` makes the file invalid |
| `[osd] backlight_dir` | `/sys/class/backlight` | where backlight devices are listed; an absolute path; read on the verb, and when the quick panel shows its brightness row (again 150 ms after it runs `brightnessctl`) |

An unknown key or a value out of range makes `bar.toml` invalid: the daemon logs one line and runs on
the defaults.

A kind brings the source it reads, as the module once brought `pw-dump` whatever it showed:
`volume` and `mic` the `pw-dump` child (and its OSD scope: the microphone and the names), `bluetooth`
the BlueZ domain on the system bus, `profile` power-profiles-daemon's, `track` the MPRIS players on
the session bus (as the `media` module does), `layout` the compositor's keyboard layout (as the
`keyboard` module does: Mango's two snapshot fields, Sway's `GET_INPUTS` and `input` events).
`brightness` reads on the
verb only, and `dnd` reads the bell, which the `notifications` module owns. So a list without
`volume` and `mic` starts no `pw-dump` for the OSD (another module may still start it), and kinds
bring nothing while `osd` is not in `modules`. See [decisions](../decisions.md), "An OSD kind is
opt-in and brings the source it reads".

## How it is built

Two files in `crates/alpheus-daemon/src/osd/`, and the backlight's reader beside them:

- `mod.rs`: the model, pure. `Watch` keeps the last sound state and each stream's grace and decides
  what a sound message means (`Heard`: the baseline, nothing, or a `Pill` to show); `side` applies the
  rules to one stream (the sink or the source). `Passive<T>` is the same baseline, grace and change
  for one value with no level, and `Kinds` holds one per passive kind with what each message means
  (BlueZ's power, the bell's do-not-disturb while serving, the active profile, the compositor's
  layout name). `TrackWatch` is the track's: the title is learnt whatever the player says, and a
  new one shows only while it plays. `Osd` keeps the pill
  that is shown with its deadline. `text_row` draws a passive pill's glyph, label and word. `rows` builds the pill's one row of cells (`level` draws the glyph, bar and percentage,
  which the quick panel's sliders share), and `parse_level` and `brightness_percent` turn the
  backlight's two texts into a level.
- `surface.rs`: the Wayland side, kept thin. `OsdSurface` is one field of `App`, with three fields of its own (the model,
  the open surface `OpenOsd`, and the hide timer); whether `osd` is in `modules` is read from the
  config each time (`osd_enabled`, and the kinds `osd_shows`), not stored. `osd_sound` takes every sound message;
  `osd_passive` takes the Bluetooth, profile and compositor messages in `on_bar_message`, the bell's
  view in `notify_view` and the media view in `media_view` (neither passes `on_bar_message`), and
  returns at once for a kind not listed;
  `osd_verb` answers the brightness verb; all end in `osd_show`, which replaces the pill, moves the deadline and calls
  `osd_sync`, the one place that creates or updates the surface (`osd_create`, `osd_update`) or
  drops it (`osd_drop`). `osd_draw` paints the row after each dispatch pass and commits nothing when
  the row did not change; `osd_expire` is the timer's callback.
- `backlight.rs` (top level, shared with the quick panel) reads `<backlight_dir>/<device>/brightness`
  and `max_brightness` with `read_capped`: nothing is written, watched or kept.
- The sound model that the OSD reads is in `pipewire/` and `system.rs`: it follows the default
  source and both defaults' names. With `osd` in `modules` and a `volume` or `mic` kind the sound
  source also publishes a microphone's mute and the names (never a microphone level alone); without it, only the sink's
  volume.

There is one timer, the pill's hide timer (`osd.timer`, a `DeadlineTimer`), armed only while a pill
shows and moved back on each change. There is no thread. The surface is an `Overlay` layer surface
anchored at the bottom with an exclusive zone of 0, with an empty input region, its own pool and
two buffers; it is created on the first change and dropped, pool included, when the timer fires (the
memory is handed back with `release_memory`).

**The path of a volume change.** `pw-dump` reports a change; the sound source sends a `Msg::Sound`;
`on_bar_message` (in `startup.rs`) hands it to `App::osd_sound` before the bar applies it. `Osd::sound` asks `Watch::sound`,
which answers `Heard::Show(Pill::Volume(..))` when the level moved outside any grace. `osd_show`
stores the pill with its deadline, `osd_sync` builds the row (`osd::rows`) and creates the surface on
the focused output (or repaints the open one), and the timer is armed for the deadline. After the
dispatch pass `flush_osd` calls `osd_draw`. When the timer fires, `osd_expire` hides the pill and drops
the surface.

**The path of the brightness verb.** `alpheus-launch --osd brightness` sends `Request::Osd` on the
launcher's socket; the daemon answers it before the launcher's own handling, in `osd_verb`, which
reads the backlight once and calls `osd_show` with `Pill::Brightness`.

## Rules it keeps

- `the_tray_spawns_no_thread`: `osd/mod.rs`, `osd/surface.rs` and `backlight.rs` start no thread,
  process or command.
- `the_osd_only_reads`: those files write no file, create nothing, change no permission and make no
  raw system call.
- `the_osd_has_no_panicking_path`: none of them panics.
- `the_osd_logs_are_fixed_words`: every log line of those files is a literal or interpolates only an
  error's word, the output's connector name, a size, a count, a literal reason and `describe`'s
  words; `describe`'s arms are literals and numbers (`bluetooth on`, `dnd off`, `profile changed`,
  `layout changed`, `track changed`), never a device name, a profile, a layout, a title or a label.
- `the_osd_and_the_bubbles_are_created_on_the_overlay_layer`: the surface is created on
  `Layer::Overlay`.
- `nothing_resident_while_the_launcher_is_closed`: the hide timer is the only one `osd/surface.rs`
  builds.
- The unit tests in `osd/tests.rs` pin the trigger rules at their boundaries (the grace's exact end,
  the microphone on a flip only, the name card), the pill's rows and the backlight's parsing. The
  headless tests (`osd_a_volume_change_shows_the_pill_bottom_centre_then_it_goes`,
  `osd_the_start_up_state_shows_nothing`, `osd_a_held_key_keeps_one_pill_up_until_the_last_change`,
  `osd_the_pill_never_takes_a_click`, `osd_the_microphone_and_a_default_sink_switch_show_their_pills`,
  `osd_a_change_while_shown_repaints_the_level`, `osd_a_theme_change_repaints_the_shown_pill`,
  `osd_no_wakeup_at_rest_before_or_after_a_pill`, `osd_off_never_reads_the_sound`,
  `osd_the_brightness_verb_reads_the_fake_backlight_and_shows_the_pill`,
  `osd_a_volume_change_replaces_the_brightness_pill_and_pushes_its_deadline_back`,
  `osd_without_a_backlight_or_the_module_the_verb_exits_0_and_shows_nothing`,
  `osd_another_output_going_away_leaves_the_pill`, `osd_the_pills_own_output_going_away_drops_it`,
  `osd_a_bluetooth_flip_shows_its_pill`, `osd_a_profile_change_shows_its_pill`,
  `osd_a_dnd_toggle_shows_its_pill`, `osd_kinds_off_read_no_bluez`,
  `osd_a_layout_change_shows_the_pill`, `osd_a_track_change_while_playing_shows_the_pill`,
  `osd_the_new_kinds_off_cost_no_wakeup`)
  pin the wiring on an
  isolated Sway with a fake `pw-dump`, a fake backlight directory and, for the passive kinds, a fake
  BlueZ and power-profiles-daemon on a private system bus, a fake Mango, and a fake player on a
  private session bus; no test reads `/sys` or calls
  `wpctl` or `brightnessctl`.
- The reasons are in [decisions.md](../decisions.md): "The microphone pill shows only when its mute
  flips", "A sound change within a second of a start or a new default device shows nothing", "The OSD
  counts a volume change in whole percent", "Brightness is shown on request, read once", "The OSD pill
  is an Overlay surface with an empty input region, made on show", "The pill follows the focused
  output only when its content changes", "The OSD logs once when its surface appears and once when
  it goes", "An OSD kind is opt-in and brings the source it reads".

## What it costs

Measured in release, back to back, 3 runs (the [memory tables](../measurements.md) have the figures and the
method):

- **Linked in:** within noise of the build before it (−28…+172 KB), binary +56 KiB stripped.
- **Module on, nothing shown:** 0 to 96 KB, measured; the model is the last sound state and three
  instants, and there is no surface, timer or sysfs read until the verb runs.
- **One pill:** 4 to 30 KB while shown, −38 to −12 KB once gone (`osd_pss_idle_shown_and_gone`).
- **At rest:** 0 loop wake-ups once the pill went (`at_rest_no_wakeups_after_the_osd`), a passive
  pill's included.
- **Passive kinds:** three of them on keep 0 to 4 KB of anonymous memory (`osd_kinds_pss_off_and_on`);
  a kind that brings a source costs that source (the `bluetooth` kind alone: the BlueZ domain and
  the D-Bus thread, `+ osd kinds` in `startup_pss_per_source`). The kinds' code: +16 KiB stripped.
