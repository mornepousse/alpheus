# The control center

`alpheus` is the terminal control center: a ratatui program with eight tabs (Home, Sound, Network,
Bluetooth (BT), Power, Displays, Theme, Diag). This page is written one tab at a time, in the order the
tabs are brought to the house standard (`docs/STYLE.md`); today it holds the Sound, Network,
Bluetooth, Power, Home, Displays, Theme and Diag tabs, which is all of them.

How the program is laid out, for every tab. The state lives in `crates/alpheus/src/app/` (one
`App` that every key, mouse event and worker answer changes, with no terminal and no thread of its
own) and the drawing in `crates/alpheus/src/ui/` (`render` draws the whole screen and returns the
map of clickable regions). A tab that has code of its own has a file of the same name in each
folder: `app/sound.rs` and `ui/sound.rs`, `app/network.rs` and `ui/network.rs`, and so on. What every tab shares stays in `app/mod.rs`,
`app/tab.rs` (one row of plain data per tab), `ui/tabs.rs` (the item rows), `ui/rail.rs` (the
rail, the tab hues) and `ui/mod.rs` (the frame and the status line's hints).

## Sound

### What it does

The Sound tab lists the audio **outputs** (sinks), then the **inputs** (sources), each with a volume
slider, and a **Per-app mixer** button at the end. A dot marks the default output and the default
input. A node's name comes from `wpctl status`. While the audio data is loading the lists show
`…`; if the read failed they show its error; a list with nothing in it shows `none`.

```
 Outputs
 ▶   Dock D6000 Dell                 ██████████████████░░░░░░  75 %
   ● CasqueBT                        ███████████████░░░░░░░░░  62 %

 Inputs
   ● Stereo Microphone               ████████████████████████ 100 %

   [ Per-app mixer ]
```

(The classic layout, in a window narrower than 84 columns or shorter than 26 rows. From that size the tab is
drawn with the rail: a blue hue for its headings and gauges, the focused row as a band, and a
knob on each gauge. The items and their order are the same.)

### Using it

| Key (default) | What it does |
| --- | --- |
| ↑ ↓ | move to the previous or next item |
| ← → | lower or raise the volume of the focused node, like `-` and `+` |
| Enter | on an output or an input: make it the default; on the button: open the mixer |
| `-` `+` | lower or raise the volume of the focused node by 5 points |
| `/` or Ctrl+F | search: only the nodes whose name matches the text are listed (a fuzzy match) |
| Esc | clear the search; with an empty search, go back to Home |
| F5 | read the audio data again |

With the mouse: a click on a row selects it and makes it the default (the same as Enter); a click
or a drag on a gauge sets the volume; the wheel over a gauge changes it by 5 points, and the wheel
elsewhere moves the focus; a click on the button opens the mixer.

Two rules about the volume. It is never sent above 100 %: `wpctl` is called with `-l 1.0`. And a
node already above 100 % (wpctl allows it) is not lowered by `+`: a requested increase would be
clamped to 100 and would lower the sound (a comment in `App::set_slider`). The first `-` brings it
to 100 % (`set_slider` clamps; `volume_above_100_is_never_lowered_by_plus`). After a successful
volume change the tab is not read again: the value shown is already the new one,
and a read in the middle of a drag would make the slider jump (a comment in `App::op_done`).

The **Per-app mixer** button opens the program set under `[commands] mixer` in `config.toml`
(default `wiremix`) in alpheus's own window. The other keys (the number keys for the tabs, Ctrl+P
for the palette) are the same on every tab, and `[keys.bindings]` assigns other keys to any of them.

### Configuration

The tab has one setting of its own, in `config.toml`, read once when the control center starts:

| Key | Default | Meaning |
| --- | --- | --- |
| `[commands] mixer` | `["wiremix"]` | the program and arguments the Per-app mixer button runs |

The keys come from `[keys]` and `[keys.bindings]` (the names are `activate`, `increase`,
`decrease`, `search`, `refresh`; see the README's list of action names).

### How it is built

Two files, one per layer, and the tab's row in `app/tab.rs`.

- `app/sound.rs`: `items` (the focusable items, in drawing order), `App::visible_nodes` (the
  outputs and inputs that pass the search) and `App::set_default` (make a node the default). The
  audio data itself (`App::data.audio`) and the slider helpers (`slider_value`, `set_slider`,
  `nudge_slider`; a node's slider is a `SliderId::Node`) are in `app/mod.rs`, beside the volume
  and brightness sliders of Home.
- `ui/sound.rs`: `rows` (the heading, node and button rows) and `hints` (the keys named in the
  status line). A row is drawn by the shared item drawing in `ui/tabs.rs`.
- `app/tab.rs`: the tab's row says it is searchable, that it is read every 2 seconds while it is
  shown, from the `Domain::Audio` domain, and that its label column is 34 cells wide.

**Refresh.** Only the visible tab is read (`docs/specs/2026-10-02-alpheus-v1-design.md`, § 3:
nothing runs in the background and nothing runs when the window is closed). `App::on_tick` asks
the worker for `Domain::Audio` once the tab's period has passed; a failing read is tried again at
most every 15 seconds (a broken tool must not be relaunched every second: a comment in
`App::on_tick`).

**The path of one change**, choosing another output as the default:

1. Enter, or a click, reaches `App::activate`, which sees the focused item is a
   `SliderId::Node` and calls `set_default` (`app/sound.rs`).
2. `set_default` marks the node as default in its own list at once (when the audio data is
   loaded and holds the id), then calls `send_op` with
   `Op::SetDefault(id)`. `send_op` notes that `Domain::Audio` and `Domain::Volume` have a change in
   flight and leaves a `Request::Do` in the box the real loop empties.
3. The worker thread runs `wpctl set-default <id>` (`WpctlSound::set_default`), through the
   injectable command executor, and answers with `Update::Done`.
4. Until then a read of those domains that arrives is dropped (`App::on_update`). On the answer
   the tab is read again (`op_done`), so what is shown is what `wpctl` now says.

### Rules it keeps

- `activating_an_output_makes_it_the_default` (`app/tests.rs`): the output becomes the default, the
  default input does not move, and exactly one `SetDefault` is queued.
- `search_filter` (`app/tests.rs`): the search narrows the outputs and the items, and the focus is
  kept inside the list.
- `items_per_tab` (`app/tests.rs`): the order of the items, with the mixer button last.
- `volume_above_100_is_never_lowered_by_plus` (`app/tests.rs`).
- `the_sound_tab_shows_both_lists_the_mixer_button_and_its_hints` (`ui/tests.rs`): the rows, the clickable regions and the hint on the status line.
- `the_sound_tab_at_80_by_24_matches_its_snapshot` (`ui/sound/tests.rs`, its snapshot in
  `ui/sound/snapshots/`): the whole 80 by 24 screen.
- `every_launched_command_is_on_the_allowlist` (`tests/guardrails.rs`): every command the backends
  run is on the allowed list, `wpctl` included. That no test runs the real tool is CLAUDE.md's rule
  ("Never a real call in tests": the tests use the fake executor).

### What it costs

The control center is not measured tab by tab. What the specs fix for it as a whole: the first
render in under 100 milliseconds, and nothing read while a tab is not shown. This tab's reads are
one `wpctl status` every 2 seconds while it is shown, and nothing otherwise. (Reason for the 2
second period: not recorded.)

## Network

### What it does

The Network tab has the **Wi-Fi** switch, the list of **active connections**, and a **Choose a
network** button at the end. Each connection line gives its name, its kind (Wi-Fi, Ethernet,
WireGuard, VPN, Bluetooth, or the type NetworkManager reports) and the device it runs on; the
loopback connection is never listed. While the data is loading the list shows `…`; if the read
failed it shows its error; with no connection it shows `none`.

```
 ▶ Wi-Fi                            [■] on

 Active connections
   Wi-Fi      MON_WIFI_1  (wlp0s1f01)
   Ethernet   Wired connection 1  (enp0s0u1u2i10)

   [ Choose a network ]
```

(The classic layout, in a window narrower than 84 columns or shorter than 26 rows. From that size the tab is
drawn with the rail: a cyan hue for its heading, the switch as an on/off switch, a glyph for each
kind of connection, and the button with a glyph. The items and their order are the same.)

### Using it

| Key (default) | What it does |
| --- | --- |
| ↑ ↓ | move to the previous or next item |
| Space or Enter | on the Wi-Fi switch: turn Wi-Fi on or off |
| Enter | on the button: open the network tool |
| `/` or Ctrl+F | search: only the connections whose name matches the text are listed (a fuzzy match) |
| Esc | clear the search; with an empty search, go back to Home |
| F5 | read the network data again |

With the mouse: a click on the switch's row turns Wi-Fi on or off; a click on the button opens the
tool. The search narrows the list of connections only; the two items stay.

The switch changes at once and the command follows: the value shown is already the new one, and a
read that was launched before the change, and arrives after it, is dropped
(`stale_read_ignored_during_a_toggle`) so it cannot flip the switch back. If the state is not
known yet (still loading, or the read failed), the switch does nothing and a message says
`Wi-Fi: unknown state`.

The **Choose a network** button opens the program set under `[commands] network` in `config.toml`
(default `nmtui`) in alpheus's own window: alpheus leaves its screen, the tool runs, and alpheus
takes its screen back when it ends and reads the tab again, because the tool may have changed
anything. If the tool cannot start, the status line and a message say `Choose a network: <why>`.

### Configuration

The tab has one setting of its own, in `config.toml`, read once when the control center starts:

| Key | Default | Meaning |
| --- | --- | --- |
| `[commands] network` | `["nmtui"]` | the program and arguments the Choose a network button runs |

The keys come from `[keys]` and `[keys.bindings]` (the names are `toggle`, `activate`, `search`,
`refresh`; see the README's list of action names).

### How it is built

Two files, one per layer, and the tab's row in `app/tab.rs`.

- `app/network.rs`: `items` (the Wi-Fi switch, then the button). The network data
  (`App::data.network`, a `NetStatus`), the switch's change (`App::toggle`, `ToggleId::Wifi`) and
  the opening of the external tool (`Command::Open`, `External::Network`) are in `app/mod.rs` and
  `app/action.rs`: Home has the same Wi-Fi switch, so they are shared.
- `ui/network.rs`: `rows` (the switch, the heading, the connection lines, the button) and `hints`
  (the keys named in the status line). The switch and the button are drawn by the shared item
  drawing in `ui/tabs.rs`.
- `app/tab.rs`: the tab's row says it is searchable, that it is read every 2 seconds while it is
  shown, from the `Domain::Network` domain, and that its label column is 20 cells wide.

**Refresh.** Only the visible tab is read (`docs/specs/2026-10-02-alpheus-v1-design.md`, § 3:
nothing runs in the background and nothing runs when the window is closed). `App::on_tick` asks
the worker for `Domain::Network` once the tab's period has passed; a failing read is tried again
at most every 15 seconds (a broken tool must not be relaunched every second: a comment in
`App::on_tick`).

**The path of one change**, turning Wi-Fi off:

1. Space, Enter or a click reaches `App::toggle` with `ToggleId::Wifi`.
2. `toggle` flips `wifi_enabled` in the loaded data at once, then calls `send_op` with
   `Op::SetWifi(false)`. `send_op` notes that the network domain has a change in flight and leaves
   a `Request::Do` in the box the real loop empties.
3. The worker thread runs `nmcli radio wifi off` (`NmCli::set_wifi`), through the injectable
   command executor, and answers with `Update::Done`.
4. Until then a read of that domain that arrives is dropped (`App::on_update`). On the answer a
   message says `Wi-Fi: off` and the data is read again, so what is shown is what `nmcli` now says.

### Rules it keeps

- `items_per_tab` (`app/tests.rs`): the Network tab's items, the switch then the button.
- `optimistic_toggle`, `stale_read_ignored_during_a_toggle`, `two_toggles_wait_for_both_responses`
  (`app/tests.rs`): the switch shows the new value at once and a stale read does not undo it.
- `tick_refreshes_sound_network_bluetooth_every_two_seconds` (`app/tests.rs`): a read after 2
  seconds, none before, and the period restarts from the read.
- `network_bluetooth_power_tabs` (`ui/tests.rs`): the heading, a connection and the button are on
  screen.
- `headings_take_the_tab_hue` (`ui/tests.rs`): the heading is drawn in the tab's cyan.
- `the_network_tab_at_80_by_24_matches_its_snapshot` (`ui/network/tests.rs`, its snapshot in
  `ui/network/snapshots/`): the whole 80 by 24 screen.
- `exact_commands` (`alpheus-common`, `backends/network.rs`): the exact `nmcli` lines, and no
  other.
- `every_launched_command_is_on_the_allowlist` (`tests/guardrails.rs`): every command the backends
  run is on the allowed list, `nmcli` included. That no test runs the real tool is CLAUDE.md's rule
  ("Never a real call in tests": the tests use the fake executor).

### What it costs

The control center is not measured tab by tab. What the specs fix for it as a whole: the first
render in under 100 milliseconds, and nothing read while a tab is not shown. This tab's reads are
two `nmcli` commands every 2 seconds while it is shown (the Wi-Fi radio, and the active
connections), and nothing otherwise. (Reason for the 2 second period: not recorded.)

## Bluetooth

### What it does

The Bluetooth tab (named **BT** in the tab bar) has the **Bluetooth** switch, the list of
**connected devices**, and a **Pair a device** button at the end. Each device line gives its name
and its address. While the data is loading the list shows `…`; if the read failed it shows its
error; with the adapter off it shows `Bluetooth is off`; with nothing connected it shows `none`.

```
 ▶ Bluetooth                        [■] on

 Connected devices
   CasqueBT  (AA:BB:CC:DD:EE:01)

   [ Pair a device ]
```

(The classic layout, in a window narrower than 84 columns or shorter than 26 rows. From that size the tab is
drawn with the rail: a purple hue for its heading, the switch as an on/off switch, a glyph before
each device, and the button with a glyph. The items and their order are the same.)

### Using it

| Key (default) | What it does |
| --- | --- |
| ↑ ↓ | move to the previous or next item |
| Space or Enter | on the Bluetooth switch: turn the adapter on or off |
| Enter | on the button: open the pairing tool |
| `/` or Ctrl+F | search: only the devices whose name matches the text are listed (a fuzzy match) |
| Esc | clear the search; with an empty search, go back to Home |
| F5 | read the Bluetooth data again |

With the mouse: a click on the switch's row turns the adapter on or off; a click on the button
opens the tool. The search narrows the list of devices only; the two items stay.

The switch behaves as the Wi-Fi switch of the Network tab: it changes at once, a stale read does
not undo it, and with the state unknown it does nothing and a message says
`Bluetooth: unknown state`.

The **Pair a device** button opens the program set under `[commands] bluetooth` in `config.toml`
(default `bluetui`) in alpheus's own window, and reads the tab again when it ends. If the tool
cannot start, the status line and a message say `Pair a device: <why>`.

### Configuration

The tab has one setting of its own, in `config.toml`, read once when the control center starts:

| Key | Default | Meaning |
| --- | --- | --- |
| `[commands] bluetooth` | `["bluetui"]` | the program and arguments the Pair a device button runs |

The keys come from `[keys]` and `[keys.bindings]` (the names are `toggle`, `activate`, `search`,
`refresh`; see the README's list of action names).

### How it is built

Two files, one per layer, and the tab's row in `app/tab.rs`.

- `app/bluetooth.rs`: `items` (the Bluetooth switch, then the button). The Bluetooth data
  (`App::data.bluetooth`, a `BtStatus`), the switch's change (`App::toggle`,
  `ToggleId::Bluetooth`) and the opening of the external tool (`Command::Open`,
  `External::Bluetooth`) are in `app/mod.rs` and `app/action.rs`: Home has the same switch, so
  they are shared.
- `ui/bluetooth.rs`: `rows` (the switch, the heading, the device lines, the button) and `hints`
  (the keys named in the status line). The switch and the button are drawn by the shared item
  drawing in `ui/tabs.rs`.
- `app/tab.rs`: the tab's row says it is searchable, that it is read every 2 seconds while it is
  shown, from the `Domain::Bluetooth` domain, and that its label column is 20 cells wide.

**Refresh.** As for the Network tab: only the visible tab is read, `App::on_tick` asks the worker
for `Domain::Bluetooth` once the 2 seconds have passed, and a failing read is tried again at most
every 15 seconds.

**The path of one change**, turning Bluetooth off, is the Network tab's with `ToggleId::Bluetooth`
and `Op::SetBluetooth(false)`; the worker runs `bluetoothctl power off`
(`BluetoothCtl::set_power`), and the message on the answer is `Bluetooth: off`.

### Rules it keeps

- The Bluetooth tab's items have no test of their own: `items_per_tab` (`app/tests.rs`) checks the
  Network tab's items. `network_bluetooth_power_tabs` (`ui/tests.rs`) checks that the heading, a
  device and the button are drawn.
- `optimistic_toggle`, `stale_read_ignored_during_a_toggle`, `two_toggles_wait_for_both_responses`
  (`app/tests.rs`): the switch rule, tested on the Wi-Fi switch, the same code.
- `tick_refreshes_sound_network_bluetooth_every_two_seconds` (`app/tests.rs`): a read after 2
  seconds, none before.
- `the_bluetooth_tab_at_80_by_24_matches_its_snapshot` (`ui/bluetooth/tests.rs`, its snapshot in
  `ui/bluetooth/snapshots/`): the whole 80 by 24 screen.
- `powered_off_does_not_list_devices` and `exact_commands` (`alpheus-common`,
  `backends/bluetooth.rs`): an adapter that is off lists no device, and the exact `bluetoothctl`
  lines.
- `every_launched_command_is_on_the_allowlist` (`tests/guardrails.rs`): every command the backends
  run is on the allowed list, `bluetoothctl` included.

### What it costs

The same as the Network tab: nothing read while the tab is not shown, and while it is shown one
`bluetoothctl show` every 2 seconds, plus one `bluetoothctl devices Connected` when the adapter is
on. (Reason for the 2 second period: not recorded.)

## Power

### What it does

The Power tab is five buttons: **Lock**, **Log out**, **Suspend**, **Reboot** and **Power off**.
It shows nothing else and reads nothing. Log out, Reboot and Power off ask first: a box
**Confirmation** with the question (`Log out of the session?`, `Restart the computer?`,
`Shut down the computer?`) and two buttons, **Cancel** and **Confirm**. Lock and Suspend run at
once.

```
 ▶ [ Lock ]

   [ Log out ]

   [ Suspend ]

   [ Reboot ]

   [ Power off ]
```

(The classic layout, in a window narrower than 84 columns or shorter than 26 rows. From that size the tab is
drawn with the rail: a red hue, and a glyph and an arrow on each button. The items and their
order are the same.)

### Using it

| Key (default) | What it does |
| --- | --- |
| ↑ ↓ | move to the previous or next button |
| Enter | run the focused button's action, after the question for the three that ask |
| ← → or Tab | in the question: move between Cancel and Confirm |
| Enter | in the question: do what the focused button says |
| Esc | in the question: cancel; with no question open, go back to Home |

There is no search on this tab. With the mouse: a click on a button is Enter on it; in the
question, a click on Cancel or Confirm answers it.

In the question the focus starts on **Cancel**, so that an Enter pressed by reflex runs nothing (a
comment on `Confirm::yes_focused`). The spec asks for the confirmation on power off, reboot and
log out (`docs/specs/2026-10-02-alpheus-v1-design.md`, § 2); the code says the rule is "whatever
closes the session or the machine" (`PowerAction::needs_confirm`). Why Suspend does not ask: reason
not recorded.

### Configuration

None: the commands are fixed in the backend and the tab reads no setting of its own. The keys come
from `[keys]` and `[keys.bindings]` (the names are `activate`, `left`, `right`, `back`; see the
README's list of action names).

What each button runs (`SessionPower::perform`, through the injectable command executor):

| Button | Command |
| --- | --- |
| Lock | `hyprlock`, started and left to run on its own |
| Log out | `wm-set exit` |
| Suspend | `systemctl suspend` |
| Reboot | `systemctl reboot` |
| Power off | `systemctl poweroff` |

### How it is built

Two small files and the tab's row in `app/tab.rs`.

- `app/power.rs`: `items` (one button per `PowerAction`, in the order of `PowerAction::ALL`). The
  rest is shared, in `app/mod.rs`: `App::run_command` opens the question (`Overlay::Confirm`) for
  an action that needs it, and sends `Op::Power` straight away for the others;
  `App::close_confirm` sends `Op::Power` once the question is answered Confirm. The question is
  drawn by `ui/overlays.rs`.
- `ui/power.rs`: `hints` only (the one key named in the status line). The rows are not here: Home
  and Power list one row per item with a blank row between them, and that one arm is in
  `ui/tabs.rs` (`Tab::Home | Tab::Power`).
- `app/tab.rs`: the tab's row says it is not searchable, that it is never read (no domain, no
  period), and that its label column is 20 cells wide.

**The path of one action**, Power off:

1. Enter, or a click, reaches `App::activate`, which sees a `Command::Power` button and calls
   `run_command`.
2. `run_command` sees that Power off needs a confirmation and opens `Overlay::Confirm` with
   the focus on Cancel. Nothing is sent.
3. Confirm, by Enter with the focus moved to it or by a click, reaches `close_confirm`, which
   sends `Op::Power(Poweroff)`.
4. The worker runs `systemctl poweroff` and answers with `Update::Done`.

### Rules it keeps

- `items_per_tab` (`app/tests.rs`): five buttons on the Power tab.
- `power_with_or_without_confirmation` (`app/tests.rs`): Power off opens the question and queues
  nothing; Lock queues `Op::Power` at once.
- `confirmation_from_the_keyboard`, `arrow_keys_in_the_confirmation` (`app/input/tests.rs`) and
  `confirmation_with_the_mouse` (`app/mouse.rs`): the question starts on Cancel, and each way of
  answering it.
- `tick_reads_nothing_on_power` and `changing_tab` (`app/tests.rs`): nothing is read on this tab,
  not even after a minute.
- `confirmation_for_logout_reboot_and_poweroff` and `exact_commands` (`alpheus-common`,
  `backends/power.rs`): the three actions that ask, and the exact commands.
- `power_buttons_with_glyphs` (`ui/tests.rs`): each button has its glyph, and the focused one is
  filled with the tab's red.
- `the_power_tab_at_80_by_24_matches_its_snapshot` (`ui/power/tests.rs`, its snapshot in
  `ui/power/snapshots/`): the whole 80 by 24 screen.
- `every_launched_command_is_on_the_allowlist` (`tests/guardrails.rs`): every command the backends
  run is on the allowed list, and none names a display power-off.

### What it costs

Nothing while the tab is shown or not: it reads no data. The first-render budget for the control
center as a whole is under 100 milliseconds (`docs/specs/2026-10-02-alpheus-v1-design.md`).

## Home

### What it does

Home is the first tab and the everyday one: two sliders (**Volume** and **Brightness**), four
switches (**Wi-Fi**, **Bluetooth**, **Do not disturb**, **Keep awake**) and the **Energy profile**
(the power profiles the system offers, one of them active). It reads these seven things and nothing
else, apart from the list of active connections and the connected Bluetooth devices, which the
dashboard shows.

In a window where the tab's body is at least 64 columns by 20 rows (the 87 by 26 window of the
Super+A launcher leaves a 67 by 20 body beside the rail), Home is a **dashboard**: a clock in large
block digits with the date, a greeting (`Good morning` from 5 to 11 o'clock, `Good afternoon` from 12
to 17, `Good evening` otherwise) and a one-line summary (`2 connections · 1 device · Balanced`); a card
for the volume, one for the brightness and one for the energy profile; four tiles for the switches,
filled with their colour when on and showing what is connected (the Wi-Fi network, the first
Bluetooth device); and the two lists, **Connections** and **Devices**. The cards and tiles take the
shape of the theme's `style` (round caps under `islands`, arrow tips under `powerline`, no fill under
`underline`, double frames under `floating`). In a smaller body the tab is plain rows (the classic
layout):

```
 ▶ Volume              ███████████████░░░░░░░░░  62 %

   Brightness          ████████████░░░░░░░░░░░░  50 %

   Wi-Fi               [■] on

   Bluetooth           [■] on

   Do not disturb      [ ] off

   Keep awake          [ ] off

   Energy profile       Balanced   Performance   Power saver
```

(What the rows say is the same on the dashboard: the same seven items in the same order, so the
keys, the palette and the mouse reach the same things.)

### Using it

| Key (default) | What it does |
| --- | --- |
| ↑ ↓ | move to the previous or next of the seven items |
| ← → or `-` `+` | on a slider: lower or raise it by 5 points; on the energy profile: the previous or next profile |
| Space | on a switch: turn it on or off |

There is no search on this tab. With the mouse: a click on a card or a tile selects it, and on a
tile switches it; a click or a drag on a gauge sets the value; a click on a profile chooses it.

The volume and brightness sliders follow the rules the Sound tab states (never above 100 % from here;
the brightness never goes below its minimum percentage, `brightness::MIN_PERCENT`).

### Configuration

None of its own. The keys come from `[keys]` and `[keys.bindings]` (the names are `toggle`,
`increase`, `decrease`, `left`, `right`). The dashboard's colours and shapes come from the theme
(`theme.toml`, the Theme tab below).

### How it is built

Two files and the tab's row in `app/tab.rs`.

- `app/home.rs`: `items`, the seven focusable items in the order they are drawn. What they do is
  shared, in `app/mod.rs`: `App::adjust` (sliders and profile), `App::toggle` (the four switches),
  `Command::SetProfile`. Their data is in `App::data` (volume, brightness, Wi-Fi, Bluetooth, Dnd,
  Keep awake, profiles, and the two lists).
- `ui/home.rs`: the dashboard (`render`, and one function per card, tile and list), the clock
  (`LocalTime`, passed in by `ui::render_at` so a test can draw at any time) and the shortcut hints.
  The rows used below 64 by 20 are the shared arm `Tab::Home | Tab::Power` of `ui/tabs.rs`, which
  stays there because Power draws its rows with the same code (see the Power section); the items are
  drawn by the shared item drawing in the same file.
- `app/tab.rs`: the tab's row says it is not searchable, that it is read every second while it is
  shown (the Volume, Brightness, Dnd, KeepAwake, Profiles, Network and Bluetooth domains) and that its
  label column is 20 cells wide.

**The dashboard's seven positions.** `ui/home.rs` names the items by position (`VOLUME` is 0, `WIFI`
is 2, and so on) and a test (`home_item_indices_match_the_app`) fails if `app/home.rs` reorders them.

**The layout.** `bands` gives the heights: at 20 rows a 4-row clock, 7-row cards, 4-row tiles and what
is left for the lists; every row more goes, in a fixed order, to the cards, a gap, the tiles, the
clock. The two lists are drawn only if four rows are left for them. The clock tries three
arrangements in turn (two-cell digits with the date beside them, one-cell digits beside them, two-cell
digits with the date stacked) and takes the first that fits.

### Rules it keeps

- `items_per_tab` (`app/tests.rs`): seven items on Home.
- `home_item_indices_match_the_app` (`ui/home/tests.rs`): the positions the dashboard names are the
  app's.
- `the_dashboard_needs_a_64x20_body` and `small_bodies_keep_the_rows` (`ui/home/tests.rs`): the
  thresholds, and the rows below them.
- `dashboard_hits_reach_today_s_actions` (`ui/home/tests.rs`): a click on a card or a tile reaches
  the same action as the rows.
- `mae_window_87x26_shows_the_whole_dashboard` and `a_clock_strip_that_just_fits_gets_the_two_cell_digits`
  (`ui/home/tests.rs`): the dashboard whole at 87 by 26, and the clock's choice at its edge.
- `the_dashboards_hints_follow_the_keymap` (`ui/home/tests.rs`): no key is written into a hint.
- `the_dashboard_at_120_by_36_matches_its_snapshot` and `the_home_tab_at_80_by_24_matches_its_snapshot`
  (`ui/home/tests.rs`, their snapshots in `ui/home/snapshots/`).

### What it costs

The first render of the dashboard is checked to take under 100 milliseconds even when the tools are
slow (`first_render_of_the_dashboard_under_100_ms_even_with_slow_tools`, `tests/first_render.rs`;
README: 218–300 µs). Home is read once a second while it is shown and never otherwise (the period:
the spec's, § 3).

## Displays

### What it does

The Displays tab shows the screens and changes them: a **profile bar** (the saved profiles, then
`+ Save current`), a **map** of the enabled screens drawn to scale (each with its number and short
name; `●` marks the one focused in the list), and **one row per output** with its number and short
name, its connector (`⚠` for a DisplayLink output), a switch (`[■] on`, `[■] only on` for the last
active screen, `[ ] off`), its mode, its scale and where it is placed (`origin`, `right of Alpha`,
`below Alpha, centered`, `position 0, 1080`).

Every change is a **trial**: the change is applied and a box, **Keep these settings?**, shows what
changed and `Automatic rollback in N s` (15 seconds). The focus starts on **Cancel**. Keep makes the
change permanent; Cancel, the end of the countdown, or closing alpheus puts the previous settings
back. The tab is locked while a trial or another change is in progress, and after one until a fresh
read has come: a change is never planned on screens that may have been rolled back.

```
 Profiles   [ Laptop only ]  [ Desktop ]  [ + Save current ]

 Map
   ┌───Acme───┐┌──Globex──┐
   │    3     ││    2   ● │
   └──────────┘└──────────┘

 Displays
 ▶ 1 Laptop   eDP-1       [■] on   2560×1600@60   scale 1.0   below Acme
   2 Acme     DVI-I-1 ⚠   [■] on   1920×1080@60   scale 1.0   origin
```

(The classic layout. From 84 columns by 26 rows the tab is drawn with the rail, in green; at 80
columns the scale column is dropped, and when the placement text still does not fit the connector
too: the placement is what the tab is for, a comment in `ui/displays.rs`.)

The settings panel of an output has the fields **Mode**, **Scale**, **Rotation**, **Placement** and,
for a placement relative to another screen, **Reference** and **Alignment**, then **Cancel** and
**Apply**. If the geometry refuses the draft, Apply is greyed out and the reason is shown.

### Using it

| Key (default) | What it does |
| --- | --- |
| ↑ ↓ | move over the profile bar and the outputs |
| ← → | on the profile bar: choose a profile (or `+ Save current`) |
| Enter | on the profile bar: apply the profile, or open the name entry for `+ Save current`; on an output: open its settings panel |
| Space | on an output: switch it on or off (never the last active one) |
| ↑ ↓, ← → or `-` `+` | in the panel: the previous or next field; the value of the field |
| `/` | in the panel, on Mode: search a mode (a substring, so `1280` does not find `1920x1080`) |
| Enter | in the panel: on Apply, apply; on Cancel, close; on another field, move to Apply |
| ← → or Tab, Enter, Esc | in the Keep box: choose Cancel or Keep; answer; cancel |

With the mouse: a click on a chip chooses and applies a profile, on a switch changes the output, on
a row opens its panel; in the panel a click on a field selects it, on `◀` `▶` steps its value; in the
box a click on Cancel or Keep answers it. The command palette (Ctrl+P) lists `Display profile: <name>`
for each profile and `Go to: Displays`; a profile chosen there from another tab is applied once the
tab has a fresh read, and dropped after 10 seconds (`Displays: profile not applied (busy)`) if it
never gets one.

A change that touches a DisplayLink output adds `⚠ DisplayLink output: known freeze risk on this
machine` to the box. A profile whose screens are not all connected still applies: the missing ones are
listed as `<name> missing, ignored`.

### Configuration

Profiles are in `displays.toml` (in the config directory), written by `+ Save current` and applied by
the daemon on hotplug when `[displays] auto = true` in `bar.toml` (the default; see the README). A
`displays.toml` that could not be read at startup is never overwritten: saving says
`invalid displays.toml: fix it before saving`. The keys come from `[keys]` and `[keys.bindings]`.

### How it is built

The daemon, not this tab, owns the screens: it speaks the Wayland output protocol, applies a plan,
holds the 15-second trial and rolls it back by itself (`docs/specs/2026-10-30-alpheus-displays-protocol-design.md`).
The tab sends the daemon's socket verbs through the worker (`Op::TryDisplays`, `Op::KeepDisplays`,
`Op::RevertDisplays`, and `Domain::Displays` for the read) and runs no command.

- `app/displays.rs`: the state (`DisplaysTab`, `Trial`, `Panel`, `SaveDialog`), `items` (the profile
  bar, then one item per output), the plans (`enable_plan`, `App::panel_preview`, `kept_in_place`, the
  geometry from `crate::displays::geometry`) and every answer: the daemon's snapshot
  (`displays_read`), its trial (`trial_read`, `trial_gone`), the worker's answer (`displays_done`),
  the countdown (`displays_tick`), Keep and Cancel (`keep_answer`).
- `ui/displays.rs`: `rows`, `hints`, the map (`map_canvas`), `draw_profiles`, `draw_output` and the
  three boxes (`keep`, `panel`, `save`), which `ui/overlays.rs` calls. The item rows are drawn by the
  shared item drawing in `ui/tabs.rs`.
- `app/tab.rs`: the tab's row says it is not searchable, that it is read every 2 seconds while
  shown, from `Domain::Displays`, and that its label column is 20 cells wide.

**The path of one change**, switching an output off:

1. Space reaches `toggle_output`, which refuses if a change is in progress or the output is the last
   active one, and otherwise builds a `Plan` with one step.
2. `start_trial` keeps the plan as the `Trial`, opens the Keep box and sends `Op::TryDisplays`.
3. The worker sends the plan to the daemon; its answer (`displays_done`) starts the countdown from
   this tab's own clock. The daemon's `seconds left` in each 2-second read only brings it forward.
4. Keep (`keep_answer(true)`) sends `Op::KeepDisplays`; Cancel or the end of the countdown sends
   `Op::RevertDisplays`. Either way no further change is planned until a read with no trial has come.

### Rules it keeps

- `locked_until_a_fresh_read`, `locked_while_the_daemon_is_busy` and `another_trial_open_locks_the_tab`
  (`app/displays/tests.rs`): no change is planned on a stale read.
- `profile_applied_with_countdown`, `automatic_rollback_after_fifteen_seconds`, `the_countdown_follows_the_daemon`
  and `cancel_reverts_at_once_and_keep_leaves_nothing_to_revert`: the trial and its countdown.
- `the_last_active_screen_cannot_be_switched_off`, `a_trial_that_touches_a_displaylink_output_says_so`
  and `saving_refused_if_the_file_is_invalid`: the three protections.
- `layout_off_origin_panel_with_no_change`, `never_a_negative_coordinate_after_anchoring` and
  `scaling_the_origin_neighbors_follow`: the placement rules.
- `the_rightmost_screen_skips_the_adjusted_one_and_the_last_of_equal_right_edges_wins` and
  `the_reference_field_wraps_round_the_other_screens`: where an enabled screen goes, and the reference
  field's wrapping.
- `an_output_row_gives_up_its_scale_then_its_connector_as_the_placement_needs_room`,
  `a_reference_screen_that_is_not_connected_shows_the_brand_from_its_description` and
  `the_displays_tab_at_80_by_24_matches_its_snapshot` (`ui/displays/tests.rs`, its snapshot in
  `ui/displays/snapshots/`); the rest of the tab's drawing in `ui/tests.rs` (`displays_tab_rendering`,
  `displays_fit_mae_window_87x26`, `displays_snapshots`).

### What it costs

One read of the daemon's model, answered from its memory, every 2 seconds while the tab is shown, and
nothing otherwise (`docs/specs/2026-10-30-alpheus-displays-protocol-design.md`). The daemon's own
costs (the trial's one timer, the hotplug debounce) are in the README's component table.
