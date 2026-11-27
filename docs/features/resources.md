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
  right now, except `disk` with `disk_mounts` set: one block per listed path, in order, named by the
  path (cut from the left with `…` to fit, so its end stays). A block is a text row and a graph two
  rows tall:
  - the item's icon in its hue (CPU orange, Memory yellow, Temperature red, Network cyan, Disk
    blue, GPU purple, as on the bar), its name, and its current figure on the right:
    CPU `12%`; Memory `6.1G` then, dim, ` of 15.5G`; Temperature `54°`, in the error colour from
    85 °C as on the bar; GPU its usage `23%`, preceded by a dim `vram 41% ` on amdgpu or a dim `active ` on Intel, and/or
    `54°` as the temperature (both: `23% 54°`, per `gpu_metric`); Network `󰇚 1.2M 󰕒 80K` (download then upload), preceded by,
    dim, `peak 3.4M` (the graph's top); Disk `62%`; and a dim `—` when there is no figure;
  - a graph under it: the last 60 samples, the newest at the right edge, drawn in pixels by the
    painter in the theme's colours. A filled graph has a body at 40 % of the item's hue over the
    popup's background and a full-colour top edge, over a baseline in the border colour; the
    network's upload is a dim line over its download's fill. A point with no reading is a gap.
- A separator, then `Monitor…`, which runs `[actions] resources`. With that setting empty the
  popup has no `Monitor…` row and no separator.

The graphs' scales: CPU and disk 0 to 100 %; memory 0 to the machine's total; temperature 0 to 100 °C
(clamped); GPU 0 to 100 % of usage when any sample of the ring has a usage reading, else 0 to
100 °C like the temperature; network one scale for both directions, the highest rate of either in the
ring, never below 128 KiB/s so that an idle link does not fill it with noise.

On the bar, `gpu` shows `󰢮 23%` (usage), `󰢮 54°` (temperature), or `󰢮 23% 54°` with
`gpu_metric = "both"` (the degrees in the error colour from 85 °C). Usage comes from the card's
sysfs files: amdgpu's `gpu_busy_percent` and VRAM figures, or on Intel an estimate from idle
residency (i915's RC6 residency, `gt/gt0/rc6_residency_ms` or `power/rc6_residency_ms`; xe's
`tile0/gt0/gtidle/idle_residency_ms`, from the kernel's driver, not checked on hardware): 100 minus
the share of time the GPU spent idle since the previous reading, labelled `active` in the popup
because a GPU out of its idle state is not necessarily loaded (no engine load without root). The
card is picked by its driver link
among the cards driving the panel (`boot_vga`; another only when `gpu_sensor` names it). Reading a
usage file marks the card busy and restarts its autosuspend timer, so: a card that is not awake is
not read at all, and an awake one at most once per its autosuspend delay and a second (about every
11 s on Intel, 6 s on amdgpu), the last figures repeating in between, so the card can still fall
asleep; in `both` the temperature comes from that same card's own hwmon and follows the same
rule (docs/decisions.md, "A GPU's usage is read without keeping it awake"). NVIDIA's proprietary driver offers no usage file and no hwmon: its GPU shows `—` in the popup
and nothing on the bar (no `nvidia-smi`, no NVML: alpheus runs no command for its figures).

On the bar, `disk` shows the fullest of the listed paths (`󰋊 87%`). The disks are read every
`disk_interval` seconds (15 by default), not every pass; in between, each pass repeats the last
figures, so the graph has no gaps.

The popup keeps its size while it is open, and it repaints on each tick of the bar's resources
timer, and only while it is open.

## Using it

- **Left click on the module** opens the popup; a second left click on the module closes it.
- **The right click** on the module runs `[actions] resources` directly (`foot -e btop` by default);
  it was the left click's job before the popup existed. That path goes through `actions::run`, which
  logs `action: could not start (<kind word>)` when the start fails, never the argv (it can name the
  user).
- **Keys:** nothing is selected when the popup opens. ↓, ↑, Tab, Shift+Tab, Home and End select
  `Monitor…`; Enter or Space runs it once selected; Esc closes. Every other key does nothing.
- **Mouse:** hovering `Monitor…` selects it, and a left press on it runs it. The wheel and the
  other buttons do nothing.
- **Esc**, a click outside, a restyle that moves the bar and the output going away close the popup,
  as for every module popup. Running `Monitor…` closes it too. A theme change repaints it.

The popup is opened with a grab (`Input::Grab`). [The bar page](bar.md) notes what that means for the bar's
keyboard on Hyprland.

## Configuration

In `bar.toml`, read at start (a change needs a restart of the bar).

| Key | Default | Meaning |
| --- | --- | --- |
| `[resources] show` | `["cpu", "memory", "temperature"]` | the items, in order: also `net`, `disk`, `gpu`; one block each in the popup |
| `[resources] gpu_metric` | `"auto"` | `usage`, `temp` or `both`; `auto`: usage when a card offering it is found at start, else the temperature; only what it names is read |
| `[resources] gpu_sensor` | `""` | the GPU's hwmon name, and the driver (`amdgpu`, `i915`, `xe`) of the card read for usage; `""`: automatic (an `evdi` or NVIDIA card never wins) |
| `[resources] net_interface` | `""` | the interface `net` follows (`wlan0`, `enp3s0`…); `""`: the IPv4 default route's; absent, `net` shows no figure |
| `[resources] disk_mounts` | `[]` | up to 4 absolute paths `disk` measures, one block each; `[]`: `$HOME`'s filesystem, one block named `Disk`. A path on a network filesystem (`nfs`, `cifs`, `sshfs`, any FUSE type not known to be local…) or an automount point (`autofs`), found once at start in `/proc/self/mountinfo`, is never read and shows `—`: `statvfs` runs on the event loop, where a hung server would stall the bar and an automount would mount its share; a local automount (systemd's `/boot`) shows `—` too |
| `[resources] disk_interval` | `15` | seconds between two readings of the disks, 1 to 300; the last figures repeat in between |
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
  monitor, close), `blocks` (the popup's blocks in order: one per item, one per `disk_mounts` path
  for `disk`, each with its index into `Sample::disks`; `render`, its graphs and `monitor_row` all
  count these), `mount_label` (a path cut from the left), `series` (one block's history as points
  in permille), `net_top` (the network graph's top), `figure` (one block's current text) and `render`, which returns the rows and the
  graphs' descriptions (`list::Boxed`). The tests are in `tests.rs`.
- `app.rs`: the wiring. `OPS` is the popup's row in the table of popups (`popup::POPUPS`).
  `resources_toggle` opens it, `resources_key` and `resources_pointer` hand input to the model,
  `resources_action` closes the popup or runs `Monitor…` through the daemon's executor
  (`spawn_monitor`), and `resources_tick` is what the bar's resources timer's callback (in
  `startup.rs`) calls with each reading: it applies the reading to the bar, then repaints the
  popup when it is the open one.
- `system::Resources` holds the latest `Sample` and a `History`, a ring of the last 60 samples
  (about 3.3 KB, inline, written in place with no allocation). This folder does not read the system:
  `sources/resources.rs` does that, and the bar's own text comes from the same samples.
- The graphs are drawn by the painter (`painter::plot`): each sample is a column whose edges are
  whole pixels, so the columns tile the graph exactly at every scale, with no anti-aliasing.

**The path of a click.** The bar's click handler finds the module's zone: a left press calls the
popup table's `toggle`, which is `resources_toggle`; a right press runs `[actions] resources`. Inside
the popup a key reaches `resources_key` and the pointer `resources_pointer`; a left press on
`Monitor…`, or Enter on it, spawns the command, logs `resources: monitor` (never the command, which
can name the user; `resources: monitor could not start (<kind word>)` on a failed start) and closes
the popup. The right click is not that path: `actions::run` logs `action: could not start (<kind
word>)` (see "Using it").

**The path of a tick.** The timer's callback in `startup.rs` calls `resources_tick` with the new
reading. The popup owns no timer: opening it adds no wake-up.

## Rules it keeps

- `the_tray_spawns_no_thread`: `resources/mod.rs` and `sources/resources.rs` start no thread,
  process or command.
- `the_resources_reader_never_logs_a_mount_an_interface_or_a_card`: no log line of the reader names
  a disk path, a mount, an interface or a GPU card.
- `a_suspended_gpu_is_not_read` and `only_an_active_card_is_read`: a card that is not awake gives no
  figure, its busy file unread; `usage_is_read_once_per_autosuspend_delay_and_a_second`,
  `both_skips_the_sensor_of_a_sleeping_card`, `a_wake_reads_the_autosuspend_delay_again`,
  `temp_spaces_the_hwmon_of_a_named_secondary_card` and
  `temp_spaces_a_named_secondary_card_that_appears_later`:
  the reads never keep the card from falling asleep; `auto_picks_only_the_boot_vga_card`;
  `a_monitor_only_card_never_wins` and `gpu_nvidia_proprietary_gives_nothing_and_no_panic`: an
  `evdi` or NVIDIA card is never picked for usage; `both_reads_the_hwmon_of_the_gated_card`: the
  temperature in `both` is the gated card's own, never another card of its driver.
- `a_disk_mount_on_a_network_filesystem_is_never_statvfs_ed`: a listed path on a network filesystem
  is never asked, so a hung server cannot block the loop; `a_network_mount_is_found_by_its_longest_mount_point`:
  an automount point and an unknown FUSE type count as remote.
- `gpu_usage_from_rc6_is_one_minus_the_idle_share` and `a_suspension_drops_the_idle_baseline`: the
  Intel estimate is 100 minus the idle share between two readings, never across a sleep;
  `the_intel_estimate_reads_active`: the popup says so.
- `the_resources_popup_has_no_panicking_path`: neither file's production code panics.
- `a_resources_tick_repaints_only_an_open_popup_after_applying_the_reading`: a tick applies the
  reading before it repaints, repaints only while the popup is open, and the timer goes through
  `resources_tick`, never around it.
- `the_history_stays_small` (in `system.rs`) pins the ring at 4 KiB; `resources_history_full_keeps_nothing_more`
  bounds the growth of the daemon's anonymous memory at 64 KB between 30 s and 70 s, with the ring
  full and wrapped.
- The headless tests (`resources_a_click_opens_the_popup_with_graphs`,
  `resources_down_enter_runs_the_monitor`, `resources_a_left_press_on_the_monitor_row_runs_it` and
  `resources_escape_closes_and_right_click_runs_the_monitor`) pin how it opens, draws, runs and
  closes, against a fake monitor that records its calls; no test runs a real `btop`.
- `at_rest_no_wakeups_after_the_resources_popup`: after a close, the daemon has no wake-up at rest.
- The reasons are in [decisions.md](../decisions.md): "The resources history is kept always, as whole
  samples", "The resources popup spans 60 times the interval", "Every resources item keeps its
  block in the popup", "Each resources graph has a set scale; the network's follows its peak, with a
  floor", "The bar shows the fullest disk, the popup one block per path", "Disks are read every
  `disk_interval` seconds, the last figures repeat between reads", "GPU usage comes from sysfs only;
  NVIDIA proprietary shows —", "A GPU's usage is read without keeping it awake", "Intel GPU usage is an idle-residency
  estimate, labelled active", "The resources popup has no timer of its own", "The left click opens the resources popup and the
  right click runs `[actions] resources`", "Logs carry fixed words and numbers only".

## What it costs

Measured in release, back to back, 3 runs (the [memory tables](../measurements.md) have the figures and the
method):

- **Linked in:** within noise of the build before it (binary +24 KiB stripped).
- **The history:** about 3.3 KB, below what PSS can show; `Pss_Anon` does not move between 30 s and
  70 s with the ring full.
- **Popup open:** +532 to +544 KB over rest; once closed, +40 to +44 KB of anonymous memory over rest;
  twenty more cycles +44 to +60 KB (`resources_popup_pss_idle_open_and_closed`).
- **At rest:** 0 loop wake-ups after a close; the bar's own tick is unchanged.
