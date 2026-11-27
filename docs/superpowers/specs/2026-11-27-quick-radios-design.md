# Quick settings: keyboard backlight and airplane mode (design)

Date: 2026-11-27. Status: the owner settled the draft's questions by its recommendations ("ok go",
2026-11-27). Cluster 04 of the feature drafts, step 4 of the cluster order. The writer made the
calls marked *(writer's call)*.

## Intent

Two everyday switches of a quick panel, each a row built when the panel opens and dropped when it
closes; nothing resident.

## Goals

- A **Keyboard backlight** row: a stepped slider over the device's own levels (three on a
  ThinkPad: off, low, high), hidden without a `*::kbd_backlight` LED.
- An **Airplane mode** row: Space blocks or unblocks the Wi-Fi, Bluetooth and WWAN radios at once.
  It reads "on" when every one of those radios that is present and not hard-blocked is soft-blocked.
- The same behaviour on Mango (the default) and Sway (the fallback): neither row involves the
  compositor.

## Non-goals

- **The night light schedule** (draft section "Night light schedule"). It is blocked until the owner
  tries `wlsunset` by hand with the DisplayLink dock attached (owner decision 8). The evdi driver
  probably exposes no gamma LUT, so the feature may end up covering only the laptop panel and native
  outputs. It gets its own spec after the trial.
- An OSD pill for keyboard backlight changes (decision 7).
- A bar module, a hard-block override, GPS/NFC/UWB radios (`rfkill block all`, decision 1).
- Remembering which radios were on before airplane mode (decision 2): leaving airplane mode
  unblocks all three types, as GNOME does.
- Following changes while the panel is closed: they are read at the next open.

## Anti-goals

- No `/dev/rfkill` fd, watch, thread, timer or poll, open or closed.
- No udev rule or other system configuration: alpheus never writes the user's system configuration.
- No DPMS, sleep or idle handling.
- No device name, rfkill index or command in a log line.

## Constraints

- **Lightness.** A feature that is not enabled costs nothing; 15 MB resident at rest for everything.
  No new dependency, thread, timer or polling.
- **Session safety.** Tests never run `brightnessctl` or `rfkill`: they use `FakeExecutor` in unit
  tests and a recorder on the harness's private `PATH` in headless tests. Every test bar points
  both new directories at fakes. `rfkill block wlan` acts by type, not by device, so a fake directory
  alone would not protect the owner's radios: only the private `PATH` does.
- **Privacy.** Logs are fixed words: `quick: keyboard backlight 2`, `quick: airplane on`.

## Approaches considered

**Writing the keyboard level.**
1. *Picked:* `brightnessctl --class=leds --device=<name> set <N>` through the executor, which the
   brightness row already uses. The LED's files are `root:root 0644`. When brightnessctl cannot
   write them, it falls back to logind's `SetBrightness`, which accepts the `leds` subsystem for the
   active session.
2. logind `SetBrightness` directly: a write call on the read-only system-bus connection.
3. Write sysfs: not writable without a udev rule (system configuration).

**Switching the radios.**
1. *Picked:* `rfkill block|unblock wlan bluetooth wwan` through the executor: one spawn, the three
   types in one command (util-linux takes several types). It is the same path as the Bluetooth
   row's `rfkill unblock bluetooth`, `FakeExecutor` can fake it, and no kernel event struct gets
   parsed. *(Writer's call: one command, not one per type.)*
2. Write an 8-byte `RFKILL_OP_CHANGE_ALL` event to `/dev/rfkill`: rejected, because it needs a raw
   device, `unsafe`, and a fake device in tests.
3. NetworkManager `WirelessEnabled`/`WwanEnabled` plus BlueZ `Powered`: rejected. That is two
   services and two write connections, and BlueZ's `Powered` is not a blocked radio. Both services
   already follow rfkill, so the Wi-Fi and Bluetooth rows update through their existing feeds.

**Reading the state.** Sysfs on panel open, and 300 ms after a radio command. A `/dev/rfkill`
reader would be an fd and wake-ups for a panel that is usually closed.

## The design

### What the user sees

- **Keyboard backlight**, in the sliders' block, right under Brightness. It shows a keyboard glyph,
  the bar, and the level as a percentage of the device's maximum (`0 %`, `50 %`, `100 %` on three
  levels), so the slider code is shared *(writer's call)*. ← → move one level when the maximum is at
  most 10. Above 10, they move `[quick] step` percent of the maximum, at least 1. The wheel works as
  on the other sliders. Space or Enter cycle the levels 0 → max → 0 (decision 3; Space also cycles,
  following the panel's rule that Space and Enter toggle a row: *writer's call*). A press on the bar
  sets the nearest level. 0 is allowed: an unlit keyboard is not a black screen.
- **Airplane mode**, the first row of the radios' block, right above Wi-Fi *(writer's call: it
  governs the rows under it)*. It shows an airplane glyph and the switch drawn on or off. When every
  present radio of the three types is hard-blocked, it shows `hard blocked` and Space does nothing.
  The row is hidden when there is no radio of those types.

### Keys and configuration (`[quick]` in `bar.toml`)

| Key | Default | Meaning |
|---|---|---|
| `kbd_backlight` | `true` | the keyboard backlight row (still hidden without a device) |
| `kbd_backlight_dir` | `"/sys/class/leds"` | where LEDs are listed; absolute path |
| `airplane` | `true` | the airplane mode row (still hidden without a radio) |
| `rfkill_dir` | `"/sys/class/rfkill"` | where rfkill switches are listed; absolute path *(writer's call: tests need it, as `backlight_dir` and `drm_dir`)* |

When `kbd_backlight` or `airplane` is `false`, nothing is read at open.

### How it is built

- `crates/alpheus-daemon/src/kbd_backlight.rs` (new, next to `backlight.rs`; read only, no command
  or thread). `find(dir) -> Option<Kbd>` lists at most `backlight::MAX_ENTRIES` names and keeps the
  plain ones ending in `::kbd_backlight`. It takes the first in sorted order that is a directory
  whose `brightness` and `max_brightness` read as numbers, with `max` in 1..=255. Anything else hides
  the row. `reread(dir, device) -> backlight::Reread`-like `KbdReread { Level(u8), Unreadable, Gone }`.
  `Kbd { device: String, level: u8, max: u8 }` with `percent()`, `stepped(up, step_pct)`,
  `nearest(percent)` and `cycled()`.
- `crates/alpheus-daemon/src/rfkill.rs` (new; read only). `read(dir) -> Option<Radios>` reads at
  most 32 entries `rfkill*/{type,soft,hard}` and keeps the types `wlan`, `bluetooth` and `wwan`;
  `None` means none of them. `Radios::state() -> Airplane { On, Off, HardBlocked }` is pure.
- `quick/mod.rs`. New `Row::KbdBacklight` (group 0) and `Row::Airplane` (group 1, before `Wifi`)
  in `ROWS`, which grows from 14 to 16 entries. New `Quick` fields `kbd: Option<Kbd>`,
  `kbd_pace: Pace`, `kbd_settle`, `airplane: Option<Airplane>` and `reread_airplane:
  Option<Instant>`, filled by `set_kbd` and `set_airplane`. `Quick::new`'s signature stays the
  same, as with `set_headset`. `Due` gains `kbd: bool` and `airplane: bool`. Two command builders:
  `kbd_command(&Kbd)` gives `brightnessctl --class=leds --device=<name> set <N>` with
  `what = "keyboard backlight <N>"`, and `airplane_command(on)` gives
  `rfkill block|unblock wlan bluetooth wwan` with `what = "airplane on|off"`. The Wi-Fi row's
  `nmcli radio wifi …` and the Bluetooth row's `rfkill unblock bluetooth` also schedule an airplane
  re-read *(writer's call: the row never contradicts a radio switched from its own row)*.
- `quick/draw.rs`. `draw_kbd` reuses `slider`. `draw_airplane` uses `draw_mark`, or `text_row` +
  `write_cell` for `hard blocked`.
- `quick/app.rs`. The open reads `kbd_backlight::find` and `rfkill::read` when their keys are on;
  `quick_tick` re-reads them when `Due` says so. Spawning goes through `quick_spawn` (fixed `what`).
- `config.rs`. The four fields with their defaults, absolute-path checks, the header comment, and
  `docs/configuration.md`.

## The owner's decisions applied

Draft questions 1 (wlan + bluetooth + wwan), 2 (leaving unblocks all), 3 (stepped slider, Enter
cycles), 7 (no OSD) by their recommendations; 8: the night light waits for the DisplayLink trial.
Every task ends with a RAM measurement recorded in `docs/measurements.md`.

## Cost estimate

- **Disabled, or no device:** nothing at rest. The code linked in is estimated at +20…+60 KiB
  stripped.
- **Enabled, panel closed:** nothing: no fd, thread, timer or buffer.
- **Panel open:** a short `String` and three bytes; about 15 small sysfs reads per open; one
  `brightnessctl` per 150 ms window or one `rfkill` per Space (external, short-lived tools).
- **Wake-ups:** 0 at rest. The re-reads use the panel's existing timer.

## Tests

Unit tests on fake directories (never `/sys`) for both readers, the steps, the cycle, the exact
argv, the airplane state over soft/hard combinations and the re-read after a radio row's command;
config defaults and refusals; guardrails (both files on `NO_THREAD_NOR_COMMAND`, read-only and
no-panic lists, the allowlist's three new exact shapes, the harness's fake directories and its
`PATH` without `rfkill` or `brightnessctl`); headless recorders for one key on each row and 0
wake-ups at rest after; memory back to back against `main`. Names and assertions are in the plan.

## Not yet verified (at the trial)

- That logind accepts `SetBrightness` on `leds` from the session through brightnessctl.
- That `rfkill` writes `/dev/rfkill` unprivileged through the uaccess ACL.

## Left to the builder

The glyphs (from the Material Design range the panel already uses, checked against the bundled
font), the exact place of the glyph and value cells, and the hook in `app.rs` that schedules the
airplane re-read after the radio rows' commands.
