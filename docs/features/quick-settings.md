# The quick settings panel

A panel opened from the bar for everyday settings, without opening the control center: the volume
and the output, the brightness, the microphone, do-not-disturb, the power profile, two toggles you
configure, the Wi-Fi, Bluetooth and VPN lists, Shield mode and the session buttons. This page says
what it does, how to use it, how to configure it, how it is built, which rules the tests enforce on
it, and what it costs. It describes the code as it is today.

## What it does

The panel is an `xdg_popup` on the bar's own layer surface. It is built each time it opens and
dropped when it closes: nothing of it stays in memory but one thing (a Wi-Fi attempt on a new network
that outlives it, below), and nothing wakes while it is closed except that followed attempt's
deadline. Every outside action is a command run through the daemon's executor (never through a
shell, never waited for), except do-not-disturb, which the daemon applies itself, and the Bluetooth
detail's D-Bus calls, made on its own acting connection (see Bluetooth below). The D-Bus connection
that feeds the panel is read-only.

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
  [shield.md](shield.md)). A left click on the `shield` module opens the panel on the Shield row (it
  needs `quick`).
- **Session buttons.** Lock, Log out, Reboot and Power off, each a command from `[quick]`; a button
  whose command is empty is not drawn. Reboot and power off ask first.

### Wi-Fi

**The row.** A Wi-Fi row first under the sliders: a glyph by the signal of the connected network
(another glyph when the radio is on and nothing is connected, another when it is off), then the SSID
cut to 16 cells (`hidden` for one that shows nothing), `on` or `off`. Space runs
`nmcli radio wifi on|off`. The row is hidden when NetworkManager has no Wi-Fi device.

**The detail.** Enter, →, a click on the row, a click on the `network` module or
`alpheus-launch --quick wifi` opens it, and opening it with the radio on also runs one
`nmcli device wifi rescan`. It lists one row per SSID, at its strongest access point, at most 32:
the connected network, then the saved ones, then the others by signal (then by name). A network whose
SSID shows nothing (zeros, or nothing left once cleaned) is not listed. A row is `●` on the connected
network, the SSID, four signal steps, a lock for a secured one and a tag: `saved`, `…` while an
attempt is under way on it, or `failed` for 4 s after one failed. The signal and the lock sit in one
column on every row. In place of the list the detail can show `Searching…` (the list is not read
yet), `No networks` or `Wi-Fi is off`. `More…` runs `[quick] wifi_more` and closes the panel. ↑ ↓
Home End walk the networks and `More…` without wrapping, Enter acts, Esc or ← go back to the rows; a
left press on a network chooses and enters it, a press on the header goes back.

**Enter on a network** runs the command for its kind:

| The network is | Enter runs |
| --- | --- |
| the connected one | `nmcli connection down uuid <the active connection's uuid>` |
| saved | `nmcli connection up uuid <uuid>`, whatever its SSID |
| open | `nmcli device wifi connect <ssid>` |
| WPA-personal and new | opens a `Password` row under it |
| 802.1X, WEP, other, or an SSID nmcli would misread | moves the selection to `More…` (nothing when `More…` is turned off) |

**The password field.** One dot per character, the end kept when the password is longer than the
row; Backspace erases a character, Ctrl+U the whole password, Space is a character. A character that
would not fit the 256-byte buffer shows `too long` and Enter is refused until Ctrl+U. Enter on an
empty field, after `too long`, or during an attempt does nothing; otherwise it runs
`nmcli --ask device wifi connect <ssid>` and writes the password, then a newline, to its standard
input: never an argument, never a log line, never `Debug`. The field then closes and its buffer is
wiped. Esc closes the field and keeps the detail; moving away, a press anywhere but on the field or
leaving the detail closes it too. A field whose network leaves the list stays 10 s. The password is
the one place a secret is typed; the per-key string SCTK builds for each typed key, and the
`Key::Char` that carries each character to the panel, are not wiped (`secret.rs` says what is and is
not covered).

**An attempt.** One at a time: while one is under way only `More…` acts. It ends `●` when
NetworkManager reports the device activated on that network; it fails when the device state is 120
(failed), when an activation that was under way is left for anything but activated, after 20 s with
no word, or at 60 s when the activation is still under way (a slow network). A failure shows `failed`
for 4 s and writes one log line, `quick: wifi connection failed (<why>)`, with a fixed reason and no
SSID. NetworkManager going away ends the attempt with one log line, `NetworkManager went away`, and
closes the detail; the row hides until it is back. Only the network domain does this: an outage of
BlueZ, UPower or the power profile keeps the detail and its field, and the state it holds is then
marked stale until a fresh read.

**What outlives the panel.** An attempt on a new network (`nmcli device wifi connect`, with or without
a password) is followed after the panel closes, up to 20 s (60 s while the device is still
activating), so that the profile that appeared during a failed attempt is deleted if it is still
saved. Whether the attempt ends in the panel or after it, the profile is never deleted when the
attempt is in doubt: still activating at 60 s, the device activated on what may be the new profile, a
state kept through an outage, a NetworkManager restart, a profile reserved to another user, or a
network-domain outage during the attempt. A wrong-password profile may then remain; remove it through
`More…`. Residual: while the list of saved
networks is unknown (a failed listing, an unreadable profile, more than 64 profiles), Enter on an
already-saved network may create a duplicate profile, which is never deleted.

**Not yet verified.** That `nmcli --ask` reads the password from a pipe, and whether polkit lets
`nmcli` change Wi-Fi from the session without a prompt, are checked at the trial. See the decisions
"An SSID that nmcli would misread is never passed to it", "A failed Wi-Fi attempt deletes its new
profile only when sure", "A row under connection reads `…`", "A Wi-Fi attempt on a new network
outlives the quick panel" and "The Wi-Fi password lives in one boxed buffer that is wiped".

### Bluetooth

**The row.** A Bluetooth row under the Wi-Fi row: the first connected device's name (cut to 16
cells, `?` when nothing is left of it once cleaned), else `on`, `off` or `blocked` (rfkill holds the
adapter off). The glyph is the bar module's own for on and connected, and a Material Design
bluetooth-off glyph for off and blocked. Space powers the adapter the other way; on a blocked radio it runs
`rfkill unblock bluetooth` first, and the adapter is powered on once BlueZ reports it unblocked. The
row is hidden without BlueZ or an adapter.

**The detail.** Enter, →, a click on the row, a left click on the `bluetooth` module or
`alpheus-launch --quick bluetooth` opens it. It lists the paired devices (● on a connected one,
connected first, then by name), then a `Nearby` heading and the devices found while it is open, at
most 32 rows in all (BlueZ's list is read up to 64 devices, names cleaned). Each row is a glyph by the
device's kind (headphones, keyboard, mouse, speaker, phone, a generic one), its name and a tag:
`paired`, `…` while an action runs on it, `failed` for 4 s after one failed. A nearby device is
inserted once, by its signal strength, and never moves; a device with no name of its own, or not heard
advertising during this open (no signal reading), is not listed. Discovery runs only while the detail
is open. The header reads `on` or `off`; with the adapter off the list reads `Bluetooth is off`, and
Space on the header powers it on. ↑ ↓ Home End walk the header and the devices without wrapping, Esc
or ← go back to the rows. A left press on a device selects it and then acts as Enter; a left press on
the header goes back. While a `Code …? ⏎` question shows, a press on a device does nothing, and a left
press on the header goes back and rejects the code.

**Acting on a device.** Enter on a connected device disconnects it (no question), on a paired one
connects it, on a nearby one pairs it, then sets it `Trusted`, then connects it unless the pairing
already did. One action runs at a time: while one shows `…`, Enter on another device does nothing. A
failure, or no answer from BlueZ within 30 s for one call (each call of a sequence has its own),
shows `failed` for 4 s and writes one line, `quick: bluetooth <action> failed (<D-Bus error name>)`
(`(timeout)` after 30 s), never a name, an address or an object path. A device that vanishes during
its action, or an adapter switched off meanwhile, ends the action with no `failed`.

**Forgetting.** Delete, or a right click, on a paired device turns its tag into `Forget? ⏎`; Enter
from 0.4 s to 3 s later removes the device, and any other key, another row or 3 s cancels the
question (the power menu's rule). A nearby device cannot be forgotten.

**Pairing codes.** alpheus registers its own BlueZ agent (`KeyboardDisplay`) and answers only for the
device being paired. A passkey or a PIN to type on the device shows under it as `Type 482 913 then ⏎`
(a PIN as BlueZ gives it, cleaned and cut to 16 characters); a code to compare shows as the device's
tag, `Code 482 913? ⏎`: Enter says yes; any other key, a left press on the header, leaving the detail
or closing the panel rejects it. The code has no timeout; BlueZ's own pairing timeout ends it. A code
to type into alpheus, an authorization and a service are rejected. Closing the panel or leaving the
detail during a pairing replies to the question, cancels the pairing and unregisters the agent, in
that order.

**The headset row.** When the default sink is a Bluetooth device that offers both kinds of profile,
a `‹ Hi-Fi ›` / `‹ Mic ›` row sits between the volume and the brightness; Space, Enter, ← → or a click
switch to the other kind, with `wpctl set-profile <device id> <index>`, choosing that kind's
available profile of the highest priority (Hi-Fi: names starting `a2dp-sink`; Mic: `headset-head-unit`).
The row is hidden for a device whose active profile is of neither kind (LE Audio, for instance), and
it follows the profile when WirePlumber switches it on its own. The device and its profiles
(at most 8 devices of 32 profiles) are kept from the `pw-dump` child the sound source already runs
(`pipewire/bt.rs`): no process is started when the panel opens.

**How it talks to BlueZ.** What the panel shows is read on the D-Bus thread's read-only connection,
only while the panel is open: a tap with a scope (Closed, Row or Detail) and its own socket pair, and
a reader that adds no match rule. What the panel changes goes through `bt_act.rs` in
`sources/dbus/`, a second connection to the system bus on the event loop, opened only while the detail
is open or a call is in flight and closed with its agent unregistered first. Its calls are a pinned
list (the power and trust properties, discovery, removal, connect, disconnect, pair, cancel, and the
agent's registration), each sent to BlueZ's unique name with auto-start off, plus `GetNameOwner` to
the bus driver to learn that name. When BlueZ goes or its
owner changes, the connection is let go without a word. The model is `bt.rs` (pure) and the wiring is
`bt_app.rs`; no thread, no timer of its own: the 30 s deadlines, the 4 s `failed` and the 3 s question
use the panel's one timer. The guardrails are `the_bluetooth_connection_acts_only_through_act_method`,
`the_bluetooth_connection_goes_with_the_panel`, `the_bluetooth_connection_serves_only_the_agent`,
`only_bt_app_and_bt_act_name_btact` and `the_bluetooth_logs_name_no_device`.

**Not yet verified.** At the trial: BlueZ's D-Bus policy on NixOS (`bluetooth.conf`, no polkit)
letting the session user call `Pair`, `Connect` and `RegisterAgent` with no `AccessDenied`; a real
pairing of a keyboard (a passkey to type); the headset switch with WirePlumber's real profile names.
To judge at the trial: whether one left click should disconnect a connected device with no question,
and whether the headset row's flicker during a Hi-Fi to Mic switch (the sink goes and comes back) is
acceptable. See the decisions "Space on a blocked Bluetooth radio unblocks it, then powers it on",
"Nearby Bluetooth devices are inserted once, by signal strength", "A paired Bluetooth device is
trusted, and a connect that is already done counts as done", "Forgetting a Bluetooth device asks
first, as the power menu does", "Bluetooth deadlines live in the panel, and a late answer is
forgotten", "A pairing is answered by alpheus's own agent, for that device only", "A pairing code has
no timeout, and leaving the detail cancels the pairing" and "The Bluetooth acting connection opens
with one blocking connect and asks for BlueZ's owner itself".

### VPN

**The row.** A VPN row under the Bluetooth row: the first activated profile's name (cut to 16 cells),
`…` while one is activating or deactivating, `failed` for 4 s after a failed attempt, else `off`. It
is hidden when NetworkManager has no VPN or WireGuard profile; for that, each open of the panel lists
the saved profiles and reads their settings (at most 64 are read). Space takes the first activating or
activated profile down, else brings the last used one up (the highest `connection.timestamp`, over
every profile read, even past the 16 listed).

**The detail.** Enter, →, a click on the row or `alpheus-launch --quick vpn` opens it. It lists at
most 16 profiles, the active ones first with `●`, then by name; a WireGuard profile has its own glyph
in front of the name, any other the VPN glyph of the `network` module. Enter on a profile brings it
down when it is activating or activated (`nmcli connection down uuid <uuid>`), does nothing while it
deactivates, and otherwise brings it up (`nmcli connection up uuid <uuid>`). The tag after a name is
`…` while an attempt is under way on it or NetworkManager activates or deactivates it, `failed` for 4
s after a failure. One attempt at a time: during one only `More…` acts. `More…` runs `[quick]
vpn_more` and closes the panel. ↑ ↓ Home End walk the profiles and `More…` without wrapping, Esc or ←
go back, a left press chooses and enters, a press on the header goes back.

**An attempt** ends when NetworkManager reports the profile activated. It fails when the connection
is deactivating or deactivated, when an activation that was under way is left with no active
connection at all, after 20 s with no word, or at 60 s when it is still activating. A failure shows
`failed` and writes one log line, `quick: vpn connection failed (<why>)`, whose reason is a fixed
phrase and never a profile's name or uuid; for a profile that NetworkManager is set to ask a secret
for, the reason says the connection may need a secret. NetworkManager going away, or the profile
disappearing, ends the attempt with no `failed` mark. A NetworkManager-domain outage drops the VPN
the way it drops Wi-Fi: the row hides and an open detail closes.

**Secrets.** alpheus never handles a VPN secret: no `nmcli --ask`, no password field, no `GetSecrets`.
A profile that needs one is answered by NetworkManager's stored secrets or a desktop's secret agent,
else it fails, and `More…` is the way. Known limit: a non-zero exit of `nmcli` is not seen (the
executor's `spawn` is detached), so a failed command shows `…` until NetworkManager's state says
otherwise or after 20 s. A VPN profile past the 64th saved connection is not seen. Any listing or
settings error other than a gone service or an absent or denied profile ends the read and keeps the
last state, so a persistent odd error on one profile freezes the VPN view. A snapshot holding only a
stale deactivated entry for a fresh attempt, before NetworkManager lists the new one, fails that
attempt at once. **Not yet verified.** Whether `nmcli connection up` finds a VPN's stored secrets
without `--ask` is checked at the trial. See the decisions "alpheus never handles a VPN secret",
"Space on the VPN row takes the active profile down, else brings the last used one up", "A VPN attempt
lives in the panel only, and taking a profile down is no attempt" and "Several active connections for
one VPN profile count by the most advanced state".

## Using it

**Opening.** A left click on the volume, network, Bluetooth or `shield` module opens the panel under
it (the right click opens the control center's tab). `alpheus-launch --quick [sound|wifi|bluetooth|vpn]`
opens it under the bar's right end on the focused output, on that section; the same verb again closes
it. Only one bar popup is open at a time. Without `quick` in `modules`, the verb exits 0 and opens
nothing.

**Keys.** ↑ ↓ and Home End move between rows (Ctrl+P and Ctrl+N too); ← → change a slider by
`[quick] step` percent, or move between session buttons; Space and Enter toggle a row, Enter on the
volume opens the outputs; Esc leaves a detail, else closes the panel. Only the moving keys (arrows,
Ctrl+P and Ctrl+N) and erasing repeat when held. The wheel over a slider changes it; elsewhere it moves the window over the rows when they do
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
| `reboot` | `["systemctl", "reboot"]` | the Reboot button's command; empty hides the button |
| `poweroff` | `["systemctl", "poweroff"]` | the Power off button's command; empty hides the button |
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
  It reads no clock (the wiring passes `now`) and runs and starts nothing.
- `draw.rs`: the model's state turned into cells (rows, sliders, switches, session buttons).
- `app.rs`: the wiring (`QuickApp` on the `App`, and the `quick_*` methods): opening, the reads,
  spawning commands, the timer, and the Wi-Fi attempt that outlives the panel.
- `state.rs`: a configured toggle's state (`file:` or `process:`), one bounded read.
- `secret.rs`: the Wi-Fi password's buffer.
- `dnd.rs` and `shield.rs`: the do-not-disturb detail and the Shield detail, each its own struct
  with its own state, rows and keys (the Shield detail also its tags).
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
"A slider sends one level per 150 ms window and keeps showing it" to "The Bluetooth acting connection
opens with one blocking connect and asks for BlueZ's owner itself".

## Rules it keeps

These are enforced by tests, so a change that breaks one fails the build.

- `the_quick_model_and_its_state_reader_only_read` (`tests/guardrails.rs`): the model, the cells, the
  state reader and the Wi-Fi, Bluetooth, VPN, do-not-disturb and Shield files write no file, create
  nothing and make no raw system call (no `libc::`, no `unsafe`). That they run no command and start
  no thread is `the_tray_spawns_no_thread`, through its `NO_THREAD_NOR_COMMAND` list; that they arm
  no timer is the `Timer::` table of `nothing_resident_while_the_launcher_is_closed`. That they read
  no clock is a rule of their headers, no guardrail checks it.
- `every_quick_command_stays_on_the_allowlist`: every command the panel can build, from every key on
  every row and every press of every cell, is one of the exact shapes; the configured commands run as
  given.
- `the_quick_panel_has_no_panicking_path`: no `unwrap`, `expect` or `panic!` in the production code
  of the panel's files (`mod.rs`, `draw.rs`, `state.rs`, `app.rs`, `wifi.rs`, `secret.rs`, `bt.rs`,
  `vpn.rs`, `dnd.rs`, `shield.rs`, `session.rs`, `bt_app.rs`).
- `the_wifi_password_goes_to_stdin_only_and_is_wiped_after`: the password is written to `nmcli`'s
  standard input, followed by a newline, and then the field closes; `quick/mod.rs`, `wifi.rs`,
  `secret.rs`, `app.rs`, `draw.rs` and `popup.rs` format no key, secret, password or field with
  `Debug`; the password buffer derives neither `Debug` nor `Clone` and has one `unsafe` block, the
  volatile wipe.
- `the_vpn_logs_name_no_profile`: no log line of the VPN files or the VPN lines of `app.rs` names a
  profile.
- `the_shield_logs_name_nothing`: every log call of the Shield's files, and the Shield's lines in the
  others, holds only fixed words and never names a name, an id, a `vid:pid`, a command, a flag, a path,
  a configuration or a device.
- `the_quick_panel_lets_go_of_the_sound_and_its_timer_whatever_closes_it`: whatever closes the
  panel, the sound thread goes back to its own scope and the panel's timer goes.
- `the_bluetooth_logs_name_no_device`: no log line of the Bluetooth files names a device, and their
  glue formats nothing with `Debug` but an `io::ErrorKind`.
- `the_panel_readers_have_no_panicking_path`: no `unwrap`, `expect` or `panic!` in the production
  code of the Wi-Fi and VPN readers on the D-Bus thread (`sources/dbus/wifi.rs`, `vpn.rs`).
- `the_tray_spawns_no_thread`: the quick panel's model files and `bt_app.rs` are on its
  `NO_THREAD_NOR_COMMAND` list, so they run no command and start no thread.
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
  bar without it (when the first delivery merged); open, 316–338 KB over rest; once closed, +208…+210
  KB over rest (the first open's code and font pages, paid once); 20 more cycles −28…−4 KB (these
  three re-measured at the UI refresh against `d9b8965`). The test bounds total PSS: the module at
  rest ≤ 256 KB, open ≤ 1 MB over rest, closed ≤ rest + 512 KB, 20 more cycles ≤ closed + 128 KB.
- **The details open:** the Wi-Fi detail on 40 access points (32 listed), 898–910 KB over the module
  at rest (`quick_wifi_pss_detail_open_and_closed`); the VPN detail on 10 profiles, 846–854 KB
  (`quick_vpn_pss_detail_open_with_10_profiles_and_after`), +696…+704 KB of the daemon's own once
  closed, of which +60…+68 KB is anonymous (the rest is the first open's code and font pages); the Bluetooth detail on 20 devices,
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
  one-second timer left armed by a close was shown to turn the quick panel's, the Wi-Fi detail's and
  the VPN detail's tests red.
