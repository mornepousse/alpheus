# More OSD kinds and a keyboard module: design

Date: 2026-11-27. Status: approved by the owner ("ok go", 2026-11-27) through the draft's
recommendations and `OWNER-DECISIONS.md` points 1 and 2. Calls marked *(writer's call)* were not
settled by the draft or the decisions.

## Intent

The OSD pill shows only volume, microphone and brightness. Other bars also flash a short pill
when Bluetooth, do-not-disturb, the power profile, the keyboard layout or the track changes, and
show the layout and the bind mode in the bar. alpheus already receives every one of these facts
(or can receive them with no new connection), so this costs a few strings and some rules.

## Goals

- Five new passive OSD kinds, each opt-in in `[osd] kinds`: `bluetooth`, `dnd`, `profile`,
  `layout`, `track`.
- A bar module `keyboard`: the active layout (short code by default) and the bind mode when it is
  not `default`. Display only.
- Mango first (the owner's compositor), Sway second, each its own task.
- A kind or a module that is not configured costs nothing: no subscription, no source, no field
  published.

## Non-goals

- Caps Lock and Num Lock indicators (decision 2): no source that is event-driven and private exists
  without keyboard focus (sysfs LEDs may go stale, evdev is a key-logger by shape).
- Wi-Fi radio on/off as a pill: `NetState` cannot tell "radio off" from "no link"; a later change
  to the network source.
- A click on the module that cycles the layout: that is a compositor command, and the daemon never
  writes to the compositor. A user command in `bar.toml` may come later.
- Hyprland and niri: retired; the module draws nothing there.

## Anti-goals

- No key grab, no `wl_keyboard`, no `/dev/input`, no polling, no timer.
- No `mmsg` child and no write to either compositor beyond today's requests, except the read-only
  `GET_INPUTS` (decision 1).
- No layout name, track title, profile or mode in a log line.

## Constraints

- Lightness: 15 MB resident at rest for everything; no new thread, timer or dependency.
- Session safety: tests use `FakeMango`, `FakeSway` (unit) and the isolated headless Sway; never
  the owner's compositor, never a session tool.
- Privacy: the track title is on screen only when the user lists `track` (it is off by default,
  like every new kind), never in the log.
- Compositors: Mango's `watch all-monitors` snapshot already carries `keymode` and
  `keyboardlayout` per monitor (`tests/fixtures/mango-all-monitors.json`). Sway needs `GET_INPUTS`
  (type 100) at session start and the `input` and `mode` events.

## Approaches considered

1. **Read what the existing compositor connection gives (picked).** Mango: two more
   `#[serde(default)]` fields in a snapshot already parsed. Sway: one read and two events on the
   connections already open. OSD kinds reuse messages that already reach `on_bar_message`.
2. **`mmsg watch keyboardlayout` / a second Sway connection.** A child process or a socket per
   feature; rejected (a resident child, a guardrail broken for nothing).
3. **Wayland keyboard events** (`wl_keyboard.modifiers`, layout from the keymap): they reach only
   the focused surface, and the bar never has focus. Rejected.

## The design

### What the user sees

- **Pills.** The same pill, one row, glyph then text: `󰂯 Bluetooth … on|off`, `󰂛 Do not disturb
  … on` / `󰂚 … off`, `<profile glyph> Power profile … balanced`, `󰌌 English (US)` (the full name,
  whatever `short` says), `▶ <title>`. The right-hand word sits where the percentage sits today.
  "Off" dims the glyph. A newer pill replaces the shown one, as today.
- **Rules, every kind.** The first state after its source (re)starts is the baseline (no pill); a
  1 s grace (`osd::GRACE`) follows it; then a change of value shows. A source going down resets
  the baseline. Bluetooth shows on a power flip only (`Off` ↔ `On`), never on a device connecting;
  `Absent` resets. Do-not-disturb shows only while alpheus serves notifications. The track shows
  when the title changes while playing; pause and resume of the same title show nothing; a burst of
  metadata within the grace after a player appears shows nothing. A layout shows on a name change;
  a hotplug burst shows the last name once (one pill replaced in place).
- **Keyboard module.** `󰌌 us`, and the mode after it in the accent when not `default`
  (`󰌌 us resize`). Unknown layout and mode: no segment. It sits in the status block and is dropped
  early on a narrow bar (in `REMOVAL`, right after `Window`) *(writer's call)*. No click.

### Configuration (bar.toml, read at start)

| Key | Default | Meaning |
| --- | --- | --- |
| `[osd] kinds` | `["volume", "mic", "brightness"]` | which pills show; also `bluetooth`, `dnd`, `profile`, `layout`, `track`. An unknown name, or `dnd` without `notifications` in `modules`, makes the file invalid |
| `modules` | (unchanged; `keyboard` not in it) | `"keyboard"` adds the module |
| `[keyboard] short` | `true` | `English (US)` → `us`: the 2–3 ASCII letters in parentheses, else the first two letters of the name, lower case; `false`: the full name |
| `[keyboard] show_mode` | `true` | show the bind mode when it is not `default` |

A kind brings the source it reads, as `osd` brings `pw-dump` *(writer's call)*: `bluetooth` the
BlueZ domain, `profile` power-profiles-daemon, `track` the MPRIS players, `layout` the keyboard
fields of the compositor source. `dnd` needs the `notifications` module (alpheus must hold the
notification name). A list without `volume` and `mic` no longer starts `pw-dump` for the OSD, and
without `brightness` the verb shows nothing.

### How it is built

- `compositor/mod.rs`: `CompositorState` gains `layout: Option<String>` and `mode: Option<String>`
  (cleaned with `text::clean`).
- `compositor/mango.rs`: `Monitor` gains `keymode` and `keyboardlayout`; `Model::new(keyboard:
  bool)` copies the active monitor's (else the first's) into the state only when `keyboard`.
- `compositor/sway.rs`: `Model` keeps `layout` and `mode`; `inputs(&[u8])` reads the first
  keyboard's `xkb_active_layout_name` from `GET_INPUTS`, `input_event` the event's
  `input.xkb_active_layout_name` (the last keyboard that changed wins), `mode_event` its `change`.
- `sources/sway.rs`: `Message` gains `SubscribeKeyboard` (payload
  `["workspace","window","input","mode"]`) and `GetInputs` (100, empty). `session(socket,
  keyboard, publish)` subscribes with one or the other; `GetInputs` is sent once per session, only
  with `keyboard`. An `input` or `mode` event updates the model without re-reading the tree
  *(writer's call: the event carries the layout, so no `GET_INPUTS` after it)*. Sway gives no mode
  at start: none until the first `mode` event.
- `sources/mod.rs`: `start(c, keyboard, send)`; `startup.rs` computes `keyboard` as `keyboard` in
  `modules`, or `osd` with `layout` in `kinds`.
- `modules.rs`: `ModuleId::Keyboard`, `Needs::KEYBOARD`; `startup::wanted` takes the OSD kinds'
  needs (`OsdKind::needs`) in place of the `osd` row's static `SOUND`.
- `segments.rs`: `keyboard(&Feed<CompositorState>, &KeyboardConfig) -> Option<Segment>`,
  `short_layout(&str) -> String`.
- `config.rs`: `OsdKind`, `OsdConfig.kinds: Vec<OsdKind>`, `KeyboardConfig`.
- `osd/mod.rs`: `Passive<T: PartialEq + Clone>` (baseline, grace, last value; `hear(Option<T>,
  now) -> Option<T>`), `TrackWatch`, `Kinds` holding one of each; `Pill` gains `Bluetooth(bool)`,
  `Dnd(bool)`, `Profile(Profile)`, `Layout(String)`, `Track(String)`; `rows` draws them;
  `describe` logs fixed words (`bluetooth on`, `layout changed`, `track changed`).
- `osd/surface.rs`: `osd_passive(&Msg)`, called from `on_bar_message` beside `osd_sound`, for
  `Bluetooth`, `Notifications`, `Profile`, `Media`, `Compositor`; a kind not listed returns at once.

## The owner's decisions applied

- Kinds (Q1, Q3): all passive kinds except Wi-Fi and lock keys, each opt-in, `track` included.
- Lock keys (decision 2): left out; no `show_locks` key.
- Click (Q4): display only.
- Sway guardrail (decision 1, Q5): `sway_writes_only_its_three_messages` becomes
  `sway_writes_only_its_messages` with five entries, in the same commit as the code, with
  `docs/features/bar.md` and the source's header.
- Wi-Fi radio (Q6): later. Names (Q7): short by default.

## Cost estimate

- Off: nothing. Mango parses two more strings per monitor per snapshot but keeps them out of the
  published state, so no extra publish or redraw. Sway subscribes to two events only with
  `keyboard`.
- Module on: 0 threads, 0 timers, no wake-up at rest; two short strings (< 1 KB); Sway one
  transient `GET_INPUTS` reply per session (a few KB). Expected < 64 KB PSS.
- Each kind: a few words of state; the pill's surface and timer are today's. A kind that brings a
  source costs that source's measured row (BlueZ, MPRIS, power profiles in `measurements.md`).

## Tests

Pure: Mango fields with and without `keyboard`, Sway inputs/events, `short_layout`, the segment,
`Passive` at its boundaries (baseline, grace end, same value, reset), each kind's rules, the
track's playing rule, config bounds. Guardrails: the Sway messages, OSD and keyboard files pure,
fixed-word logs. Headless: a Mango layout push and a Sway layout switch on the isolated Sway
redraw the module and show the pill; a Bluetooth flip on the private bus shows its pill; kinds off
read nothing. Memory: each task measures its parts against `main` (`docs/measurements.md`).

## Left to the builder

The exact pill wording within the shapes above, the profile glyph (the quick panel's `PROFILE`),
the segment's colours (no hue unless the bar's neighbours need one).
