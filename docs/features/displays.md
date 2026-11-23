# Displays (the daemon's side)

The daemon reads and changes the screens through the compositor's output protocol, applies a saved
profile when the set of connected screens changes, and holds the 15 s "keep these settings?" trial. This
page is about the daemon. The Displays tab of the control center, which draws the map and asks for
changes, is described in [control-center.md](control-center.md). It says what the daemon does, how to use
it, how to configure it, how it is built, which rules the tests enforce on it, and what it costs, and it
describes the code as it is today.

## What it does

- **Reads the screens.** The daemon binds the output manager (versions 1 to 4) when `displays` is in
  `modules` and the compositor has it: Hyprland, Sway, Mango and niri do. It keeps a model of every
  screen (name, description, make, model, serial, enabled, current mode, position, scale, transform and
  its modes, at most 256 per screen), updated only when the manager says `done`.
- **Changes them** only through one configuration at a time, sent whole. A change that would leave no
  screen enabled is refused. A change that changes nothing is not sent, and the request succeeds at
  once (displays spec § 1).
- **Applies a profile on hotplug.** With `[displays] auto = true` (the default), when the identities of
  the connected screens change, the profile of `displays.toml` whose screens are exactly the connected
  ones is applied once the screens have been still for 500 ms. No matching profile: nothing happens.
- **Holds the trial.** A change asked by the control center starts a 15 s trial; Keep ends it, and
  Revert, the deadline and a screen unplugged bring the screens back to what they were; a layout changed
  elsewhere ends the trial and restores nothing (`ChangedElsewhere`), and a refusal or no answer from the
  compositor are other ends (`Refused`, `NoAnswer`). It lives in the daemon, so a control center that dies
  cannot leave a bad layout.
- **Lowers a DisplayLink screen to 120 Hz.** No mode above 120 Hz is offered or accepted on a DisplayLink
  screen, and one the compositor sets above it is lowered by the daemon: the one change it makes without
  being asked.

## Using it

The daemon has no command-line verb of its own for displays; the control center is its client, over the
launcher's socket (the owner-only one the other `alpheus-launch` verbs use). The socket speaks four
verbs:

| Verb | Reply | What it does |
| --- | --- | --- |
| `outputs` | the screens and the trial's state | Answered from memory, with no round trip to the compositor |
| `outputs-try` | `done` or an error | Applies the desired layout and starts a trial; the reply comes once the compositor answered (5 s at most) |
| `outputs-keep` | `done` | Ends the trial and keeps the new layout |
| `outputs-revert` | `done` | Ends the trial and restores the layout from before |

The errors a client may show: `displays are off in bar.toml`, `the compositor has no output
management`, `another change is in progress`, `refused: no screen would stay on`, `the compositor
refused this configuration`, `no answer from the compositor`, and `no trial in progress (the daemon
restarted)` for a keep or a revert that reaches a daemon that has seen no try.

**Profiles.** `displays.toml` in alpheus's configuration directory holds the profiles; the control
center writes it. A screen is matched by its identity (`make model serial` joined by spaces, else its
description), never by its connector name, which DisplayLink screens swap between plugs. Two screens that
look the same make a profile not match, because a guess could swap them.

**After a daemon restart.** A trial is not kept across a crash: the layout of the trial stays. A trial
open, or a try in flight, when the daemon is stopped normally is restored.

## Configuration

`[displays]` in `bar.toml` (the struct is `DisplaysConfig` in `config.rs`):

| Key | Default | What it does |
| --- | --- | --- |
| `auto` | `true` | Apply the profile that matches the connected screens when they change; `false` turns the hotplug behaviour off |
| `drm_dir` | `/sys/class/drm` | Where the DRM connectors are listed; a screen whose card is driven by `evdi` is DisplayLink. Absolute; tests point it elsewhere |

`displays` is in the default `modules` and draws nothing on the bar. Without it the manager is not bound
and the verbs answer `displays are off in bar.toml`.

## How it is built

Four files in `crates/alpheus-daemon/src/outputs/`:

- `wlr.rs`: the only file that speaks the output protocol. It binds the manager, keeps the model of the
  screens, builds and sends one configuration (`Wlr::send`) and forwards the compositor's answers
  (`succeeded`, `failed`, `cancelled`) as events.
- `model.rs`: the model of a screen (`Head`, `HeadMode`) and the pure helpers on it: the targets of a
  configuration, the no-change check, the 120 Hz cap, the conversion for the wire.
- `control.rs`: the controller, pure. `Controller::on_event` takes an `Event` (a `done`, an answer, a
  configuration that could not be built) and returns `Action`s (send a configuration, write a log line,
  answer the control center, report a profile as applied). It holds the rules: one configuration in
  flight (a `done` meanwhile means "evaluate again after the answer"; `cancelled` is retried 3 times), the
  500 ms debounce, the 5 s watchdog, the trial, the cap on automatic applies, and `next_deadline`.
- `app.rs`: the wiring on the daemon's state. `OutputsApp` holds the controller, the protocol side, the
  timer and the waiting client; `execute` runs the controller's actions; `verb` answers a displays verb;
  `read_profiles` reads `displays.toml`; `is_displaylink` and `DisplayLinks::mark` set the DisplayLink flag
  of each screen; `TryClient` keeps the client of a try until the compositor answers.

**One timer.** `OutputsApp` has one `DeadlineTimer`, which `displays_sync` arms to the controller's
`next_deadline` after each event: the debounce, the watchdog, the trial's end, or the retry of a profile
that the limit of 3 automatic applies per screen set per minute skipped. At rest there is none.

**The path of a hotplug.** The compositor sends a `done` with the new screens. `displays_done` marks the
DisplayLink ones and hands them to the controller as `Event::Done`. If the set of identities differs from
the last one evaluated, the controller asks for the 500 ms debounce; `displays_sync` arms the timer. When
it fires, `displays_tick` has the controller read `displays.toml` (capped, from a regular file only),
which answers with a `Send` of the matching profile, unless a trial is open (it waits). `execute` sends
the configuration through `Wlr::send`; the compositor's `succeeded` comes back as an event, and
`displays_applied` hands the applied profile's name to the hooks (`[hooks] displays`).

**The path of a try.** The control center sends `outputs-try`; `displays_verb` decodes the desired layout
and asks the controller, which records the state before, sends, and waits. The client's connection is
kept by `TryClient` and watched for a hang-up until the answer comes (or the watchdog fires: no answer
counts as "possibly applied", the trial still starts and can be rolled back).

**Never written to the log:** a screen's identity, or the error of an invalid `displays.toml`. See "A
screen's identity and a profile file's errors are never logged" in [decisions.md](../decisions.md).

## Rules it keeps

- `the_daemon_only_reads_from_the_compositor`: the only writes to the compositor are the workspace
  command and output configurations, built and applied in `outputs/wlr.rs` and sent only from
  `outputs/app.rs`; never output power management.
- `outputs_protocol_only_in_wlr_rs`: no other file names the protocol.
- `output_power_management_never_appears`: no `dpms` and no output power management anywhere in alpheus.
- `the_displays_timer_is_synced_once_the_loop_exists` and
  `the_displays_are_restored_before_the_app_is_dropped`: the timer is synced as soon as the loop exists,
  and a trial open at shutdown is restored before the connection closes.
- `the_tray_spawns_no_thread`: `outputs/` starts no thread, process or command.
- The thread and timer inventory in `guardrails.rs` counts the one `DeadlineTimer` and the one hang-up
  watch of the try's client.
- Unit tests: `displaylink_detection_reads_the_driver_link`,
  `a_driver_name_must_end_in_evdi_and_an_unreadable_name_is_not_displaylink`,
  `displays_toml_is_read_capped_from_a_regular_file_only`, and the verbs and the waiting client in
  `outputs/app.rs`, the controller's in `outputs/control/tests.rs`.
- The reasons are in [decisions.md](../decisions.md): "The displays are changed by the daemon, and its
  trial lives there", "displays.toml is read at each evaluation of the profiles, never watched", "A DisplayLink screen
  is told from the DRM directory, once per screen name".

## What it costs

Measured in release, back to back, 3 runs (the [memory tables](../measurements.md) have the figures and the method):

- **Linked in, off:** +4 to +168 KB at start over nine pairings of the `everything` case of
  `startup_pss_per_source`; the binary grew 244 KiB stripped.
- **On at start:** +56 to +204 KB with 1 output, −36 to +36 KB with 3 (`displays_pss_at_start`).
- **Trials and hotplug:** a trial open −46 to +8 KB over idle and 20 more trials +0 KB of the daemon's own
  memory (`displays_pss_after_trials`); 20 hotplug cycles +40 to +44 KB, then +8 KB between cycles 21
  and 41 and +0 to +8 KB from 41 to 61, in the daemon's own memory (PSS without the shm pools;
  `displays_pss_after_hotplug_cycles`).
- **At rest:** 0 loop and 0 whole-daemon wake-ups after the start, a trial and the hotplug cycles.
- **Parked:** a 64 KiB `displays.toml` raises the heap by about 2.8 MB once, because the parse peaks at
  about 44 times the file's size and the allocator keeps the high-water mark; a real file of a few
  profiles costs 20 to 60 KB.
