# Resources: GPU usage and VRAM, several disks, a named interface

Date: 2026-11-27. Draft: cluster 05 of the feature round. Decisions applied: OWNER-DECISIONS 3
(NVIDIA proprietary shows `—`) and 4 (history ring bound 3 KiB → 4 KiB, its own task and commit);
every other open question settled by the draft's recommendation.

## Intent

Three of the four asks half-exist: `net`, `disk` and `gpu` are items of `[resources] show`, off by
default, read on the module's one timer (`sources/resources.rs`, `startup.rs`). What is missing is
GPU **utilisation and VRAM** (today `gpu` is the hwmon temperature only), **several disks** (today
`$HOME`'s filesystem only) and a **chosen interface** (today the default route's). This design is
those three deltas, read from `/proc` and `/sys` on the same tick.

## Goals

- `gpu` shows utilisation (`%`) when the driver offers it without root: amdgpu exactly, Intel
  i915/xe as an idle-residency estimate labelled `active` in the popup; the temperature as today
  when there is no usage source, or beside it on request; VRAM use in the popup (amdgpu).
- `disk` can follow up to four paths; the bar shows the fullest, the popup one block per path.
- Disks are read on a slower cadence than the other items (default every 15 s).
- `net` can follow a named interface instead of the default route.

## Non-goals

- Per-process GPU use (`fdinfo`), engine busy through the perf PMU, debugfs: root or paranoid-gated.
- NVIDIA proprietary usage: no `nvidia-smi` child, no NVML (decision 3). It shows `—`, documented.
- A sum of all interfaces; a resources tab in the control centre (draft Q7, Q8).
- Alerts, thresholds, a disk timeout (it would need a thread).

## Anti-goals

- No new timer, thread, watch, process or dependency. Every new reading is a branch of
  `Reader::sample_at`, guarded by `shows(item)`.
- Never read what is not shown; never wake a runtime-suspended GPU.
- Never log a mount path, an interface name or a card path.

## Constraints

- **Lightness:** an item left out costs zero reads and zero state, as today. The ring stays inline
  and fixed: `Sample` grows from 48 to 56 bytes, the ring from about 2.9 to 3.3 KB, inside the
  4 KiB bound the owner approved.
- **Session safety:** read-only files under `/sys` and `/proc`; tests use fixture trees and the
  injectable `StatFs`; nothing calls a session tool.
- **Privacy:** fixtures anonymised (no real serials, hostnames, mount names); the config's error
  messages name the key and the entry's position, never the path or the interface.
- **Compositors:** nothing here touches the compositor; identical on Mango (default) and Sway.
- **Hung network mounts:** `statvfs` on the loop blocks the bar if the mount hangs, as it already
  can for `$HOME`; a list makes it likelier. Documented on the key; the slower cadence narrows it.

## Approaches considered

1. **More branches in the existing reader (picked).** Each source is one or two small file reads in
   `sample_at`; state lives in `Reader`. Zero new wake-ups, testable with fixture trees exactly like
   the sensors today.
2. **A second, slow timer for the disks.** Cleaner cadence, but a calloop source and a wake-up for
   what an elapsed-time check inside the existing tick does for free. Rejected.
3. **An optional helper (`nvidia-smi`, `intel_gpu_top`) per vendor.** Covers NVIDIA and gives true
   engine busy, at the cost of a resident child, a parser and a privilege story. Rejected by
   decision 3 and the "no command" rule of the reader.

## The design

### What the user sees

- **Bar, `gpu`:** `󰢮 23%` when a usage source exists; `󰢮 54°` without one (today's look); with
  `gpu_metric = "both"`, `󰢮 23% 54°` (the degrees in the error colour from 85 °C, as today).
  Nothing when neither is read.
- **Popup, GPU block:** the figure is the same, preceded by a dim `vram 41% ` on amdgpu and a dim
  `active ` before the percent when it is the Intel estimate. The graph plots usage (0 to 100 %) when
  any sample of the ring has a usage reading, else the temperature as today.
- **Bar, `disk`:** the fullest of the listed paths, `󰋊 87%`.
- **Popup, disks:** one block per listed path, in order, named by the path (left-truncated with `…`
  to fit); with no list, the single block keeps its name `Disk`. A path with no reading shows `—`.
- **`net`:** unchanged look; follows `net_interface` when set (`—` while that interface is absent).
- **NVIDIA proprietary:** no hwmon, no usage file: the GPU figure is `—` in the popup, absent on the
  bar. Documented on the key and the feature page.

### Configuration (`[resources]`, all additive)

| Key | Default | Bounds | Meaning |
| --- | --- | --- | --- |
| `gpu_metric` | `"auto"` | `auto`, `usage`, `temp`, `both` | `auto`: usage when the card offers it, else the temperature; only what it names is read |
| `gpu_sensor` | `""` | | existing; also selects the card by driver name (`amdgpu`, `i915`, `xe`) for usage |
| `disk_mounts` | `[]` | at most 4, absolute, no repeat | paths whose filesystems `disk` measures; `[]` = `$HOME`, as today |
| `disk_interval` | `15` | 1 to 300 seconds | the disks are read on the first pass, then once this long has passed; between reads the last figures repeat |
| `net_interface` | `""` | ≤ 15 bytes, no `/`, `:` or whitespace | `""` = the IPv4 default route (today); else that exact name |

`disk_interval = 15` makes the disk figure update less often than today (every pass); the draft's
recommendation, applied.

### How it is built

- `config.rs` (daemon): `ResourcesConfig` gains `gpu_metric: GpuMetric`, `disk_mounts:
  Vec<PathBuf>`, `disk_interval: u64`, `net_interface: String`; `GpuMetric { Auto, Usage, Temp,
  Both }` (serde lowercase); validation in `BarConfig::from_text`; header doc comment updated.
- `system.rs`: `Sample` gains `gpu_busy: Option<GpuBusy>` (`GpuBusy { percent: u8, idle_based:
  bool }`, 2 bytes with the niche) and `vram: Option<u8>`; `disk: Option<u8>` becomes `disks:
  [Option<u8>; DISKS]` with `DISKS = 4`. `the_history_stays_small` reads `4 * 1024`.
- `sources/resources.rs`:
  - net: `Reader::net` skips `/proc/net/route` when `net_interface` is set.
  - disks: `Reader { disk_paths: Vec<PathBuf>, disks: [Option<u8>; DISKS], disks_at:
    Option<Instant>, disk_interval: Duration }`; `disk_due(now) -> bool`.
  - GPU: `GpuDriver { Amdgpu, I915, Xe }`, `Card { dir, driver }`, `pick_card(root, hint) ->
    Option<Card>` over `sys/class/drm/card<N>` by `device/driver` link name (evdi, nvidia and
    every other driver never win), `runtime_suspended(&Card)`, `read_amdgpu(&Card) -> (Option<u8>,
    Option<u8>)`, `idle_ms(&Card) -> Option<u64>`, `busy_from_idle(prev, now) -> Option<u8>`; a
    `GpuUsage` state in `Reader` that keeps the previous idle reading and re-picks a vanished card
    on the existing 30-tick throttle. `Reader::new` decides once what `gpu_metric` reads.
- `segments.rs`: `resources()` draws the fullest disk and the GPU per what the sample holds.
- `resources/mod.rs`: `blocks(c) -> Vec<(ResourceItem, usize)>` (the disk index, 0 otherwise)
  drives `render`, `monitor_row`, `series` and `figure`; `mount_label(path, width)`.

## Cost estimate

Per pass, shown items only: GPU usage one or two small sysfs reads (amdgpu busy, plus the two VRAM
files; Intel one idle file) and one `runtime_status` read; disks N `statvfs` every
`disk_interval` instead of one each pass; a named interface one read fewer. Ring +480 B, below PSS
resolution. Binary +~20 KiB. Wake-ups and threads: 0 added. Disabled: unchanged from today.

## Tests

Fixture trees under the temp dir, anonymised, built like `gpu_tree` and `workstation`:
`gpu_usage_reads_amdgpu_busy_and_vram`, `gpu_usage_from_rc6_is_one_minus_the_idle_share`,
`gpu_nvidia_proprietary_gives_nothing_and_no_panic`, `a_monitor_only_card_never_wins`,
`a_suspended_gpu_is_not_read`, `gpu_metric_temp_reads_no_usage_file`,
`disk_mounts_statvfs_only_the_listed_paths`, `disks_are_read_once_per_disk_interval`,
`net_interface_named_skips_the_route_read`, and `nothing_is_read_for_an_item_not_in_show` extended.
Display: segment and popup tests for the fullest disk, one block per mount, `active` and `vram`.
Guardrails: `sources/resources.rs` joins the no-thread/no-process list;
`the_resources_reader_never_logs_a_mount_an_interface_or_a_card`; `the_history_stays_small` at
4 KiB in its own commit; `resources_every_item_ticks_at_the_same_rate` unchanged. Memory: a new
`resources_pss_every_key` against main, back to back, each task.

## Left to the builder

- The exact fixture of the xe tree (`device/tile0/gt0/gtidle/idle_residency_ms`) is taken from the
  kernel's documentation, not seen on hardware here: the xe path is the last task and may be dropped
  if it cannot be checked.
- The wording of the docs and of the `decisions.md` entries.
- Whether `mount_label` shares code with an existing truncation helper.
