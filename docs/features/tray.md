# The system tray

The status icons of running applications in the bar, and their menus. This page says what it does,
how to use it, how to configure it, how it is built, which rules the tests enforce on it, and what it
costs. It describes the code as it is today. The month calendar that opens on the clock uses the same
popups; it has [its own page](calendar.md).

## What it does

The `tray` module draws one icon per application that registers a status-notifier item (Steam,
Discord, Telegram, nm-applet, blueman, KDE Connect and the like), and opens their menus in a popup
under the icon.

**Icons.** An icon is a Nerd Font glyph, picked from a built-in table by matching, in this order,
the item's `Id`, its `IconName`, its tooltip title, then the name of the process that owns it (a
case-insensitive substring match; Electron applications have an `Id` such as `chrome_status_icon_N`
and no `IconName`, so the process name is what identifies them). An application the table does not
know is shown as its first letter, in bold accent colour with no background, taken from its
`Title`, then its process name, then its `Id`, and `?` when none of them is known. Image icons are
never requested, so no application sends the bar a picture. An item whose `Status` is
`NeedsAttention` is drawn in the error colour. One that is `Passive` is dim by default.

**Menus.** The menu is drawn by the bar as a grid of cells, with the same list rendering and keys as
the launcher. It shows each entry's label (mnemonic underscores removed), whether it is enabled,
check marks and radio buttons, separators, and `▸` for a submenu. Item icons are ignored. A submenu
opens in the same popup, replacing the level, with a first row `◂` and the parent's label to go back.
The popup is between 12 and 48 cells wide, and a tall menu scrolls with `▴` and `▾`. Choosing an
entry sends the application a `clicked` event; the bar runs nothing itself.

**Tooltips.** Resting the pointer on an icon for half a second shows one line: the tooltip's title,
else the `Title`, else the `Id`, cut to the output's width.

## Using it

On an icon:

| Gesture | Effect |
| --- | --- |
| Left click | Sends `Activate`. If the item is a menu only (`ItemIsMenu`), or if `Activate` fails (Steam has none), its menu opens instead |
| Right click | Opens the menu; for an item with no menu, sends `ContextMenu` |
| Middle click | Sends `SecondaryActivate` |
| Wheel | Sends `Scroll` (vertical or horizontal) |
| Hover, ½ s | Shows the tooltip |

In a menu: the mouse selects by hovering and runs an entry on a click. ↑ and ↓ move, Enter runs the
entry (or opens a submenu), → opens a submenu, ← goes back one level, Escape closes. Only one menu
is open at a time. It closes on an outside click, on Escape, and after an entry runs. A press on the
icon whose menu is open, with the button that opened it, or with the right button, only closes the
menu.

A menu opens once its layout has arrived, with its first entry selected; if the application takes
more than 2 s, nothing is shown.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar). `tray` is in the default
`modules`. On a narrow bar the tray is removed right after the resources.

| Key | Default | Meaning |
| --- | --- | --- |
| `[tray] passive` | `"dim"` | how an item that says it is idle is drawn: `"dim"`, `"show"` (like an active one) or `"hide"` |
| `[tray] tooltips` | `true` | `false` arms no hover timer at all |
| `[tray.icons]` | empty | overrides of the glyph table: a key is matched as a lower-cased substring of the item's `Id`, `IconName`, tooltip title or process name, and its value is a glyph of one or two cells. At most 128 entries; a blank key, a control character, or two keys that differ only by case is an error |

An unknown key or a bad value makes `bar.toml` invalid: the daemon logs one line and runs on the
defaults, as for every other table. `[clock] calendar` and `week_numbers` belong to the calendar.

## How it is built

The tray is on a second D-Bus connection, to the **session** bus, non-blocking, whose file
descriptor is a source of the bar's own event loop. It adds no thread. The pieces are in
`crates/alpheus-daemon/src/session_bus/` (`bus.rs`: the connection, the typed calls and their
2 s timeouts; `codec.rs`: replies and signals read in place, with bounds; `watcher.rs`: the
`org.kde.StatusNotifierWatcher` object; `host.rs`: the items; `menu_session.rs`: one menu's
dbusmenu session; `router.rs`: `Tray`, which owns them all). This page is about the two files that
connect that to the bar, in `crates/alpheus-daemon/src/tray/`:

- `bridge.rs`: `tray_output` applies what a callback reported (the icons redrawn through
  `Msg::Tray`, the bus closing, a menu closed, asked for or changed); `tray_sync` runs after every
  callback, queues the answers the tray kept, keeps the deadline timer and asks for one frame
  callback; `tray_gesture_at` sends a click or wheel step to the item under the pointer;
  `mpris_do`, `notify_do`, `mpris_of` and `notifications_of` reach the media and notification
  consumers, which live on the tray's connection or, without the tray, on the bar's own link
  (`link.rs`).
- `menu_app.rs`: `menu_requested` asks for a menu, `menu_layout_changed` opens its popup on the
  first layout and patches it afterwards, `menu_action` carries out what the user did, and
  `menu_end` closes the session. `TrayMenu` is the state between those calls, one field of `App`.

The tray has two timers: its `DeadlineTimer` (`tray_timer`), armed only while calls are pending (each
has a 2 s timeout), and the tooltip's, armed only while the pointer rests on an icon.

Rules the tray keeps on what applications send: at most 64 items (16 per connection), 256 characters
for a title, tooltip, `Id`, `IconName` or menu label (control characters removed), 500 entries per
menu and a depth of 8. Properties are read with one `Get` per property (the first eight sent together
when an item appears), never `GetAll`, and a menu layout
is always asked at depth 1 with an explicit list of properties, so icon pictures never cross the bus.

**The path of one click.** The bar's click handler finds the icon's zone and calls
`tray_gesture_at`. It remembers the icon, its cells and the button (`menu.last_icon`) and hands
the gesture to `Tray::gesture`, which sends the call. If the answer is a failed `Activate`, the
router reports `open_menu`; `tray_output` takes the remembered press and calls `menu_requested`,
which sends `AboutToShow(0)` and then `GetLayout`. When the layout arrives, `menu_layout_changed`
builds the menu, opens the popup with `Input::Grab` and paints it. A click on an entry reaches
`menu_action`, which sends the `Event` and closes the popup (`Activated`), and `menu_end` removes
the match rule of the menu's session.

## Rules it keeps

- `the_tray_spawns_no_thread`: `tray/` and `session_bus/` start no thread, process or command.
- `tray_d_bus_is_confined_and_allowlisted`: `dbus_crossroads` is used in `session_bus/watcher.rs`
  alone, every call under `session_bus/` is built in `session_bus/bus.rs` from a typed `Call`, the
  list `TrayMethod::ALL` is pinned member by member, some tokens are forbidden (blocking calls,
  `REPLACE_EXISTING`, `ALLOW_REPLACEMENT`, libdbus's own session lookup), and `RequestName` asks for
  exactly two names: the watcher's, with `DO_NOT_QUEUE`, and the notification server's, with flags 0.
- `answers_left_by_the_settle_rounds_are_routed_from_an_idle`: `tray_sync` routes the answers a
  burst left waiting from an idle callback, and `mpris_do` ends in the syncs.
- `menu_repaint_launcher_open_and_the_interactivity_reset_keep_their_guards`: the menu finds its bar
  by the popup's parent surface, and the launcher opening drops the remembered press.
- `nothing_resident_while_the_launcher_is_closed`: counts the timers, thread starts, event-loop
  sources and keyboard bindings the daemon builds.
- `the_tray_adds_no_thread` and `at_rest_no_wakeups_with_a_populated_tray` (release memory tests): no
  thread more, no wake-up at rest with a populated tray.
- The unit tests of `tray/bridge/tests.rs` and `tray/menu_app/tests.rs` pin the three small rules
  that do not need a Wayland connection; the headless tests (for example
  `a_fake_apps_menu_opens_runs_an_item_and_closes`, `a_submenu_opens_with_right_and_back_with_left`,
  `steam_left_click_opens_the_menu`, `a_second_left_click_on_steams_icon_only_closes_its_menu`,
  `a_menu_answering_after_2_s_is_not_shown`, `a_dead_session_bus_closes_an_open_menu`) pin the
  menu's wiring against fake applications on a private bus. No test calls a real application.
- The reasons are in [decisions.md](../decisions.md): "A tray menu is shown only once its layout has
  arrived", "A tray menu opens with its first entry selected", "A submenu opens in the same popup",
  "A failed Activate opens the menu of the icon that was pressed", "A tray menu is closed by the
  button that opened it, or by any right click", "A press right after the compositor closed a menu
  only closes it", "A late submenu graft is refused once the selection has moved", "A burst of tray
  changes is redrawn once per frame callback", "A closed session bus is never reconnected".

## What it costs

Measured in release, 3 runs (the [memory tables](../measurements.md) have the figures and the method):

- **Linked in, tray unused:** +0.4 to 0.7 MB over the delivery before it (stripped binary
  4.49 → 5.36 MB: libdbus, dbus-rs, menus, popups, calendar), a one-time cost of the bigger binary.
- **Module on:** about 380 KB, and 10 items about 490 KB (budget 2 MB). 20 popup open and close
  cycles: +32 KB in each of 3 runs.
- **At rest:** 0 wake-ups with a populated tray; 6 threads, the tray adds none.
