# The media module

What is playing, in the bar, and a popup to control it. This page says what it does, how to use it,
how to configure it, how it is built, which rules the tests enforce on it, and what it costs. It
describes the code as it is today. The notification panel shows the same player in a card, with the
same rows and keys; that card is described with the notifications, except for what it shares with
this popup.

## What it does

The `media` module follows the programs that play sound or video and speak MPRIS (the D-Bus
interface media players use to say what they play and to be controlled): Spotify, VLC, Firefox and
so on. It shows the active one in the bar and opens a popup under it.

**In the bar.** `▶ Title` while the active player plays, `⏸ Title` while it is paused. The title is
cut to `title_max` cells, ending with `…`. With no title the bar shows the last segment of the track's
url (query and fragment dropped, `%XX` decoded), then the player's name. The module takes no room,
and no separator, when no player is there or every player is stopped. It is its own block under the
block styles. Playing, it takes the purple hue as `module_colors` says; paused, the whole text is dim
whatever `module_colors` says.

**The active player.** The one chosen in the popup stays active while it exists and is not stopped.
Otherwise the best status wins (playing over paused), and a tie goes to the player whose status
changed last. A stopped player is never shown.

**In the popup.** Title and artist (the `xesam:artist` entries joined with `, `), then the elapsed
time with a progress bar and the length (`1:42 ━━━━━───── 4:05`; `h:mm:ss` from one hour; the row is
hidden when the length is unknown, as for a stream), then the buttons `⏮ ⏯ ⏭` (a button the player
cannot use is dim and ignored). With two players or more, a separator and the list of players, `●`
on the active one. A player is named by its `Identity`, else by the first label after
`org.mpris.MediaPlayer2.`.

## Using it

**On the bar.**

- Left click: opens the popup under the module; a second left click closes it.
- Middle click: play or pause the active player.
- Wheel up: next track. Wheel down: previous track.
- A thing the player says it cannot do (`CanPause`, `CanPlay`, `CanGoNext`, `CanGoPrevious` false) is
  a no-op.

**In the popup.**

- ↑ ↓ (and Ctrl+P, Ctrl+N) move over the rows that can be selected: the buttons, and the players.
  They stop at the ends and do not wrap round.
- ← → choose a button, on the buttons row; a button the player cannot use is skipped.
- Enter acts on the selected row: a button runs, a player becomes the active one.
- A left press on a button or a player does what Enter does; hovering does nothing.
- Esc closes it. So do a click outside, a restyle that moves the bar, and the last player going
  away (`player gone`). While it is open, the next active player is shown if the active one goes.

The popup is opened with a grab (`Input::Grab`); the README notes what that means for the bar's
keyboard on Hyprland.

The module never writes a player's volume, never seeks and never raises or launches a player.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar). `media` is in the default
`modules`.

| Key | Default | Meaning |
| --- | --- | --- |
| `[media] title_max` | `30` | cells of the title in the bar, `…` included; 8 to 80 |
| `[media] ignore` | `["kdeconnect"]` | substrings of a player's bus name: a player whose name contains one is never queried. `playerctld` is always ignored, whatever this holds; an empty entry matches nothing |

An unknown key or a value out of range makes `bar.toml` invalid: the daemon logs one line and runs on
the defaults, as for every other table.

## How it is built

Three files in `crates/alpheus-daemon/src/media/`, and the bus side in
`crates/alpheus-daemon/src/session_bus/mpris.rs`:

- `mod.rs`: the model, pure. `Players` (at most 16 `Player`s, each with its `Status`, `Track`, `Caps`,
  `rate` and last `PositionSync`), the rule for the active player (`Players::active`), `view` (what
  the bar shows), `display_title`, `truncate_title` (cut by display width, never inside a cluster),
  `position_now` and `clock`. A value arrives as a `Value` that `session_bus/codec.rs` decoded, so
  this file never names D-Bus.
- `popup.rs`: the popup's content, pure. `lines` (which rows exist), `rows` (the cells), `key` and
  `press` (what a key or a left press asks: a `MediaAction`), `tick_due` and `player_tick` (when the
  open popup must redraw next) and `next_deadline`. `MediaPopup` keeps the selection by what it
  shows (`Chosen`), so it follows its row when rows or players come and go (`fit`).
- `app.rs`: the wiring on `App`. `OPS` is the popup's row in the table of popups (`popup::POPUPS`);
  `media_toggle` opens it; `media_key` and `media_pointer` hand input to `popup.rs`; `media_action`
  carries out the answer (redraw, close, a control to the active player, a pin); `media_pin` applies
  a pin and takes the bar's new view at once; `media_popup_refresh` rebuilds and repaints the rows;
  `media_timer_sync` and `media_tick` keep the tick; `media_closed` runs whatever closes the popup.
- `session_bus/mpris.rs` talks to the bus: it lists the players at start, follows
  `NameOwnerChanged` and `PropertiesChanged`, asks for what is missing, and sends the controls.
  `App::mpris_do` is the one way from `App` to it. It lives on the tray's session connection when
  the tray runs, else on the bar's own session link (`link.rs`), which is one connection shared with
  the portal and the notification server. The bar's view reaches the bar as `Msg::Media`. The
  segment is `segments::media`; a middle click or the wheel become a `Control` in
  `actions::media_control`.

There is no thread. The only timer is the popup's 1 s tick (`media_timer`, a `DeadlineTimer`), armed
only while the popup, or the notification panel's media card, is shown and the active player plays,
before the end of its track and with its position known. A tick still ahead is kept when the popup
refreshes (`next_deadline`). At a rate above 1 the tick is still about one redraw per real second.

**The path of one change.** A player sends `PropertiesChanged` on the bus. `session_bus/mpris.rs`
maps it to its player by unique name and calls `Players::apply`, which updates the status, track,
rate and abilities, forgets the position after a status, track or rate change, and asks for
`Metadata` again when it came invalidated. The bar's view is rebuilt once per batch
(`take_view_change`) and reaches `App::media_view`, which repaints the bars only if what they show
changed. With the popup open, `media_popup_refresh` rebuilds its rows, and closes it with
`player gone` if no player is shown any more.

**The path of a left click.** The bar's click handler finds the module's zone and calls the popup
table's `toggle`, `media_toggle`: it builds `MediaPopup::new` and its rows, opens the popup, tells
the player side the popup is open (`popup_opened`: the `Seeked` rule goes on, the position of the
active player is read once), and arms the tick. `popup_close` ends with `media_closed`: the `Seeked`
rule goes off and the tick with it.

## Rules it keeps

- `the_tray_spawns_no_thread`: `media/mod.rs` and `media/popup.rs` start no thread, process or
  command.
- `the_quick_panel_lets_go_of_the_sound_and_its_timer_whatever_closes_it`: the media popup's row, and
  the notification panel's, name `media_closed`, which drops the `Seeked` rule and the tick.
- `nothing_resident_while_the_launcher_is_closed`: counts the timers the daemon builds, so the media
  tick cannot be joined by another unnoticed.
- `the_media_files_have_no_panicking_path`: the production code of `media/mod.rs`, `media/popup.rs`
  and `media/app.rs` has no `.unwrap()`, `.expect(`, `panic!(`, `unreachable!(` or indexing by a
  constant, `i` or `..n`.
- The unit tests of `media/tests.rs` and `media/popup/tests.rs` pin the model and the popup's rows,
  keys and tick, with hostile players (an unknown status, a length sent as a string, an empty artist
  list, a 100 KB title, a newline in a title, a 17th player). `tests/media_private_bus.rs` runs the
  consumer against fake players on a private bus; the headless tests
  (`the_media_popup_opens_on_a_left_click_and_escape_closes_it`, `the_popup_buttons_reach_the_player`,
  `the_tick_runs_only_while_the_popup_is_open_and_the_player_plays`,
  `the_popup_moves_to_the_next_player_then_closes_when_none_is_left`,
  `a_player_chosen_in_the_popup_takes_the_bar`,
  `a_restyle_closes_the_media_popup_on_the_second_output_once`,
  `a_middle_click_and_the_wheel_reach_the_player_on_the_bars_link`) pin the wiring. No test calls a
  real player.
- The reasons are in [decisions.md](../decisions.md): "The active media player is the pinned one, else
  the best status", "One connection owning two MPRIS names keeps the first", "The media popup
  computes the position, and reads it only while open", "A player is labelled by its identity, else
  by its name", "A paused player's module is fully dim", "Media titles are cut by display width,
  never inside a cluster".

## What it costs

Measured in release, back to back, 3 runs (the [memory tables](../measurements.md) have the figures and the
method):

- **Linked in:** within noise of the build before it (−52…+32 KB), binary +92 KiB stripped.
- **Module on, no player:** 0 to 4 KB in mode `auto` (the connection is the portal's, already
  open); 286 to 395 KB in a fixed mode with the tray off (it opens its own connection); it shares
  the tray's when the tray is on. Three players: 85 to 112 KB. A hundred track changes: +0 KB
  (`media_module_pss_without_and_with_players`).
- **At rest:** 0 loop wake-ups with a player playing, the popup closed; the tick exists only while
  the popup shows.
