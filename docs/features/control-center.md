# The control center

`alpheus` is the terminal control center: a ratatui program with eight tabs (Home, Sound, Network,
Bluetooth (BT), Power, Displays, Theme, Diag). This page is written one tab at a time, in the order the
tabs are brought to the house standard (`docs/STYLE.md`); today it holds the Sound, Network,
Bluetooth and Power tabs. The tabs that are not described here yet are described in the README's
control center section.

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
