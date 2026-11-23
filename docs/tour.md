# The life of a battery change

One real event, followed from the moment it enters the daemon to the pixel it changes: the laptop's
battery goes from 54 % to 53 %, and the bar's battery module says so. Every step names the file
and the function that does it, so you can open the code and read along. The map of the folders is
in [`ARCHITECTURE.md`](ARCHITECTURE.md); words in the [glossary](GLOSSARY.md) are linked at their
first use.

Written against commit `2b2c730`. Names are checked by reading the code at that commit; lines are
not given because they move, so search for the name. All paths are under
`crates/alpheus-daemon/src/`.

The path is the common one for every bar module that a source feeds: a source thread reads the
outside world, sends one [message](GLOSSARY.md#message) to the loop, the loop updates a pure state,
and a redraw turns that state into pixels. The battery is a good example because it also feeds two
side paths (the [hooks](features/hooks.md) and the low-battery alerts), which show how a message
fans out.

## 0. Why there is a thread at all

The daemon has one event loop, and nothing on it may block. D-Bus calls can, so the system bus has
its own thread, named `D-Bus`, started only when a configured module [needs](GLOSSARY.md#needs) one
of its [domains](GLOSSARY.md#domain). `battery` in `modules` sets `Needs::BATTERY` (the `battery`
row of `MODULES` in `modules.rs`); with no such module, `sources::dbus::start` returns before
creating a thread or a connection. The reason UPower is read on this thread and not on the loop is
the read-only, never-activating connection described in `sources/dbus/mod.rs` and
[`decisions.md`](decisions.md) ("UPower is the one service the daemon may start").

## 1. The signal arrives on the D-Bus thread

UPower sends `org.freedesktop.DBus.Properties.PropertiesChanged` from the object
`/org/freedesktop/UPower/devices/DisplayDevice` (the aggregate of the laptop's battery) with the
property `Percentage`. The daemon receives it only because it asked the bus for it:

- `MATCH_RULES` in `sources/dbus/dbusrs_bus.rs` holds one rule per domain; the `Domain::Battery`
  rule matches `PropertiesChanged` under `/org/freedesktop/UPower/devices`. A connection adds only
  the rules of its own domains, so the bus sends nothing else.
- The thread sleeps in `DbusRsBus::next_signal` (same file): it is blocked in `poll` on the bus's
  socket (and on the quick panel's taps, see [scope and tap](GLOSSARY.md#scope-and-tap)) with no
  timeout. At rest nothing wakes it.

## 2. The thread decodes just enough

`dbusrs_bus::signal(&Message) -> Signal` turns the raw message into a small `Signal`: path,
interface, member, the first string argument (for `PropertiesChanged`, the interface name) and the
*names* of the changed properties. Changed values are never decoded: the signal only says *which*
domain to look at, and the value is read again afterwards.

`Signal::domain` (`sources/dbus/signal.rs`) then decides whether this is worth a read:

- the path is under `/org/freedesktop/UPower`, so the domain is `Domain::Battery`;
- `relevant` drops the signal when `upower_device_ignored` says the object is a raw battery
  (`battery_BAT0`) or a line power supply, because the `DisplayDevice` already aggregates them;
- `relevant_properties` is an allowlist (`UPOWER_PROPS`: `Percentage`, `State`, `IsPresent`...); a
  signal whose properties are all outside it re-reads nothing.

`Percentage` is in the list, so `domain()` returns `Some(Domain::Battery)`.

## 3. The thread re-reads the domain

`session` (`sources/dbus/session.rs`) is the loop of one connection. After `next_signal` it calls
`read(bus, Domain::Battery, ...)`, which calls `upower::read` (`sources/dbus/upower.rs`):

1. `bus.owner(UPOWER)`: `GetNameOwner`, because every call goes to UPower's *unique name*, never
   its well-known name (a call to the well-known name could start the service);
2. `GetAll` on the `DisplayDevice`: if it is present and a battery, the percentage and the
   charge state (`charge`, `percent`) become a `Battery`;
3. `EnumerateDevices` and one `GetAll` per peripheral (a mouse, a headset), each turned into a
   `Peripheral`.

The result is a `PowerState`, stored in `SystemState::battery`.

## 4. Publish only if something changed

`publish_if_changed` (same file) compares the new `SystemState` with the last one published and
calls `publish` only when they differ. A signal that leaves the battery as it was (UPower sends a
few) stops here: the loop is never woken. 54 % to 53 % differs, so it goes on. `publish` is the
closure `sources::dbus::start` was given by the loop.

## 5. One message per domain, through the channel

That closure is built in `startup.rs`, in `start_sources` (the call to `sources::dbus::start`). It
calls `sources::dbus::messages` (`sources/dbus/session.rs`), which expands a `SystemState` into
eight `Msg` values, `Msg::Battery(s.battery)` among them, and sends each as `Mail::Bar(msg)` on the
calloop channel (`Mail` is defined in `startup.rs`). Sending on that channel is what wakes the
event loop; this is the only wake-up the whole chain costs.

## 6. The loop receives the mail

`insert_core_sources` (`startup.rs`) registered the channel as a loop source. Its callback matches
`Mail::Bar(m)` and calls `App::on_bar_message(m)`. The callback does not draw; it only updates the
state and raises a `dirty` flag (see step 9), so a burst of messages in one pass redraws once.

## 7. `on_bar_message`: who sees the message, in what order

`App::on_bar_message` (`startup.rs`) is where one message is shown to everybody who cares, in a
fixed order, *before* the bar keeps it, so that those who compare it with the previous value still
see the previous one:

```rust
let quick = self.quick_take(&mut m);      // the quick panel takes what is its own
...
self.hooks_msg(&m);                       // event hooks
self.privacy_feed(&m);                    // the privacy indicators (sound, compositor only)
self.battery_feed(&m);                    // low-battery alerts
note_compositor_message(self, &m);        // the launcher's window list
...
on_message(self, m, || LocalTime::now().0);   // the bar's own state
```

For `Msg::Battery` three of them matter.

**The hooks feed.** `App::hooks_msg` (`hooks/app.rs`) calls `Hooks::on_msg` (`hooks/mod.rs`), the
pure model. For `Msg::Battery(Some(PowerState { laptop: Some(b), .. }))` it compares the charge
state and the percentage with the last ones seen, and returns `Event::BatteryCharge` or
`Event::BatteryLevel` only for a change; the first value of each is a [seed](GLOSSARY.md#seed) and
fires nothing, and neither does an edge during the [quiet window](GLOSSARY.md#quiet-window) after
start. Only events whose hook is set are produced (`wants`). Each event goes through
`App::hooks_event`, which asks the model whether it may fire now (`Hooks::fire`, with its token
bucket) and, if so, starts the configured command through `hook_spawn`. With no `[hooks]` in
`bar.toml`, `self.hooks` is `None` and the function returns at its first line.

**The battery-alert feed.** `App::battery_feed` (`notify/app.rs`) passes the reading to
`battery_deliver`, which feeds `BatteryAlerts::feed` (`notify/battery_alert.rs`): one alert per
`[battery] warn` threshold crossed while discharging, re-armed by any charge. When it returns an
`Alert`, `incoming` makes the notification and `notify_act` hands it to the notification server;
the log line is `battery: alert at N %`. It counts an alert as given only while the daemon holds
the notification name (`serving`), per [`decisions.md`](decisions.md) ("A battery alert is given
only while the server serves"). `self.battery_alerts` exists only when `alerts_wanted` is true:
`battery` and `notifications` both in `modules` and a non-empty `warn`.

Both feeds are read-only with respect to the message: neither changes `m`.

**The bar's own state.** `on_message` (`startup.rs`) calls `dispatch_msg`, which calls `Bar::apply`
(`state.rs`). The arm `Msg::Battery(s) => update(&mut self.battery, s)` stores the reading as a
`Feed::Ready(PowerState)` (or `Feed::Down` for `None`) and returns whether it differs from the
stored one (`set_if_changed`). `state::Bar` holds only what the sources said; it has no idea how
it will be drawn. When `apply` returns true, `on_message` sets `*target.dirty_mut() = true`.

## 8. Back to the end of the pass

The loop callback returns. If other messages arrived in the same pass they are handled the same
way, each only updating state. `main_loop` (`startup.rs`) then reaches the code after
`event_loop.dispatch`: `after_dispatch` (`startup.rs`) takes the `dirty` flag and, if it was set,
calls `DeferredRedraw::redraw_now`, which for `App` (`wayland.rs`) is `redraw_all`: `draw(i)` for
each bar surface (one per output).

## 9. From state to a grid

`App::draw` (`wayland.rs`) asks the pure side for what to show: `Bar::grid` (`layout.rs`).

1. For each module in `config.modules`, `segment` calls the module's row of `MODULES`
   (`modules.rs`). The battery's row has `segment: Some(|bar, _| segments::battery(&bar.battery,
   &bar.config.battery, bar.module_colors))`.
2. `segments::battery` (`segments.rs`) builds a [segment](GLOSSARY.md#segment): the icon chosen by
   `battery_icon` (a glyph per ten percent, a charging variant), the `53%` text, each piece with a
   `Style` (warn or error colour below the thresholds, green with `module_colors`), plus the
   peripherals according to `[battery] peripherals`. A battery shows `Feed::Down` as a dimmed
   dash and `Feed::Waiting` as nothing.
3. `Bar::grid` removes modules from the right when the bar is too narrow (the `REMOVAL` list), then
   `lay_out_styled` places the segments left to right in the configured order and writes the `Grid`:
   a row of cells (a character and a style each), the click zones, and one `Group`
   ([block](GLOSSARY.md#block)) per module under the block styles.

The grid is the tested object: layout tests compare it, and the painter only copies it.

## 10. The painter and the buffer

Back in `App::draw`:

1. `if s.last.as_ref() == Some(&grid) { return; }`: if this output's grid is identical to the last
   one drawn (a state change that draws the same cells, such as a peripheral's reading the bar does
   not show), it stops. Here the text changed from `54%` to `53%`, so it goes on.
2. `pick_buffer` finds a free buffer of the surface's shared-memory pool (or makes one).
3. `Fonts::paint_bar` (`painter.rs`) draws the grid into it: the per-style background pass, then
   each cell's glyph with cosmic-text. The painter reads the theme's 16 roles and nothing else.
4. `painter::opaque_rects` and `send_opaque` tell the compositor which part is opaque.
5. `surface.set_buffer_scale`, `damage_buffer`, `attach_to` (the buffer onto the surface) and
   `s.layer.commit()`: the Wayland requests that make the new pixels visible. `s.last` is updated
   with the grid.

The compositor shows the new frame. The loop goes back to sleep: no timer was armed, no thread
woke, and `0` is the number of wake-ups until the next event.

## The whole chain at a glance

| Step | Where | What |
|---|---|---|
| signal in | `sources/dbus/dbusrs_bus.rs`: `MATCH_RULES`, `next_signal` | one match rule, a blocked `poll` |
| decode | `dbusrs_bus::signal`, `signal.rs`: `Signal::domain`, `relevant` | names only, an allowlist |
| read | `session.rs`: `read`, `upower.rs`: `read` | `GetNameOwner`, `GetAll` on unique names |
| dedupe | `session.rs`: `publish_if_changed` | nothing published if equal |
| to the loop | `session.rs`: `messages`; `startup.rs`: `Mail::Bar` | one `Msg::Battery` per change |
| dispatch | `startup.rs`: `insert_core_sources`, `App::on_bar_message` | hooks, alerts, then the bar |
| hooks | `hooks/app.rs`: `hooks_msg`; `hooks/mod.rs`: `Hooks::on_msg` | edges only, seeds, quiet window |
| alerts | `notify/app.rs`: `battery_feed`; `notify/battery_alert.rs`: `BatteryAlerts::feed` | one per threshold |
| state | `startup.rs`: `on_message`, `dispatch_msg`; `state.rs`: `Bar::apply` | `Feed::Ready`, dirty flag |
| redraw | `startup.rs`: `after_dispatch`; `wayland.rs`: `redraw_all`, `draw` | once per loop pass |
| segment, grid | `modules.rs` row; `segments.rs`: `battery`; `layout.rs`: `Bar::grid` | cells and zones |
| paint, commit | `painter.rs`: `Fonts::paint_bar`; `wayland.rs`: `draw` | pixels, `attach_to`, `commit` |

## Where this is tested

- The signal rules: `upower_raw_battery_and_line_power_signals_are_dropped_display_device_carries_them`
  and the fake-bus session tests in `sources/dbus/tests.rs` (the fake bus is
  `sources/dbus/fake.rs`; no test touches the system bus).
- The read: the tests at the end of `sources/dbus/upower.rs`.
- The bar's state and its single redraw: the tests of `state.rs` and `startup.rs`
  (`on_message` with a counting `DeferredRedraw`, no Wayland).
- The alerts' place in `on_bar_message`: the guardrail `on_bar_message_feeds_the_battery_alerts`
  (`tests/guardrails.rs`); a real run with a private bus is
  `battery_low_at_start_alerts_once_for_the_lowest_threshold` (`tests/headless_session.rs`).

For what a new module has to touch, read [`adding-a-module.md`](adding-a-module.md).
