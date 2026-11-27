# Resources GPU usage, disks and interface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the existing `gpu`, `disk` and `net` resources items GPU utilisation and VRAM,
up to four disk paths read on a slower cadence, and a named network interface, with no new timer,
thread, process or dependency.

**Architecture:** More branches in `sources/resources.rs`'s `Reader::sample_at`, each guarded by
`shows(item)`, state kept in `Reader`; `Sample` grows by 8 bytes (ring about 3.3 KB, bound raised to
4 KiB on its own); the bar segment and the popup read what the sample holds. Nothing touches the
compositor.

**Tech Stack:** Rust, `/proc` and `/sys` text files, `statvfs` (`sys::statvfs`), the daemon's
`Grid`, `Boxed` and painter. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-27-resources-gpu-design.md`.

## Global Constraints

- Every cargo command runs in `nix develop -c …`; heavy suites (headless, memory) at `nice -n 19`.
- Tests first, each failing for the right reason, then mutated to prove it bites (say the mutation
  and the red run in the report). Fixture trees anonymised (no real serials, hostnames, user names,
  mount names: `/fictional/...`).
- No new dependency, thread, timer, watch or process; every new read sits inside `sample_at`,
  behind `shows(item)`. An item left out reads nothing and keeps no state.
- Never wake a runtime-suspended GPU: `device/power/runtime_status` is read before any usage file.
- Log lines are fixed words: never a mount path, an interface name, a card path. Config errors name
  the key and the entry's position, never its value.
- Never call a session tool; never run `alpheus`/`alpheus-demon` on the session; headless and memory
  tests only through the isolated harness; `pgrep` afterwards; kill only PIDs you started.
- Never loosen a guardrail. The one approved change is `the_history_stays_small` 3 KiB → 4 KiB
  (OWNER-DECISIONS 4), alone in Task 2.
- NVIDIA proprietary shows `—`: no `nvidia-smi`, no NVML (OWNER-DECISIONS 3).
- One logical change per commit, `type(scope): …`, body says why, the session's trailers; stage by
  name, read `git diff --cached` before each commit; never `flake.lock`. Work only on
  `feat/resources-gpu` in `.worktrees/resources-gpu`.
- Everything in English. `cargo fmt` on touched files, `clippy --workspace --all-targets -D warnings`,
  `cargo test --workspace` green at each commit.
- **Every task ends with a RAM measurement of each part it touches, recorded in
  `docs/measurements.md`.** Here the part is the daemon: `tests/memory.rs` in release, back to back
  against `main` built in its own target directory (`docs/measurements.md` gives the method), 3 runs.
  A task with no measurement is not done.

## Review Focus

1. **A suspended discrete GPU woken by the bar** (hybrid laptop, amdgpu in runtime suspend): the
   busy file must not be opened → Task 4 (`a_suspended_gpu_is_not_read`: the busy file holds a
   real figure while the card is suspended, so only the guard keeps it out of the sample).
2. **A monitor-only or NVIDIA card picked by auto** (DisplayLink evdi, proprietary `nvidia`):
   usage comes from the iGPU or nowhere → Task 4 (`a_monitor_only_card_never_wins`,
   `gpu_nvidia_proprietary_gives_nothing_and_no_panic`).
3. **Gaps in the disk graph between slow reads**: the last figures must repeat each pass, not turn
   into `None` → Task 3 (`disks_are_read_once_per_disk_interval`).
4. **The popup's rows drift from its graphs or from `Monitor…`** once disks give several blocks:
   `render`, `graphs[]` and `monitor_row` must count the same blocks → Task 3
   (`one_block_per_mount_and_monitor_row_follows`).
5. **A private value in a log or error** (mount path, interface name): → Task 1
   (`net_interface_rejects_a_bad_name_without_naming_it`), Task 3
   (`disk_mounts_rejects_bad_entries_without_naming_them`), Task 4
   (`the_resources_reader_never_logs_a_mount_an_interface_or_a_card`).

---

### Task 1: `net_interface`, a named interface instead of the default route

**Files:** `crates/alpheus-daemon/src/config.rs` (`ResourcesConfig`, validation in
`BarConfig::from_text`, header doc comment, tests near `actions_and_resources_defaults_and_settings`),
`crates/alpheus-daemon/src/sources/resources.rs` (`Reader`, `Reader::net`, tests),
`crates/alpheus-daemon/tests/memory.rs`, `docs/configuration.md`, `docs/features/resources.md`,
`docs/measurements.md`.

**Interfaces — Produces:** `ResourcesConfig { net_interface: String /* default "" */, .. }`;
`pub const NET_INTERFACE_MAX: usize = 15;` (config.rs); `Reader { net_interface: Option<String>, .. }`
(None when empty). **Consumes:** `default_route`, `net_counters`, `net_rate` unchanged.

- [ ] Failing tests (config.rs): `net_interface_defaults_empty_and_parses` (default `""`;
  `net_interface = "wlan0"` → `"wlan0"`); `net_interface_rejects_a_bad_name_without_naming_it`
  (16 bytes, `"a/b"`, `"a:b"`, `"a b"` each → `Err`, and the message contains
  `resources.net_interface` and does not contain the value).
- [ ] Failing tests (resources.rs): `net_interface_named_skips_the_route_read` (fixture tree with
  `proc/net/dev` naming `fict0` and `fict1`, and `proc/net/route` replaced by a **directory** so a
  read of it returns `Err`; config `net_interface = "fict1"`, `show = [Net]`; two `sample_at` calls
  1 s apart → `net == Some(NetRate { down, up })` from `fict1`'s counters);
  `net_interface_absent_gives_no_figure` (`"fict9"` → `net == None` on both passes, no panic).
- [ ] Run `nix develop -c cargo test -p alpheus-daemon net_interface` → fails to compile (no field).
- [ ] Implement: the field with `#[serde(default)]` via `ResourcesConfig::default`; validation
  `if n.len() > NET_INTERFACE_MAX || n.contains(['/', ':']) || n.chars().any(char::is_whitespace)`
  → `Err("resources.net_interface: not an interface name (at most 15 bytes, no '/', ':' or space)")`;
  `Reader::new` keeps `Some(name)` when non-empty; `fn net(&mut self, now)` takes the named
  interface first and reads `proc/net/route` only when it is None.
- [ ] Mutation: make `net` read the route even when named → `net_interface_named_skips_the_route_read`
  red (the route read fails, no figure). Restore.
- [ ] Measurement: add `resources_pss_every_key` to `tests/memory.rs` (two fresh daemons:
  `EVERY_ITEM` versus `EVERY_ITEM` plus `EVERY_KEY = "net_interface = \"lo\"\n"`; asserts
  `every_key <= every_item + 128` KB and own memory after ~60 passes `<= settled + 64` KB, shm pools
  left out, like `resources_pss_default_and_every_item`). Run it and
  `resources_every_item_ticks_at_the_same_rate` on this branch and on `main` (the main side runs
  `resources_pss_default_and_every_item`'s six-item figure), release, back to back, 3 runs; add a
  row to `docs/measurements.md`.
- [ ] Docs: `docs/configuration.md` `[resources]` row for `net_interface`; the `show` row's `net`
  text ("the IPv4 default route's interface, or `net_interface`"); `docs/features/resources.md`
  Configuration table; config.rs header comment.
- [ ] Commit `feat(resources): net follows a named interface when set`.

### Task 2: The history ring bound, 3 KiB → 4 KiB

**Files:** `crates/alpheus-daemon/src/system.rs` (`the_history_stays_small`, module doc line 12),
`crates/alpheus-daemon/src/sources/resources.rs` (module doc "about 3 KB"),
`docs/features/resources.md` ("Rules it keeps", "How it is built"), `docs/decisions.md` ("The
resources history is kept always, as whole samples": the bound and the owner's approval, dated
2026-11-27), `docs/measurements.md`.

**Interfaces:** none changed; `size_of::<History>() <= 4 * 1024`.

- [ ] The test change is the task: `the_history_stays_small` asserts `<= 4 * 1024`. Before editing,
  add a temporary local check (not committed) that a `History` of a 56-byte `Sample`
  (`[u8; 56]`-padded stand-in) reads 3,376 bytes, above 3,072: the reason the bound must move.
- [ ] Mutation: set the bound to `size_of::<History>() - 1` → red; restore to `4 * 1024`.
- [ ] Docs: the decision entry says the owner approved 4 KiB on 2026-11-27 for the GPU, VRAM and
  four-disk fields (+8 B per sample, ring about 3.3 KB); the feature page's two "3 KiB/3 KB" mentions.
- [ ] Measurement: `resources_history_full_keeps_nothing_more` on this branch and `main`, release,
  back to back, 3 runs (expected identical: no code moved); row in `docs/measurements.md`.
- [ ] Commit `test(resources): raise the history ring bound to 4 KiB` (body: the owner's decision,
  why the ring needs it, that nothing else is loosened).

### Task 3: Several disks and their slower cadence

**Files:** `crates/alpheus-daemon/src/config.rs`, `crates/alpheus-daemon/src/system.rs` (`Sample`,
`DISKS`), `crates/alpheus-daemon/src/sources/resources.rs`, `crates/alpheus-daemon/src/segments.rs`
(`resources`), `crates/alpheus-daemon/src/segments/tests.rs`,
`crates/alpheus-daemon/src/resources/mod.rs` (`blocks`, `mount_label`, `series`, `figure`, `render`,
`monitor_row`), `crates/alpheus-daemon/src/resources/tests.rs`, every reader of `Sample::disk`
(grep), `tests/memory.rs` (`EVERY_KEY`), docs.

**Interfaces — Produces:** `pub const DISKS: usize = 4;` (system.rs);
`Sample { disks: [Option<u8>; DISKS], .. }` replacing `disk`; `ResourcesConfig { disk_mounts:
Vec<PathBuf> /* [] */, disk_interval: u64 /* 15, 1..=300 */ }`; `Reader { disk_paths: Vec<PathBuf>,
disks: [Option<u8>; DISKS], disks_at: Option<Instant>, disk_interval: Duration }`;
`fn disk_due(&self, now: Instant) -> bool`; `pub fn blocks(c: &ResourcesConfig) -> Vec<(ResourceItem,
usize)>` (one `(Disk, i)` per path, `i` 0 for every other item; `[]` mounts give one `(Disk, 0)`);
`pub fn mount_label(path: &Path, width: u16) -> String`; `series(item, index, h)`,
`figure(item, index, r)`. **Consumes:** `Roots.home` (used when `disk_mounts` is empty),
`StatFs`, `disk_percent`.

- [ ] Failing tests (config.rs): `disk_keys_default_and_parse` (`[]`, `15`; a list of two and `60`
  parse); `disk_mounts_rejects_bad_entries_without_naming_them` (five entries, a relative
  `"fictional"`, a repeat, `disk_interval = 0` and `301` → `Err`; messages name
  `resources.disk_mounts` / `resources.disk_interval` and the entry's position, never the path).
- [ ] Failing tests (resources.rs): `disk_mounts_statfs_only_the_listed_paths` (an injected `StatFs`
  answering `/fictional/a` and `/fictional/b` and panicking on any other path, `$HOME` included;
  `disks == [Some(62), Some(10), None, None]`); `disks_are_read_once_per_disk_interval` (a counting
  `StatFs` through an `AtomicUsize`; `disk_interval = 15`; passes at t=0, 2, 14 s read once, the
  figures repeat (`Some` on every pass, never `None`); t=15 s reads again); `no_mounts_still_measures_home`
  (today's `the_disk_of_home_through_the_injected_statfs` against `disks[0]`); extend
  `nothing_is_read_for_an_item_not_in_show` with `disk_mounts` set and `refuse`.
- [ ] Failing tests (segments/tests.rs): `the_bar_shows_the_fullest_disk` (`[Some(40), Some(87),
  None, None]` → `󰋊 87%`; all `None` → no disk figure).
- [ ] Failing tests (resources/tests.rs): `one_block_per_mount_and_monitor_row_follows` (three
  mounts plus `cpu`: four blocks, the `Monitor…` row at `2 + 3 * 4`, one `Graph` per block at
  `2 + BLOCK * i`); `a_mount_block_is_named_by_its_path_left_truncated`
  (`mount_label("/fictional/very/long/path/to/a/mount", 12)` starts with `…` and ends with
  `a/mount`, width ≤ 12); `no_mounts_keeps_the_disk_block_name` (`Disk`).
- [ ] Run the tests → compile failures on the new names.
- [ ] Implement: config fields and validation (≤ `DISKS`, `is_absolute`, no repeat, interval
  range); `Reader::new` builds `disk_paths` (mounts, else `home`); in `sample_at`, when
  `shows(Disk)` and `disk_due(now)`, `statvfs` each path into `self.disks` and set `disks_at`;
  always copy `self.disks` into the sample. `segments::resources` takes the max of `disks`;
  `render`, the graph loop and `monitor_row` iterate `blocks(c)`; the disk block's name is
  `mount_label` of its path when `disk_mounts` is non-empty, else `Disk`.
- [ ] Mutation 1: set the disk figures to `None` between reads → `disks_are_read_once_per_disk_interval`
  red. Mutation 2: leave `monitor_row` on `c.show.len()` → `one_block_per_mount_and_monitor_row_follows`
  red. Restore both.
- [ ] Measurement: `EVERY_KEY` gains `disk_mounts = ["/", "/tmp"]`; `resources_pss_every_key`,
  `resources_history_full_keeps_nothing_more`, `resources_popup_pss_idle_open_and_closed` and
  `resources_every_item_ticks_at_the_same_rate`, this branch against `main`, release, back to back,
  3 runs; row in `docs/measurements.md` (the ring now about 3.3 KB).
- [ ] Docs: configuration rows `disk_mounts` (with the hung network mount warning) and
  `disk_interval`; the `show` row's `disk` text; feature page (What it does: one block per mount,
  bar shows the fullest; Configuration; How it is built: `blocks`); `docs/decisions.md` entries
  "The bar shows the fullest disk, the popup one block per path" and "Disks are read every
  `disk_interval` seconds, the last figures repeat between reads" (draft Q5, Q6).
- [ ] Commit `feat(resources): disk follows up to four paths on a slower cadence`.

### Task 4: GPU usage and VRAM on amdgpu, the card pick and the suspend guard

**Files:** `crates/alpheus-daemon/src/config.rs` (`GpuMetric`, `ResourcesConfig.gpu_metric`),
`crates/alpheus-daemon/src/system.rs` (`GpuBusy`, `Sample.gpu_busy`, `Sample.vram`),
`crates/alpheus-daemon/src/sources/resources.rs`, `segments.rs` + tests, `resources/mod.rs` + tests,
`crates/alpheus-daemon/tests/guardrails.rs`, `tests/memory.rs`, docs.

**Interfaces — Produces:** `pub enum GpuMetric { Auto, Usage, Temp, Both }` (serde lowercase,
default `Auto`); `pub struct GpuBusy { pub percent: u8, pub idle_based: bool }`;
`Sample { gpu_busy: Option<GpuBusy>, vram: Option<u8>, .. }`; in resources.rs:
`pub enum GpuDriver { Amdgpu, I915, Xe }`, `pub struct Card { pub dir: PathBuf, pub driver: GpuDriver }`
(`dir` = `sys/class/drm/card<N>`), `pub fn pick_card(root: &Path, hint: &str) -> Option<Card>`
(only `card` + digits entries; driver from the `device/driver` link's file name; with a hint, that
driver only; else preference `amdgpu`, `i915`, `xe`; any other driver never wins),
`pub fn runtime_suspended(card: &Card) -> bool` (`device/power/runtime_status` == `suspended`),
`pub fn read_amdgpu(card: &Card) -> (Option<u8>, Option<u8>)` (`device/gpu_busy_percent`;
`device/mem_info_vram_used` ÷ `_total`, rounded up), `struct GpuUsage { card: Option<Card>,
idle: Option<(u64, Instant)>, retry_in: u32 }` in `Reader { gpu_usage: Option<GpuUsage>, .. }`.
`Reader::new`: `Temp` → sensor only; `Usage` → usage only; `Both` → both; `Auto` → usage when
`pick_card` finds a card at start, else the sensor. **Consumes:** `Sensor`, `RETRY_EMPTY_EVERY_TICKS`.

- [ ] Failing tests (config.rs): `gpu_metric_parses_its_four_words` (default `Auto`; `"usage"`,
  `"temp"`, `"both"`; `"load"` → `Err`).
- [ ] Failing tests (resources.rs, fixture `drm_tree` with `device/driver` symlinks to
  `../fictional/drivers/<name>`): `gpu_usage_reads_amdgpu_busy_and_vram` (busy `37`, VRAM
  1 GiB of 8 GiB → `gpu_busy == Some(GpuBusy { percent: 37, idle_based: false })`, `vram == Some(13)`);
  `a_monitor_only_card_never_wins` (`card0` evdi, `card1` amdgpu → `card1`; evdi alone → None);
  `gpu_nvidia_proprietary_gives_nothing_and_no_panic` (`card0` nvidia, no hwmon → `gpu_busy`,
  `vram`, `gpu` all None over three passes); `a_suspended_gpu_is_not_read` (`runtime_status`
  `suspended`, `gpu_busy_percent` holds `37` → `gpu_busy == None` and `vram == None`; flip to
  `active` → `Some(37)`); `gpu_metric_temp_reads_no_usage_file` (`Temp`, the busy file holds `37`,
  hwmon present → `gpu == Some(..)`, `gpu_busy == None`);
  `gpu_metric_auto_prefers_usage_and_skips_the_sensor` (amdgpu with both → `gpu_busy` Some, `gpu`
  None); `a_vanished_card_is_picked_again_throttled` (remove the card dir, recreate it: no figure
  until the 30-tick retry, then Some).
- [ ] Failing tests (display): `the_bar_shows_gpu_usage_then_both` (segments: busy 23 → `󰢮 23%`;
  busy 23 and `gpu` 90 → `󰢮 23% 90°` with the degrees in `Style::ERROR`); popup
  `the_gpu_block_shows_vram_and_plots_usage` (`vram 41% ` dim before `23%`; `series(Gpu, 0, h)`
  plots usage permille when any sample has `gpu_busy`, else degrees as today).
- [ ] Failing guardrails (guardrails.rs): add `"sources/resources.rs"` to the list that
  `the_tray_spawns_no_thread` reads (no `thread::spawn`, no `Command`); new
  `the_resources_reader_never_logs_a_mount_an_interface_or_a_card` (every `log(` line of
  `sources/resources.rs`'s production code has no `{` placeholder naming `path`, `mount`, `iface`,
  `interface`, `card` or `dir`, camera-watch style; asserts at least one log line was found).
- [ ] Run → compile failures.
- [ ] Implement as the interfaces say. In `sample_at`, when `shows(Gpu)` and `gpu_usage` is Some:
  re-pick on a missing `dir` (throttled), `runtime_suspended` → None without reading further, else
  `read_amdgpu` for amdgpu (Intel returns None until Task 5). Segment and popup draw usage when
  Some, degrees when Some, both when both.
- [ ] Mutation 1: drop the `runtime_suspended` check → `a_suspended_gpu_is_not_read` red. Mutation 2:
  let `pick_card` accept any driver → `a_monitor_only_card_never_wins` red. Mutation 3: add a log
  line formatting the card's dir → the log guardrail red. Restore all.
- [ ] Measurement: `EVERY_KEY` gains `gpu_metric = "both"` (on the test machine, no amdgpu: the pick
  and the suspend read are what is measured); the four memory tests of Task 3, this branch against
  `main`, release, back to back, 3 runs; row in `docs/measurements.md`, with the stripped binary size.
- [ ] Docs: configuration rows `gpu_metric` and the extended `gpu_sensor` (selects the card for
  usage; NVIDIA proprietary shows `—`); feature page (bar and popup GPU figures, the graph's scale,
  the NVIDIA gap, Rules it keeps: the new guardrails); `docs/decisions.md` entry "GPU usage comes
  from sysfs only; NVIDIA proprietary shows —" (OWNER-DECISIONS 3).
- [ ] Commit `feat(resources): gpu shows usage and VRAM from amdgpu's sysfs`.

### Task 5: Intel usage as an idle-residency estimate (i915, then xe)

**Files:** `crates/alpheus-daemon/src/sources/resources.rs`, `resources/mod.rs` + tests, docs.

**Interfaces — Produces:** `pub fn idle_ms(card: &Card) -> Option<u64>` (i915:
`gt/gt0/rc6_residency_ms`, else `power/rc6_residency_ms`; xe:
`device/tile0/gt0/gtidle/idle_residency_ms`), `pub fn busy_from_idle(prev: (u64, Instant), now:
(u64, Instant)) -> Option<u8>` (`100 − Δidle·100 / Δt`, rounded, clamped to 0..=100; None when
Δt is 0 or the counter went back). **Consumes:** `GpuUsage.idle`, `Card`, `runtime_suspended`.

- [ ] Failing tests: `gpu_usage_from_rc6_is_one_minus_the_idle_share` (i915 fixture, rc6 1,000 then
  2,500 ms over 2 s → `Some(GpuBusy { percent: 25, idle_based: true })`; first pass None like the
  CPU); `busy_from_idle_clamps_and_refuses_a_counter_going_back` (idle Δ > Δt → 0; counter back →
  None); `the_older_rc6_path_is_read_when_gt0_is_absent`; `a_suspension_drops_the_idle_baseline`
  (suspended pass → None and `idle` reset, so the next active pass gives None, not a Δ across the
  suspension); `xe_idle_residency_is_read_from_the_tile_tree` (fixture from the kernel's xe
  documentation, marked so in a comment); popup `the_intel_estimate_reads_active` (dim `active `
  before the percent when `idle_based`).
- [ ] Run → failures on the new names.
- [ ] Implement; `sample_at` for I915/Xe reads `idle_ms` and `busy_from_idle` against `GpuUsage.idle`.
- [ ] Mutation: invert the share (`Δidle·100 / Δt`) → the rc6 test red. Restore.
- [ ] Measurement: the four memory tests of Task 3 with `EVERY_KEY` unchanged (the test machine's
  i915 is now read for real, read-only), this branch against `main`, release, back to back, 3 runs;
  row in `docs/measurements.md` noting the i915 read per pass.
- [ ] Docs: feature page and configuration (`active` is an estimate from idle residency, not engine
  load; xe unverified on hardware if so); `docs/decisions.md` entry "Intel GPU usage is an
  idle-residency estimate, labelled active" (draft Q2). If the xe path cannot be checked, drop
  `Xe` from `pick_card`'s preference, keep the i915 part, and say so in the report.
- [ ] Commit `feat(resources): gpu estimates Intel usage from idle residency`.

## Self-review

- Spec coverage: net_interface (T1), ring bound alone (T2), disk_mounts and disk_interval, fullest
  on the bar, a block per path (T3), gpu_metric, amdgpu busy and VRAM, the card pick, the suspend
  guard, NVIDIA `—`, guardrails (T4), Intel estimate and `active` (T5). Control centre tab and an
  all-interfaces sum stay out (non-goals).
- Types: `Sample.disks: [Option<u8>; DISKS]`, `Sample.gpu_busy: Option<GpuBusy>`,
  `Sample.vram: Option<u8>`; `blocks(c) -> Vec<(ResourceItem, usize)>` feeds `series(item, index, h)`
  and `figure(item, index, r)`; `Card`, `GpuDriver`, `GpuUsage` are produced in T4 and consumed in T5.
- Size check: `Sample` 56 B × 60 + 16 = 3,376 B ≤ 4,096 after T3 and T4 (T2 precedes the first growth).
