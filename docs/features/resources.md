# The resources popup

A popup under the resources module of the bar, with each figure and a short graph of its last
minutes. This page says what it does, how to use it, how to configure it, how it is built, which
rules the tests enforce on it, and what it costs. It describes the code as it is today. The bar's
resources module itself (the figures on the bar, and the code that reads `/proc` and `/sys`) is not
described here, except for what the popup depends on.

## What it does

A left click on the resources module opens a popup under it:

- A header row: `Resources` on the left, and on the right, dim, the span the graphs cover
  (`last 2 min`).
- One block per item of `[resources] show`, in that order, whether or not the item has a figure
  right now. A block is a text row and a graph two rows tall:
  - the item's icon in its hue (CPU orange, Memory yellow, Temperature red, Network cyan, Disk
    blue, GPU purple, as on the bar), its name, and its current figure on the right:
    CPU `12%`; Memory `6.1G` then, dim, ` of 15.5G`; Temperature and GPU `54°`, in the error
    colour from 85 °C as on the bar; Network `󰇚 1.2M 󰕒 80K` (download then upload), preceded by,
    dim, `peak 3.4M` (the graph's top); Disk `62%`; and a dim `—` when there is no figure;
  - a graph under it: the last 60 samples, the newest at the right edge, drawn in pixels by the
    painter in the theme's colours. A filled graph is the item's hue (a lighter body, a full-colour
    top edge); the network's upload is a dim line over its download's fill. A point with no reading
    is a gap.
- A separator, then `Monitor…`, which runs `[actions] resources`. With that setting empty the
  popup has no `Monitor…` row and no separator.

The graphs' scales: CPU and disk 0 to 100 %; memory 0 to the machine's total; temperature and GPU
0 to 100 °C (clamped); network one scale for both directions, the highest rate of either in the
ring, never below 128 KiB/s so that an idle link does not fill it with noise.

The popup keeps its size while it is open, and it repaints on each tick of the bar's resources
timer, and only while it is open.

## Using it

- **Left click on the module** opens the popup; a second left click on the module closes it.
- **The right click** on the module runs `[actions] resources` directly (`foot -e btop` by default);
  it was the left click's job before the popup existed.
- **Keys:** nothing is selected when the popup opens. ↓, ↑, Tab, Shift+Tab, Home and End select
  `Monitor…`; Enter or Space runs it once selected; Esc closes. Every other key does nothing.
- **Mouse:** hovering `Monitor…` selects it, and a left press on it runs it. The wheel and the
  other buttons do nothing.
- **Esc**, a click outside, a restyle that moves the bar and the output going away close the popup,
  as for every module popup. Running `Monitor…` closes it too. A theme change repaints it.

The popup is opened with a grab (`Input::Grab`). The README notes what that means for the bar's
keyboard on Hyprland.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar).

| Key | Default | Meaning |
| --- | --- | --- |
| `[resources] show` | `["cpu", "memory", "temperature"]` | the items, in order: also `net`, `disk`, `gpu`; one block each in the popup |
| `[resources] interval` | `2` | seconds between two readings, 1 to 60; the graphs span 60 of them, so `last N min` where N is the interval |
| `[actions] resources` | `["foot", "-e", "btop"]` | the command of `Monitor…` and of the right click, run as written (no shell); `[]` removes the row |

```toml
[resources]
show = ["cpu", "memory", "temperature", "net"]
```

The history is kept whenever the `resources` module is on, so the popup opens on a graph that is
already full. It has no setting of its own.

## How it is built

Two files in `crates/alpheus-daemon/src/resources/`, and the figures they read in `system.rs`:

- `mod.rs`: the pure model. `ResourcesPopup` (whether `Monitor…` is selected and whether it exists),
  `ResourcesAction` (what a key or the pointer asks of the wiring: nothing, redraw, run the
  monitor, close), `series` (an item's history as points in permille), `net_top` (the network
  graph's top), `figure` (an item's current text) and `render`, which returns the rows and the
  graphs' descriptions (`list::Boxed`). The tests are in `tests.rs`.
- `app.rs`: the wiring. `OPS` is the popup's row in the table of popups (`popup::POPUPS`).
  `resources_toggle` opens it, `resources_key` and `resources_pointer` hand input to the model,
  `resources_action` closes the popup or runs `Monitor…` through the daemon's executor
  (`spawn_monitor`), and `resources_tick` is the bar's resources timer's callback: it applies the
  reading to the bar, then repaints the popup when it is the open one.
- `system::Resources` holds the latest `Sample` and a `History`, a ring of the last 60 samples
  (about 3 KB, inline, written in place with no allocation). This folder does not read the system:
  `sources/resources.rs` does that, and the bar's own text comes from the same samples.
- The graphs are drawn by the painter (`painter::plot`): each sample is a column whose edges are
  whole pixels, so the columns tile the graph exactly at every scale, with no anti-aliasing.

**The path of a click.** The bar's click handler finds the module's zone: a left press calls the
popup table's `toggle`, which is `resources_toggle`; a right press runs `[actions] resources`. Inside
the popup a key reaches `resources_key` and the pointer `resources_pointer`; a left press on
`Monitor…`, or Enter on it, spawns the command, logs `resources: monitor` (never the command, which
can name the user; `resources: monitor could not start (<error>)` on a failed start) and closes the
popup.

**The path of a tick.** The timer's callback in `startup.rs` calls `resources_tick` with the new
reading. The popup owns no timer: opening it adds no wake-up.

## Rules it keeps

- `the_tray_spawns_no_thread`: `resources/mod.rs` starts no thread, process or command.
- `the_resources_popup_has_no_panicking_path`: neither file's production code panics.
- `a_resources_tick_repaints_only_an_open_popup_after_applying_the_reading`: a tick applies the
  reading before it repaints, repaints only while the popup is open, and the timer goes through
  `resources_tick`, never around it.
- `the_history_stays_small` (in `system.rs`) pins the ring at 3 KiB; `resources_history_full_keeps_nothing_more`
  bounds the growth of the daemon's anonymous memory at 64 KB between 30 s and 70 s, with the ring
  full and wrapped.
- The headless tests (`resources_a_click_opens_the_popup_with_graphs`,
  `resources_down_enter_runs_the_monitor`, `resources_a_left_press_on_the_monitor_row_runs_it` and
  `resources_escape_closes_and_right_click_runs_the_monitor`) pin how it opens, draws, runs and
  closes, against a fake monitor that records its calls; no test runs a real `btop`.
- `at_rest_no_wakeups_after_the_resources_popup`: after a close, the daemon has no wake-up at rest.
- The reasons are in [decisions.md](../decisions.md): "The resources history is kept always, as whole
  samples", "The resources popup spans 60 times the interval", "Every resources item keeps its
  block in the popup", "The resources graphs have fixed scales, and the network's has a floor",
  "The resources popup has no timer of its own", "The left click opens the resources popup and the
  right click runs `[actions] resources`", "Logs carry fixed words and numbers only".

## What it costs

Measured in release, back to back, 3 runs (the README's memory table has the figures and the
method):

- **Linked in:** within noise of the build before it (binary +24 KiB stripped).
- **The history:** about 3 KB, below what PSS can show; `Pss_Anon` does not move between 30 s and
  70 s with the ring full.
- **Popup open:** about 530 KB over rest; once closed, +40 to +44 KB of anonymous memory over rest;
  twenty more cycles +44 to +60 KB (`resources_popup_pss_idle_open_and_closed`).
- **At rest:** 0 loop wake-ups after a close; the bar's own tick is unchanged.
