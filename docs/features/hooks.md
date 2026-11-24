# Event hooks

A command of your own, run when something happens in the shell. This page says what it does, how
to use it, how to configure it, how it is built, which rules the tests enforce on it, and what it
costs. It describes the code as it is today.

## What it does

`[hooks]` in `bar.toml` maps an event to a command. When the event happens, alpheus runs the
command once, detached, with the event's data in environment variables. A hook can lock the screen,
change a wallpaper when the theme mode changes, light a USB LED while the camera is on, or log
battery levels. The bar, the OSD and the bubbles do what they do whatever a hook does, and whether
it runs at all.

There are thirteen events. Every hook gets `ALPHEUS_EVENT=<event>`, plus:

| Event | Fires when | Variables | Needs in `modules` |
| --- | --- | --- | --- |
| `started` | once, when the daemon has started its sources | none | nothing |
| `theme_mode` | the mode in effect changes (the portal's word in `auto`, or a `theme.toml` change) | `ALPHEUS_MODE` = `dark` or `light` | nothing |
| `dnd` | do-not-disturb turns on or off, by any path | `ALPHEUS_DND` = `on` or `off` | `notifications` |
| `battery_charge` | the laptop battery's charge state changes | `ALPHEUS_CHARGE` = `charging`, `discharging`, `full` or `pending`; `ALPHEUS_PERCENT` | `battery` |
| `battery_level` | the laptop battery's whole percentage changes | the same (`ALPHEUS_CHARGE` may also read `unknown`) | `battery` |
| `power_profile` | the active power profile changes | `ALPHEUS_POWER_PROFILE` = `power-saver`, `balanced` or `performance` | `quick` |
| `network` | the primary connection goes online or offline, or changes kind | `ALPHEUS_NETWORK` = `online` or `offline`; `ALPHEUS_LINK` = `wifi`, `ethernet`, `vpn`, `other` or `none` | `network` or `quick` |
| `vpn` | a VPN becomes active, or the last one goes | `ALPHEUS_VPN` = `on` or `off` | `network` or `quick` |
| `bluetooth` | the adapter powers on or off, or a device connects or disconnects | `ALPHEUS_BLUETOOTH` = `on`, `off`, `connected` or `disconnected`; `ALPHEUS_DEVICE`, the device's name, set for the last two only | `bluetooth` or `quick` |
| `privacy` | the microphone, camera or screen indicator turns on or off | `ALPHEUS_CAPTURE` = `microphone`, `camera` or `screen`; `ALPHEUS_STATE` = `on` or `off` | `privacy` |
| `displays` | a `displays.toml` profile was applied on hotplug | `ALPHEUS_DISPLAY_PROFILE`, the profile's name | `displays` |
| `shield` | Shield mode turns on or off | `ALPHEUS_SHIELD` = `on` or `off` | `[shield]` configured |
| `failed_units` | the failed units' total changes | `ALPHEUS_FAILED`, the total; `ALPHEUS_FAILED_SYSTEM` and `ALPHEUS_FAILED_USER`, each manager's count once it has answered | `failed` |

The rules that decide when one fires:

- **Edges only.** A hook fires on a change, never for the state at start. Each event's first value
  is a seed and never fires, and for the first 3 seconds after the daemon starts any change only
  moves the seed. `started` ignores that window. `failed_units` seeds each manager apart: the
  first answer of the system manager and the first of the user manager are each a seed, so a user
  count that arrives late is still a seed and never a change.
- **A source that is down is not a state.** NetworkManager or BlueZ leaving the bus, a missing
  reading: nothing fires and the seed stays; when the source returns, its value is compared with
  the seed.
- **One fire per change.** A message that changes several things fires each: two Bluetooth devices
  connecting are two `connected` fires; the adapter powering off with a device connected fires
  `disconnected` for the device, then `off`.
- **The variables an event does not set are removed** from the hook's environment, so a variable
  inherited from elsewhere never passes for the event's.

## Using it

Write the command in `bar.toml`, then restart the bar. Nothing else is needed: there is no key to bind and no verb.

A hook is an argv, run as given: no `~` or `$VAR` expansion. A hook that needs a shell says so:

```toml
[hooks]
started = ["notify-send", "alpheus up"]
theme_mode = ["sh", "-c", "gtk-theme-switch \"$ALPHEUS_MODE\""]
bluetooth = ["sh", "-c", "[ \"$ALPHEUS_BLUETOOTH\" = connected ] && headset-profile"]
```

At most five runs of one event happen at once, then one more every 3 seconds; a run that finds no
token is dropped, not queued. `started` goes through the same bucket as the other events (it has
its five tokens at start). The program is looked up on `$PATH` at each run, so one installed after
the daemon started is found. alpheus never waits for a hook, never reads its output and never kills
it. A hook runs in its own systemd scope, so it never dies with the bar and a restart of the bar
never kills it. When the bucket is dry, a dropped run logs one `throttled` line per episode, and the
next drops of that episode are silent.

When a hook does not run, the log says why in fixed words (see "How it is built").

Details that trip people up:

- **Finding the program.** The first word is an absolute path or a bare name looked up on the
  daemon's own `$PATH` (not your shell's); relative paths (`./x`) and `~` are not expanded or found.
  Quote variables in `sh -c` (`"$ALPHEUS_DEVICE"`): a name may hold spaces.
- **The data.** An event's data reaches the hook only through the environment, never as an argument;
  device and profile names are cleaned and cut to 255 bytes. A hook variable the event does not set is
  removed from the environment, so an inherited one never passes for the event's. A Bluetooth device
  whose name is only its address gets no `ALPHEUS_DEVICE`; one that is renamed from an address-like
  alias to its real name fires a disconnect and a connect (rename churn).
- **Rate limit.** A hook that re-triggers its own event settles at one run per 3 s.
- **A change takes effect when `alpheus-demon` restarts** (`bar.toml` is read once at start).

## Configuration

`[hooks]` in `bar.toml`, read at start (there is no hot reload of `bar.toml`). Each key is an argv
array; absent or `[]` means no hook. The keys are the events' names above: `started`, `theme_mode`,
`dnd`, `battery_charge`, `battery_level`, `power_profile`, `network`, `vpn`, `bluetooth`, `privacy`,
`displays`, `shield`, `failed_units`.

A misspelt key is an error (`deny_unknown_fields`), as everywhere in the file. A hook whose program
is blank is refused with `hooks.<key>: the program is blank`, and an argument holding a NUL with
`hooks.<key>: an argument holds a NUL`.

An empty or absent `[hooks]` costs nothing: no model is kept and each place that would feed it is
one `Option` check.

## How it is built

- `hooks/mod.rs`: the pure model. `HookId` (the thirteen events, their `[hooks]` keys, whether the
  configured modules feed them), `Event` and its variables (`Event::env`), `Hooks` (the seeds, the
  quiet window and the buckets) and `Hooks::fire`, which says whether an event runs now
  (`Verdict::Run`, `Throttled` or `Dropped`). It reads no clock (the caller passes the instant), no
  file, and starts nothing.
- `hooks/app.rs`: the wiring. The `hooks_*` methods on `App` hand what changed to the model, and
  `hook_spawn` starts a `Fire`: the program is looked up on `$PATH` first, then run in a systemd user
  scope (`systemd-run --user --scope --quiet --collect --`), or bare when `systemd-run` is missing
  or when no user manager exists. The spawner is chosen once at start, as the launcher's is. A
  hook goes through `Executor::spawn_with_env`, the one way to give a child variables without a
  shell.
- The tests sit next to each file (`hooks/tests.rs`, `hooks/app/tests.rs`) and in
  `tests/headless_session.rs` (the `hooks_*` tests).

**The log.** Fixed words only: `hook: <event>` when one runs; `hook: <event> failed (<kind>)` with
the kind `program not found`, `systemd-run failed` or `could not start`; `hook: <event> throttled`;
`hook: <event> detached (systemd-run not found)`; at start `hooks: <n> set` and `hooks: <event> never
fires (needs <modules>)`; and `hooks: quiet window over` once, when the first event after the
window is handed to the hooks. Never a command, an argument, a variable's value or a name.

**The path of one change.** A source's message reaches `on_bar_message` (`startup.rs`), which calls
`hooks_msg` before anything else uses it, so a source going down is known before the privacy
indicators it clears are read. `hooks_msg` gives the message to `Hooks::on_msg`, which returns the
edges; each goes to `hooks_event`, which asks `Hooks::fire` for a verdict, then calls `hook_spawn`
and writes the log line. The other events come in their own way: `hooks_dnd` from `dnd_apply`,
`hooks_mode` from `publish_mode`, `hooks_capture` from `privacy_changed`, `hooks_shield` from
`shield_readable`, `hooks_displays` from the displays controller's answer, and `hooks_started` from
`startup::run`, after the launcher's socket is watched so a `started` hook can call
`alpheus-launch`.

## Rules it keeps

- `hook_logs_carry_fixed_words_only` (`tests/guardrails.rs`): every log statement of `hooks/app.rs`
  is a fixed word or a number, whole statements read.
- `spawn_with_env_only_runs_hooks`: `spawn_with_env` is called from `hooks/app.rs` in one place.
- `hooks_start_after_the_launcher_socket_watch`, `hooks_read_a_message_before_the_privacy_feed` and
  `the_hooks_are_fed_where_state_changes`: where each event is fed from, and in which order.
- `the_calculator_has_no_panicking_path` (it also covers `hooks/mod.rs` and `hooks/app.rs`): no
  `unwrap`, `expect` or `panic!` in their production code.
- `the_tray_spawns_no_thread`: `hooks/mod.rs` starts no thread, process or command.
- The reasons are in [decisions.md](../decisions.md): "A hook fires on a change, never for the
  state at start", "A hook's data reaches it through the environment, never the arguments", "Each
  hook has a token bucket", "A hook never adds a source", "A hook runs in its own systemd scope",
  and "A source that is down is not a state".

## What it costs

Measured in release, back to back with the version before it (the full table is in [measurements](../measurements.md)).

- **Linked in, no `[hooks]`:** within the harness's spread at start (−967…−188 KB over three
  pairings); the stripped binary grew by 56 KiB. The model is `None`, so nothing is kept.
- **Every hook set** (`hooks_pss_at_start_and_after_fires`): +0…+64 KB of anonymous memory at
  start, and +20…+32 KB after a burst of 25 do-not-disturb changes.
- **Wake-ups:** none at rest after a burst (`at_rest_no_wakeups_with_hooks`). The bucket and the
  quiet window are timestamps compared when an event arrives; there is no timer, watch or thread.
