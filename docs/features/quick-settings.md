# The quick settings panel

A panel opened from the bar for everyday settings, without opening the control center: the volume
and the output, the brightness, the microphone, do-not-disturb, the power profile, two toggles you
configure, the Wi-Fi, Bluetooth and VPN lists, Shield mode and the session buttons. This page says
what it does, how to use it, how to configure it, how it is built, which rules the tests enforce on
it, and what it costs. It describes the code as it is today.

## What it does

The panel is an `xdg_popup` on the bar's own layer surface. It is built each time it opens and
dropped when it closes: nothing of it stays in memory but one thing (a Wi-Fi attempt on a new network
that outlives it, below), and nothing wakes while it is closed. Every
write it makes is a command run through the daemon's executor (never through a shell, never waited
for); the D-Bus connection that feeds it is read-only, except for the Bluetooth detail's own acting
connection (see Bluetooth below).

A row whose tool, hardware or command is missing is not shown.

- **Volume.** The default sink's level as a bar and a percentage, `›` to open the outputs detail
  (every sink by label, `●` on the default one; Enter makes one the default with
  `wpctl set-default`). The panel never raises the volume above 100 %.
- **Brightness.** Read from `[osd] backlight` in `[osd] backlight_dir`; set with `brightnessctl`,
  re-read 150 ms after, and never lower than 1 % from the panel. Hidden without a backlight device.
- **Microphone.** `Microphone`, muted or live. Hidden without a default source.
- **Mixer.** `Mixer…` opens a terminal mixer (`[quick] mixer`).
- **Do not disturb.** Space toggles it. Enter or → opens a detail with six choices (`Off`,
  `30 minutes`, `1 hour`, `2 hours`, `Until <dnd_until>`, `Until I turn it off`); a timed one reads
  `until HH:MM` on the row. Shown only with the `notifications` module.
- **Power profile.** The active profile of `net.hadess.PowerProfiles`, read only; Enter or a click
  cycles to the next one offered with `powerprofilesctl set`. Hidden when the service has no owner.
- **Keep awake and night light.** Rows for the toggles you configure (`[quick] keep_awake`,
  `night_light`), with their state read from the `state` you give. Hidden unless configured.
- **Wi-Fi, Bluetooth, VPN.** One row each, with a detail list behind it (below).
- **Shield.** A row and a detail of the blocked USB devices, when `[shield]` is configured (see
  [shield.md](shield.md)).
- **Session buttons.** Lock, Log out, Reboot and Power off, each a command from `[quick]`; a button
  whose command is empty is not drawn. Reboot and power off ask first.

### Wi-Fi

A Wi-Fi row first under the sliders: a glyph by signal, the SSID, on or off; Space runs
`nmcli radio wifi on|off`. Its detail (Enter, →, a click, a click on the network module, or
`alpheus-launch --quick wifi`, which also asks for one `nmcli device wifi rescan`) lists the
networks, at most 32: the connected one, then saved ones, then the others by signal, with `●`, four
signal steps, a lock and `saved`. `More…` runs `[quick] wifi_more`.

Enter on a network connects: a connected one goes down (`connection down uuid`), a saved one comes up
(`connection up uuid`), an open one connects (`device wifi connect`). A new WPA-personal network asks
for its password in a `Password` row, and `nmcli --ask device wifi connect` reads it on its standard
input, never from an argument. While connecting a row reads `…`, then `●`; a failure reads `failed`
for 4 s and writes one log line without the SSID. An attempt on a new network outlives the panel, so
that the profile that appeared during a failed attempt is deleted if it is still saved.

### Bluetooth

A Bluetooth row under the Wi-Fi row: the first connected device, else `on`, `off` or `blocked`
(rfkill). Space powers the adapter; on a blocked radio it runs `rfkill unblock bluetooth` first. The
row is hidden without BlueZ or an adapter. A left click on the `bluetooth` module, or
`alpheus-launch --quick bluetooth`, opens the detail: the paired devices (● connected), then
`Nearby` and the named devices found while it is open, at most 32 rows. Enter disconnects a
connected device, connects a paired one, or pairs a nearby one; Delete or a right click asks
`Forget? ⏎`. Discovery runs only while the detail is open. alpheus answers a pairing code through its
own BlueZ agent: a code to confirm shows as `Code 482 913? ⏎`, a code to type shows as
`Type 482 913 then ⏎`; anything else is rejected. With a Bluetooth headset as the default sink, a
`‹ Hi-Fi ›` / `‹ Mic ›` row between the volume and the brightness switches its profile with
`wpctl set-profile`.

### VPN

A VPN row under the Bluetooth row: the active profile's name, else `off`; hidden when NetworkManager
has no VPN or WireGuard profile. Space takes the active profile down, else brings the last used one
up. Its detail (Enter, →, a click, or `alpheus-launch --quick vpn`) lists at most 16 profiles, the
active ones first with `●`; Enter brings one up (`nmcli connection up uuid`) or down. While
activating a row reads `…`; a failure reads `failed` for 4 s with one log line that names no profile.
alpheus never handles a VPN secret: a profile that needs one is answered by NetworkManager's stored
secrets or the desktop's agent, else it fails, and `More…` (`[quick] vpn_more`) is the way.

## Using it

**Opening.** A left click on the volume, network or Bluetooth module opens the panel under it (the
right click opens the control center's tab). `alpheus-launch --quick [sound|wifi|bluetooth|vpn]`
opens it under the bar's right end on the focused output, on that section; the same verb again closes
it. Only one bar popup is open at a time. Without `quick` in `modules`, the verb exits 0 and opens
nothing.

**Keys.** ↑ ↓ and Home End move between rows (Ctrl+P and Ctrl+N too); ← → change a slider by
`[quick] step` percent, or move between session buttons; Space and Enter toggle a row, Enter on the
volume opens the outputs; Esc leaves a detail, else closes the panel. Only the arrow keys repeat when
held. The wheel over a slider changes it; elsewhere it moves the window over the rows when they do
not fit.

**Mouse.** A left press acts on its row; a press on a slider's bar sets the level of the cell; a
right click on the volume toggles mute.

**Reboot and power off** turn their button into `Reboot? ⏎` for 3 s. A second activation runs the
command, but only from 0.4 s after the question, so a double click never confirms. Any other key,
another row or the 3 s cancel it. Lock and Log out run at once. Every session button closes the
panel once its command is spawned.

**What closes it.** Esc, the verb again, a click outside, a restyle that moves its bar, or the output
going away.

**With the verb, no grab.** A panel opened by the verb takes the keyboard without a pointer grab: the
bar goes `Exclusive` while it is open. A click on a window does not close it. On Hyprland older than
0.56.2 a click inside it may close it. A verb arriving within 250 ms of the verb's own open is
ignored (`quick: toggle debounced`), because Hyprland fires a press bind again.

## Configuration

`[quick]` in `bar.toml`. Add `quick` to `modules` (it is in the default list). The panel also reads
`[osd] backlight` and `backlight_dir` for the brightness row.

| Key | Default | Meaning |
| --- | --- | --- |
| `step` | `5` | percent per ← → or wheel notch on a slider, 1 to 25 |
| `keep_awake` | none | `{ toggle = [..], state = ".." }`: the command and where its state is read; the row shows only with a `toggle` |
| `night_light` | none | the same, for night light |
| `lock` | `["loginctl", "lock-session"]` | the Lock button's command; empty hides the button |
| `logout` | `[]` | the Log out button's command (empty by default, so the button is hidden) |
| `reboot` | `["systemctl", "reboot"]` | the Reboot button's command |
| `poweroff` | `["systemctl", "poweroff"]` | the Power off button's command |
| `wifi_more` | launcher.toml's terminal and `nmtui` | the Wi-Fi detail's `More…`; `[]` hides the row |
| `vpn_more` | launcher.toml's terminal and `nmtui-edit` | the VPN detail's `More…`; `[]` hides the row |
| `mixer` | launcher.toml's terminal and `wiremix` | the `Mixer…` row; `[]` hides it |
| `dnd_until` | `"08:00"` | the time of the do-not-disturb detail's `Until` entry, `HH:MM` local |

A toggle's `state` is `""` (unknown: the row shows `·`), `"file:<path>"` (on while the file exists;
the path starts with `/`, `~/` or `$XDG_RUNTIME_DIR/`) or `"process:<name>"` (on while a process
with that `comm` runs, at most 15 bytes). It is read when the panel opens and 300 ms after the toggle
ran. A `state` without a `toggle` is refused. Every command is run as given, never through a shell;
a blank program or an argument holding a NUL character is refused when the file is read, with a
message naming the key. The lock, reboot and power off commands handle no sleep or screen-off.

```toml
[quick]
step = 5
keep_awake = { toggle = ["my-keep-awake"], state = "file:$XDG_RUNTIME_DIR/keep-awake" }
night_light = { toggle = ["my-night-light"], state = "process:hyprsunset" }
logout = ["loginctl", "terminate-session", "self"]
dnd_until = "07:30"
```

## How it is built

The files are in `crates/alpheus-daemon/src/quick/`, split the way alpheus splits most features: a
pure model, pure details, and the wiring that touches the outside world.

- `mod.rs`: the model. `Quick` is the open panel's state, `Live` borrows what the daemon knows,
  `QuickAction` is what a key or a click asks of the wiring, and `ROWS` says which rows exist.
  It reads no file and no clock and starts nothing.
- `draw.rs`: the model's state turned into cells (rows, sliders, switches, session buttons).
- `app.rs`: the wiring (`QuickApp` on the `App`, and the `quick_*` methods): opening, the reads,
  spawning commands, the timer, and the Wi-Fi attempt that outlives the panel.
- `state.rs`: a configured toggle's state (`file:` or `process:`), one bounded read.
- `secret.rs`: the Wi-Fi password's buffer.
- `dnd.rs` and `shield.rs`: the do-not-disturb detail and the Shield detail, each its own struct
  with its own state, rows, keys and tags.
- `wifi.rs`, `bt.rs`, `vpn.rs` and `bt_app.rs`: the Wi-Fi, Bluetooth and VPN lists. Their reads are
  on the D-Bus thread (`sources/dbus/wifi.rs`, `bt.rs`, `vpn.rs`); Bluetooth's calls go through its
  own connection (`sources/dbus/bt_act.rs`).
- `session.rs` (outside this folder): the session entries and the confirmation rule, shared with the
  power menu.
- The tests sit next to each file (`tests.rs`, `app/tests.rs`, `dnd/tests.rs`, `shield/tests.rs`,
  and so on).

**The one timer.** The panel owns a single `DeadlineTimer`, armed only while the panel is open and
something is pending: a slider's 150 ms window, a re-read after a command, the reboot question's 3 s,
a Wi-Fi, VPN or Bluetooth deadline or a `failed` tag's end. Closed, it is armed only for a Wi-Fi
attempt still followed.

**The sink list.** The sound thread keeps the pw-dump model behind a `SoundTap`; the panel reads the
sinks from it when it opens and asks the thread to publish them while it is open (`Scope::Panel`).
Closed, the thread goes back to its own scope.

**The reads on the D-Bus thread.** The Wi-Fi, Bluetooth and VPN states are read only while the panel
is open, through a tap per list (a scope Closed, Row or Detail, and a socket pair the D-Bus thread
polls beside the bus). A state read for an earlier open is dropped.

**The path of a click, from the bar to a command.**

1. The bar's click handler finds which module was clicked. A left click on volume, network or
   Bluetooth calls the popup table's `toggle` for the quick panel, which is `OPS` in `quick/app.rs`,
   so `quick_toggle_at` and then `quick_toggle` open the popup.
2. Inside the popup, a key reaches `quick_key` and a pointer event `quick_pointer`, which hand it to
   the model through `quick_with` and get a `QuickAction` back.
3. A verb arrives on the launcher's socket, and `launcher/socket_glue.rs` routes it to `quick_verb`,
   which calls `quick_toggle` too.
4. `quick_action` is the one place an action is carried out: `Run` spawns the command through
   `spawn_logged`, `RunSecret` hands the password to `quick_spawn_secret`, and `Redraw` repaints.

The "why" behind several of these choices is in [decisions.md](../decisions.md): the entries from
"The quick panel keeps its selection on a row, not on a position" to "The Shield detail's device
rows are selectable only when `allow` is configured".

## Rules it keeps

These are enforced by tests, so a change that breaks one fails the build.

- `the_quick_model_and_its_state_reader_only_read` (`tests/guardrails.rs`): the model, the cells, the
  state reader and the Wi-Fi, Bluetooth, VPN, do-not-disturb and Shield files write no file, create
  nothing, make no raw system call, and start no command, thread or timer.
- `every_quick_command_stays_on_the_allowlist`: every command the panel can build, from every key on
  every row and every press of every cell, is one of the exact shapes; the configured commands run as
  given.
- `the_quick_panel_has_no_panicking_path`: no `unwrap`, `expect` or `panic!` in the production code
  of the panel's files (`mod.rs`, `draw.rs`, `state.rs`, `app.rs`, `wifi.rs`, `secret.rs`, `bt.rs`,
  `vpn.rs`, `dnd.rs`, `shield.rs`, `session.rs`, `bt_app.rs`).
- `the_wifi_password_goes_to_stdin_only_and_is_wiped_after`: the password is written to `nmcli`'s
  standard input and then the field closes; no file of the panel formats a key or the secret with
  `Debug`; the password buffer has one `unsafe` block, the volatile wipe.
- `the_vpn_logs_name_no_profile`: no log line of the VPN files or the VPN lines of `app.rs` names a
  profile.
- `the_shield_logs_name_nothing`: every log call of the Shield's files, and the Shield's lines in the
  others, holds only fixed words and never names a name, an id, a `vid:pid`, a command, a flag, a path,
  a configuration or a device.
- `the_quick_panel_lets_go_of_the_sound_and_its_timer_whatever_closes_it`: whatever closes the
  panel, the sound thread goes back to its own scope and the panel's timer goes.
- `the_quick_panels_timer_follows_the_view_from_the_open`: the timer is synced as the panel opens, so
  a `failed` shown at open clears at its 4 s.
- `the_bluetooth_connection_goes_with_the_panel` and `the_vpn_tap_follows_the_panel`: the acting
  connection and the VPN tap follow the panel's open and close.

## What it costs

Figures are those recorded when each part merged, in release, back to back with the version before it,
3 runs each, on the headless harness (absolute PSS drifts between boots there: trust the deltas).

- **Linked in, panel not in `modules`:** +64…+196 KB at start when the first delivery merged, and the
  later details each added code: +176…+372 KB (Wi-Fi), +12…+208 KB (Bluetooth), within noise (VPN).
  It is code linked in and unused.
- **The panel itself** (`quick_pss_idle_open_and_closed`): module on and closed, 96–108 KB over the
  bar without it; open, 316–338 KB over rest; once closed, +208…+210 KB over rest (the first open's
  code and font pages, paid once); 20 more cycles −28…−4 KB. The test bounds the daemon's own
  memory, not the shared pages.
- **The details open:** the Wi-Fi detail on 40 access points (32 listed), 898–910 KB over the module
  at rest (`quick_wifi_pss_detail_open_and_closed`); the VPN detail on 10 profiles, 846–854 KB
  (`quick_vpn_pss_detail_open_with_10_profiles_and_after`); the Bluetooth detail on 20 devices,
  +1.1–1.8 MB, of which the daemon's own +436…+540 KB and the shared buffers the rest
  (`bluetooth_detail_pss_with_20_devices_and_after`); the Shield detail on 10 devices, about
  +0.6 MB (`shield_pss_detail_open_and_closed`). The leak bounds for the details read anonymous
  memory (closed within 256 KB of rest, ten cycles within 128 KB of closed): the file-backed share
  moves with the tests run before.
- **The acting Bluetooth connection:** +54…+116 KB while the detail is open
  (`bluetooth_acting_connection_cost`), the headset row +12…+20 KB (`bluetooth_headset_row_cost`).
- **Wake-ups:** none at rest after the panel closes, for the loop and for the D-Bus thread
  (`at_rest_no_wakeups_after_the_quick_panel`, `at_rest_no_wakeups_after_the_wifi_detail`,
  `at_rest_no_wakeups_after_the_vpn_detail`, `bluetooth_at_rest_no_wakeups_after_the_detail`). A
  one-second timer left armed by a close turns them red.
