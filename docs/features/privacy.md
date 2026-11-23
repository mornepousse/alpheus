# The privacy indicators

The bar shows, while it happens, that the microphone, a camera or the screen is being captured, and
by which app. This page says what it does, how to use it, how to configure it, how it is built, which
rules the tests enforce on it, and what it costs. It describes the code as it is today.

## What it does

The `privacy` module draws nothing at rest. When something is captured it draws one glyph per
captured thing, in its own block, always in the same order and the same hues whatever `module_colors`
says:

| Glyph | Meaning | Hue |
| --- | --- | --- |
| `󰍬` | the microphone is held | orange |
| `󰄀` | a camera is held | green |
| `󰍹` | the screen is being captured | red |

While the default source is muted, the microphone glyph is dim: an app still holds the microphone, but
hears nothing. On a narrow bar the module is removed just before the power button.

**What counts.** The data comes from PipeWire's graph, which the sound source already reads:

- *Microphone.* A link from an audio source to a recording stream, active or running. A meter such as
  pavucontrol's, a visualiser that reads a sink's monitor, a member of a link group (a filter chain, a
  loopback, echo cancellation) and a passive stream never light it.
- *Camera.* A link from a device video source (v4l2 or libcamera, or the Camera role) to a video
  stream. A running device source with no link lights it with no app name.
- *Screen.* Any other video producer that runs or has an active link, which is what the portals' and
  niri's screen casts are.
- The compositors add to the screen: niri's cast events, and Hyprland's `screencast` events (a
  release counts only after a hold of 1 s, so a screenshot's blip never shows). Sway and Mango say
  nothing, so a recorder that bypasses PipeWire (wf-recorder) is not seen there.
- A camera opened outside PipeWire (`/dev/video*` held by a process that does not go through
  PipeWire: most V4L2 programs, Chromium without its PipeWire camera flag) also lights the camera,
  and the popup names the process by its `comm`. A camera held by another user's process is not seen.

An indicator that goes off stays shown for half a second, so a stream that renegotiates never blinks.

**The popup.** It lists, under one heading per lit indicator, the apps that hold it, or `(unnamed)`
when only the compositor knows (the compositors' events name no app). An app's name is the first of
the portal's app id, `application.name`, the process binary and the node's name, cleaned, at most
8 per indicator.

## Using it

- Left click on the module opens the popup under it; a second left click closes it. Nothing is
  selectable. Escape or a click outside closes it, and so does a restyle that moves the bar. It also
  closes by itself (`popup closed (nothing captured)`) once everything is off.
- Middle click runs `wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle`. The glyph's dimming follows from
  the next sound state.
- Nothing can stop a capture from here: that would be a write to the compositor.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar). `privacy` is in the default
`modules`, after `media`.

| Key | Default | Meaning |
| --- | --- | --- |
| `[privacy] ignore` | `[]` | app names never shown and never lighting an indicator on their own, compared whole and without regard to case. An indicator left with no name and no unnamed holder goes off |
| `[privacy] dev_dir` | `"/dev"` | where video devices are listed (watched, never opened); an absolute path |
| `[privacy] proc_dir` | `"/proc"` | where processes are listed (the camera scan reads their `comm` and open files); an absolute path |

An unknown key, or a path that is not absolute, makes `bar.toml` invalid: the daemon logs one line and
runs on the defaults. The two directories exist so that the headless tests can point the real daemon
at fake trees. Without `privacy` in `modules`, nothing of the graph is kept and nothing is watched or
scanned; the `[privacy]` keys are still checked when `bar.toml` is read.

## How it is built

Five files in `crates/alpheus-daemon/src/privacy/`, and the graph in
`crates/alpheus-daemon/src/pipewire/`:

- `mod.rs`: the model, pure. `GraphNode` and `GraphLink` are the graph, `capture` turns it into a
  `Capture` (`Indicator`s for the microphone, camera and screen), and `Privacy` merges it with the
  video devices and the compositor, applies the ignore list, the 0.5 s off-delay and the hold of a
  compositor's release, and draws the popup (`render`). Time is passed in, never read.
- `app.rs`: the wiring on `App`. `privacy_feed` reads each sound or compositor message before the bar
  keeps it; `privacy_changed` redraws the bar, follows the popup and syncs the timer;
  `privacy_timer_sync` and `privacy_tick` keep the off-delay's timer; `privacy_toggle` opens and
  closes the popup; `OPS` is its row in the table of popups.
- `camera.rs`: the camera outside PipeWire, pure of the loop. `scan` finds the holders, `classify`
  reads one inotify record, `CameraWatch` is the inotify instance on `dev_dir` and on each `video*`
  node (at most 64).
- `camera_app.rs`: the camera's wiring. `start` puts the inotify fd on the event loop and scans once;
  `camera_readable` drains it and moves the debounce; `camera_tick` runs the scan after it.
- `pipewire/graph.rs` keeps the graph, only when the model is built for privacy: nodes' class, state
  and name fields, and links, at most 1,024 nodes and 4,096 links. The compositor sources
  (`compositor/`) add what their cast events say.

Two timers, both armed only while something is pending: the off-delay's (`privacy_timer`, the bar's,
whether the popup is open or not) and the camera scan's debounce (`camera.timer`: 300 ms after a
device event, and at most 1 s after the first event of a burst). The inotify fd is level-triggered and
drained, so it never wakes the loop at rest. There is no thread.

**The path of one capture.** A browser records: `pw-dump` reports the new stream and its link, the
sound source publishes a state, and the loop hands it to `privacy_feed` before the bar keeps it.
`Bar::privacy_at` calls `Privacy::set_sound`, which finds the microphone lit and shown at once;
`privacy_changed` redraws the bar and syncs the timer. When the stream ends the graph no longer
holds it, `set_sound` finds the indicator no longer live and sets a pending off, and the timer is armed
for 0.5 s; `privacy_tick` then calls `due`, which clears the indicator and redraws. An open popup is
repainted by each change and closed when nothing is left.

**The path of one camera opened directly.** An app opens `/dev/video0`. The inotify record reaches
`camera_readable`, which drains the fd and moves the debounce; 300 ms later `camera_tick` runs
`camera_scan`, which reads `<proc_dir>/<pid>/fd` for the daemon's own user, names the holder by its
`comm` and gives it to `Privacy::set_devices`.

## Rules it keeps

- `the_privacy_module_has_no_panicking_path`: no `unwrap`, `expect` or `panic!` in `mod.rs`, `app.rs`
  or the privacy functions of the sound model.
- `the_tray_spawns_no_thread` (its list names `privacy/mod.rs`, `camera.rs` and `camera_app.rs`): no
  thread, process or command.
- `the_theme_and_camera_watches_only_read`: `camera.rs`, `watch.rs` and `inotify.rs` never write,
  create or remove a file, and no other file of the daemon names inotify.
- `nothing_resident_while_the_launcher_is_closed`: counts the timers, event-loop sources and threads
  the daemon builds, so the camera's timer and fd cannot be joined by another unnoticed.
- `the_privacy_timer_is_bar_level_and_fired_before_due`: the off-delay's timer is the bar's and not
  the popup's, it follows every change, and a message that changes nothing shown still syncs it.
- `privacy_changed_follows_sharing_for_dnd`: do-not-disturb is told after the timer is synced.
- The unit tests of `privacy/tests.rs` and the camera tests pin the rules and the scan, with fake
  `/proc` and `/dev` trees and a temporary directory for inotify; the headless tests (names starting
  `privacy_`) run the daemon against a fake `pw-dump` and fake trees. No privacy test reads the real
  `/dev` or `/proc` of the machine or calls a real tool.
- The reasons are in [decisions.md](../decisions.md): "The microphone lights for a recording stream,
  never a meter or a filter", "A muted app stream still lights the microphone", "A running camera
  with no link lights it unnamed", "An indicator going off stays shown for half a second", "A
  compositor's screencast release counts after a one-second hold", "A screen lit by the compositor
  alone reads `(unnamed)`", "The privacy popup lists apps and selects nothing", "The camera is read
  from `/proc` after a trigger, never counted", "A burst of camera triggers is scanned at most one
  second after its first", "PipeWire's own hold on a camera adds no name", "A video node udev has not
  opened to the user yet is watched again when its attributes change", "The camera scan runs on the
  event loop", "The camera watch is a second inotify instance".

## What it costs

Measured in release, back to back, 3 runs (the [memory tables](../measurements.md) have the figures and the method):

- **Linked in, `privacy` not in `modules`:** within noise at start (−80 to +116 KB for the capture
  graph, −236 to +100 KB for the camera), binary +76 KiB stripped for the first and +24 KiB for the
  second: code linked in, nothing allocated.
- **Module on, at start:** −88 to +120 KB over the same bar without it on a full-size graph, and the
  camera watch adds +84 to +192 KB.
- **A capture shown:** −60 to +26 KB; a camera holder shown −38 to +70 KB; both back to about idle
  once ended; 20 more cycles at most +64 KB.
- **At rest:** 0 loop wake-ups, and 0 for the whole daemon, after a capture and after a holder went.
- **The camera scan:** a median of 4.0 to 4.1 ms on 120 processes of 30 descriptors, on the event loop
  (`camera_scan_duration`, bound 10 ms); a 500 × 20 tree, printed only, a median of 14 to 15 ms.
