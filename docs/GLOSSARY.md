# Glossary

The words alpheus's code and documents use, in alphabetical order. Where a word is a type or a
file, its name is given so that you can search for it; [`ARCHITECTURE.md`](ARCHITECTURE.md) says
where each lives.

### Block

One module's group of cells in the bar, drawn as a unit by the block styles (`islands`,
`powerline`, `floating`, ...); in the code a `Group` in `grid.rs`, empty under the `flat` style.

### Budget test

A test in `crates/alpheus-daemon/tests/memory.rs` that starts the real daemon and asserts a memory
or wake-up bound; it refuses a debug build, so it is run in release (see
[`measurements.md`](measurements.md)).

### Deadline timer

`DeadlineTimer` in `deadline.rs`: one calloop timer set to the earliest deadline while there is one
and absent otherwise, so a feature that waits for a time costs no wake-up at rest.

### Detail

A second page of the quick settings panel opened from one row (Wi-Fi, Bluetooth, VPN,
do-not-disturb duration, outputs, Shield's blocked devices); in the code `quick/wifi.rs`,
`quick/bt.rs`, `quick/vpn.rs`, `quick/dnd.rs` and `quick/shield.rs`.

### Domain

One system service the D-Bus thread follows, the `Domain` enum in `sources/dbus/mod.rs`: network,
Bluetooth, battery, power profile and systemd's failed-units count. A signal re-reads only its own
domain.

### Executor

The `Executor` trait in `alpheus-common/src/exec.rs`, the only way alpheus starts an external
program: `SystemExecutor` in production, `FakeExecutor` in tests, and `NonBlocking` in the daemon,
which refuses the call that would block the event loop.

### Glue

The half of a feature that talks to the outside (clock, files, processes, Wayland, the bus), in
`app.rs`; the other half, the pure model in `mod.rs`, is handed everything it needs.

### Guardrail

A test that reads the source files and fails on a forbidden pattern (a write to the compositor, a
D-Bus method that acts, a thread in a file that must have none); they are in
`crates/*/tests/guardrails.rs` and `crates/alpheus-common/tests/boundaries.rs`.

### Layer surface

A Wayland surface of the layer-shell protocol (`zwlr_layer_surface_v1`) that a compositor draws at
a fixed layer: the bar is `Top`; the launcher, the bubbles, the OSD pill and the notes pad are
`Overlay`.

### Mail

The `Mail` enum in `startup.rs`: what the other threads send the event loop through one channel (a
bar message, a finished launcher scan, a Shield result, ...). Each message is handled on the loop's
thread.

### Message

A `Msg` in `state.rs`: one thing a source said (the compositor's state, the clock, the sound, a
battery reading). `App::on_bar_message` applies it to `state::Bar`.

### Needs

The `Needs` bit flags in `modules.rs`: the sources a module asks for. `startup::wanted` reads them
from the `modules` list, so a module that is not listed starts nothing.

### Pill

The small bar of the on-screen display that shows a volume, microphone or brightness level for a
moment (`osd/`); also the module-colours mode `pills` (`module_colors = "pills"`), which fills a
block with its hue.

### Popup body

The `PopupBody` enum in `popup.rs`: what an open popup shows (a tray menu, the calendar, a tooltip,
the media popup, the notifications panel, the quick panel, the power menu, the privacy list, the
resources popup, the timer). A popup is an `xdg_popup` hung from the bar's layer surface, one at a
time, dropped whole on close.

### Prove-it-bites

The rule that every test is shown to fail when the code it guards is broken: apply a mutation,
see the test go red, restore it (see [`CONTRIBUTING.md`](../CONTRIBUTING.md)).

### PSS

Proportional set size, the memory figure alpheus is measured in: a process's share of each page it
maps, read from `/proc/<pid>/smaps_rollup`; the budget tests print it and, for leak bounds, assert
`Anonymous:` (heap, stacks, written data) instead.

### Quiet window

The first 3 seconds after the daemon starts (`QUIET` in `hooks/mod.rs`), during which a change
only moves a hook's seed and fires nothing.

### Scope and tap

A `Tap` (`sources/dbus/tap.rs`) is how the loop tells the D-Bus thread what to read for an open
quick panel: a `PanelScope` (`Closed`, `Row` or `Detail`) plus a socket pair the thread polls beside
the bus. The thread reads and follows only what the scope covers, and nothing when it is `Closed`.

### Seed

A hook event's first observed value (`hooks/mod.rs`): it is stored, never fired, so that the state
at start does not run a hook; only later changes (edges) do.

### Segment

What one module contributes to a bar: a `Segment` in `layout.rs` (pieces of text with styles and
click targets). `layout` places the segments left to right; a module with nothing to show returns
none.

### Source

A thread, supervised by `spawn_source` in `sources/mod.rs`, that reads one part of the world (the
compositor, PipeWire, the system bus) and sends the loop messages, with a backoff when it goes
down. (The launcher's `[[sources]]` are a different thing: commands that list entries.)

### Verb

One request that `alpheus-launch` sends the daemon over its socket (`--toggle`, `--quick`,
`--osd`, `--dnd`, ...); the wire format is `alpheus-common/src/launcher_ipc.rs`, the list is in
[`cli.md`](cli.md).

### Wake-up

A voluntary context switch of a daemon thread: it blocked, then ran again. The budget tests read it
from `/proc/<pid>/task/*/status` and assert that it stays at zero at rest.
