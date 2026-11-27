# Bar click actions and module tooltips Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `bar.toml` bind each module's clicks and wheel to a user argv or a built-in verb
(`[bindings.<module>]`), and show a one-line tooltip on nine modules after ½ s of rest, through the
tray's existing hover timer and popup. Defaults unchanged.

**Architecture:** Config types and validation in `config.rs`; a pure `actions::bound` read before
every default in `wayland.rs::click`; a `Target::Passive(ModuleId)` zone for modules that have no
click zone today; a pure `tooltips.rs` with one text function per module, a `tooltip` column in
`modules.rs`'s `MODULES`; `session_bus/hover.rs`'s key generalised to `HoverKey`.

**Tech Stack:** Rust, the daemon's `Grid`, `Hover`, `DeadlineTimer`, `PopupBody::Tooltip`, serde/toml
(already in). No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-27-bar-actions-design.md`.

## Global Constraints

- Every cargo command in `nix develop -c …`; headless and memory suites at `nice -n 19`; the memory
  suite in release, `--test-threads=1`.
- No new dependency, thread, timer or poll. The hover timer stays the only timer and is armed only
  while the pointer rests on a hover zone.
- Never call a session tool for real: `FakeExecutor` in unit tests, `fake_argv_recorder` on the
  harness's private `PATH` in headless tests; never a state-changing command. Never run
  `alpheus`/`alpheus-demon`/`alpheus-launch` on the session; `pgrep` after each headless run, kill only
  PIDs you started.
- Logs are fixed words: never a binding's command, a tooltip's text, a title, a device or network
  name. Config errors name the key, never the command.
- A tooltip never shows what the bar does not draw (decision 7 extended): no SSID, sink/source label,
  peripheral name, window class, app name.
- Tests first, run red, then mutated to prove they bite; never loosen a guardrail test (an existing
  test whose setup must change says why in the commit body, its assertions kept).
- One logical change per commit, `type(scope): …`, body says why, trailers
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and the session line. Stage by name,
  read `git diff --cached`. English everywhere.
- Every task ends with a RAM measurement of each part it touches, recorded in
  `docs/measurements.md`: the daemon through `tests/memory.rs`, release, back to back against `main`
  (07227887) built in its own target directory (`CARGO_TARGET_DIR=$(mktemp -d)`), 3 runs each side;
  trust deltas. A task with no measurement is not done.
- 15 MB resident at rest for everything; a feature not enabled costs nothing.
- Out of scope: autohide (a later cluster); do not add its keys or code.

## Review Focus

1. **A zone added for a tooltip or a binding gives a module a click it never had** (the clock with
   `calendar = false` opening the calendar; the window opening a tab) → Task 2
   (`a_passive_zone_takes_no_click_of_its_own`).
2. **Defaults drift**: an empty or absent `[bindings]` changes any command, popup or verb on any
   zone and button → Task 2 (`empty_bindings_change_no_click_byte_for_byte`), with
   `every_click_on_a_full_bar_stays_on_the_allowlist` kept unchanged.
3. **A module tooltip closed by the tray**: `tooltip_item_gone` treated a missing tray item as
   "gone", so any tray change would close a clock tooltip → Task 4
   (`a_module_tooltip_is_never_gone_with_a_tray_item`).
4. **A tooltip leaks what the bar does not draw** (SSID, sink label, peripheral name) → Task 3
   (`tooltips_name_no_private_field`, `the_battery_tooltip_names_kinds_never_devices`).
5. **The hover timer keeps waking or arms where nothing shows** (a module without text, `tooltips =
   false`, the pointer at rest on a module after the fire) → Task 4
   (`a_module_with_no_tooltip_arms_nothing`, `tooltips_false_arms_no_timer_anywhere`,
   headless `a_module_tooltip_fires_once_then_the_loop_sleeps`).

---

### Task 1: The `[bindings]` and `tooltips` configuration

**Files:** `crates/alpheus-daemon/src/config.rs` (types, `Raw`, `BarConfig`, `check_bindings`,
header example), `crates/alpheus-daemon/src/config/tests.rs`, `docs/configuration.md`,
`docs/measurements.md`.

**Interfaces — Produces:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Verb { None, Popup, Quick, OpenTab, Launcher, Dnd, Mute }
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding { Verb(Verb), Command(Vec<String>) }      // Deserialize: a string → Verb, a list → Command
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)] #[serde(default, deny_unknown_fields)]
pub struct ButtonBindings { pub left: Option<Binding>, pub middle: Option<Binding>, pub right: Option<Binding>,
                            pub wheel_up: Option<Binding>, pub wheel_down: Option<Binding> }
pub const BINDING_ARGV_MAX: usize = 16;
pub struct BarConfig { pub bindings: BTreeMap<ModuleId, ButtonBindings>, pub tooltips: bool, /* … */ }
fn check_bindings(b: &BTreeMap<ModuleId, ButtonBindings>, modules: &[ModuleId]) -> Result<(), String>;
impl ModuleId { pub fn bindable(self) -> bool }   // false: Workspaces, Tray, Osd, Quick, Displays
```
`ModuleId` gains `PartialOrd, Ord`. `BarConfig.tooltips = raw.tooltips && raw.tray.tooltips`; the
code reads `bar.config.tooltips` (Task 4 switches `hover_moved` to it).

- [ ] Failing tests (`config/tests.rs`):
  - `bindings_default_empty_and_tooltips_on`: `BarConfig::default().bindings.is_empty()`, `.tooltips`.
  - `a_binding_parses_as_argv_or_verb`: `[bindings.volume] right = ["pavucontrol"]`, `wheel_up = "none"`,
    `left = "open_tab"` → the three `Binding`s, other buttons `None`.
  - `bindings_reject_blank_programs_unknown_modules_and_unknown_verbs`: `right = []`, `right = [" "]`,
    `[bindings.nowhere]`, `right = "explode"`, `[bindings.volume] scroll = …` → each `Err` naming
    `bindings.<module>.<button>` (or the module).
  - `bindings_reject_unbindable_modules`: `[bindings.tray]`, `[bindings.workspaces]`, `osd`, `quick`,
    `displays` → `Err`.
  - `binding_argv_bounds_and_placeholders`: 16 items ok, 17 `Err`; NUL `Err`; `{tab}` on `clock` (no tab)
    `Err`, on `volume` ok, twice `Err`, inside `"x{tab}"` `Err`, as the program `Err`; `{output}` ok once.
  - `binding_verbs_need_what_they_act_on`: `popup` on `battery` (no popup) `Err`; `popup` on `volume`
    without `quick` in `modules` `Err`, with it ok; `quick` without `quick` `Err`; `open_tab` on `clock`
    `Err`; `dnd` without `notifications` `Err`; `none`, `launcher`, `mute` always ok.
  - `a_binding_for_an_absent_module_is_accepted`: `modules = ["clock"]`, `[bindings.volume] …` → `Ok`.
  - `binding_errors_never_quote_the_command`: an invalid argv `["secret-tool", "\u{0}"]` → message has no
    `secret-tool`.
  - `tray_tooltips_false_turns_every_tooltip_off`: top `tooltips = false` → false; `[tray] tooltips = false`
    → false; both absent → true.
- [ ] Run `nix develop -c cargo test -p alpheus-daemon --lib config::` → fails to compile (no `bindings`).
- [ ] Implement: `Binding` deserialised by hand (`Visitor` accepting a string or a sequence; an
  unknown verb names the accepted list); `check_bindings` after `check_timer`, reusing
  `check_placeholders` with `("{tab}", 0, n)` where `n` is 1 when `m.spec().tab` is `Some`, else 0, and
  `("{output}", 0, 1)`; NUL as `check_hooks`.
- [ ] Mutate: drop the `quick` requirement of `popup` → `binding_verbs_need_what_they_act_on` fails;
  restore.
- [ ] MEASUREMENT: `startup_pss_per_source` (`everything` case) and the stripped binary size, this
  branch against `main`, release, back to back, 3 runs each; row "Bar bindings and tooltips key, linked
  in" in `docs/measurements.md`.
- [ ] Docs: `docs/configuration.md` top level (`tooltips`), new `### [bindings.<module>]` section after
  `[actions]` (buttons, argv rules, verbs and their needs, bindable modules), `[tray] tooltips` marked
  as the old spelling; `config.rs` header example.
- [ ] Commit `feat(config): per-module click bindings and one tooltips key`.

### Task 2: Bindings take effect

**Files:** `crates/alpheus-daemon/src/grid.rs` (`Target::Passive`), `src/layout.rs` (`segment`,
`wants_zone`), `src/actions.rs` (`Bound`, `bound`), `src/actions/tests.rs`, `src/popup.rs`
(`ModulePopup::of` unchanged, matches stay exhaustive), `src/wayland.rs` (`click`, `zone_anchor`),
`tests/guardrails.rs`, `tests/headless_session.rs`, `docs/features/bar.md`, `docs/measurements.md`.

**Interfaces — Consumes:** `Binding`, `Verb`, `ButtonBindings`, `BarConfig.bindings` (Task 1).
**Produces:**
```rust
pub enum Target { Workspace { .. }, Module(ModuleId), Passive(ModuleId), TrayItem(ItemKey) }
pub(crate) fn wants_zone(bar: &Bar, m: ModuleId) -> bool;   // a binding, or (tooltips && spec.tooltip.is_some()) — the tooltip half is a stub returning false until Task 3
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound { Nothing, Run(Vec<String>), Popup(ModulePopup), Launcher, Dnd }
pub fn bound(bar: &Bar, output: &str, target: &Target, b: Button) -> Option<Bound>;
fn zone_anchor(&self, i: usize, x: f64) -> Option<(PopupAnchor, Target)>;   // wayland.rs, split from module_popup_at
```
`bound`: `Module(m)` or `Passive(m)` with a binding for `b` → `Some`; `Command` → `Run` with `{tab}`
and `{output}` through `substitute`; `Verb::Mute` → `Run(wpctl set-mute SINK toggle)`; `OpenTab` →
`Run(open_tab(bar, m))`; `Popup` → `Popup(spec.popup)`; `Quick` → `Popup(ModulePopup::Quick)`;
`None` → `Nothing`. `command(…, Passive(_), …)` → `None`.

- [ ] Failing tests (`actions/tests.rs`):
  - `a_command_binding_runs_as_written`: `[bindings.volume] right = ["my-mixer", "{output}", "{tab}"]` →
    `bound(…, "DP-1", Module(Volume), Right) == Some(Run(["my-mixer","DP-1","sound"]))`.
  - `each_verb_resolves`: none → `Nothing`, mute → the allowlisted wpctl argv, open_tab → `alpheus-toggle
    sound`, popup on clock → `Popup(Calendar)`, quick → `Popup(Quick)`, launcher → `Launcher`, dnd → `Dnd`.
  - `an_unbound_button_keeps_today`: only `right` bound on volume → `bound(…, Left)` is `None`.
  - `empty_bindings_change_no_click_byte_for_byte`: for `guardrails::full_bar`-like bar with and without
    `[bindings.volume]` (no key) and `[bindings.clock]` (no key): every zone × button gives the same
    `command`, `ModulePopup::of` and `bound == None`.
  - `a_passive_zone_takes_no_click_of_its_own`: `Passive(Clock)` and `Passive(Window)` × every button →
    `command` `None`, `ModulePopup::of` `None`, `toggles_launcher/dnd/timer_middle` false,
    `media_control` `None`.
  - `layout`: `a_bound_module_without_a_zone_gets_a_passive_one` (window with a binding → a
    `Passive(Window)` zone over its cells; clock `calendar = false` same; without binding → no zone).
- [ ] Failing guardrail (`tests/guardrails.rs`): `every_user_binding_runs_as_written` — a bar binding
  every button of every bindable module to `["recorder", "{output}"]` (and `{tab}` where allowed): each
  `bound` `Run` through `FakeExecutor` records exactly the configured program and substituted args, the
  program is never `sh`/`bash`, no argument is `-c`, no call contains `dpms`.
- [ ] Run `nix develop -c cargo test -p alpheus-daemon actions:: layout:: --lib` and the guardrail →
  compile errors (no `Bound`, no `Passive`).
- [ ] Implement; in `wayland.rs::click` ask `bound` first (from the zone under `x`), then: `Run` →
  `actions::run`; `Popup(p)` → `(p.ops().toggle)(self, qh, anchor, &target)` with `zone_anchor`;
  `Launcher` → `decide_click(true, launcher_was_open)`; `Dnd` → `self.set_dnd(!self.dnd)`; `Nothing`
  → return. Then today's path untouched.
- [ ] Headless (`tests/headless_session.rs`, `#[ignore]`):
  `a_user_binding_runs_its_argv_on_a_click` — `fake_argv_recorder(&sway, "bound")`, `bar.toml` with
  `modules = ["clock"]`, `[bindings.clock] right = ["<recorder path>", "{output}"]`; right click on the
  clock → `recorded_argv == [["HEADLESS-1"]]`; a left click still logs `calendar shown`. `pgrep` after.
- [ ] Mutate: let `command` treat `Passive(m)` like `Module(m)` → `a_passive_zone_takes_no_click_of_its_own`
  fails; restore.
- [ ] MEASUREMENT: `startup_pss_per_source` (`everything`), `pss_under_budget_and_stable`, `at_rest_no_wakeups`,
  back to back against `main`, 3 runs each; record a row "Bar bindings, dispatch" (expected within
  noise; 0 wake-ups).
- [ ] Docs: `docs/features/bar.md` "Using it" (bindings paragraph: a button left out keeps today's;
  verbs), "Configuration" example, "Rules it keeps" (a binding runs as written, never a shell; a
  `Passive` zone adds no click; the allowlist test pins defaults only), "How it is built".
- [ ] Commit `feat(bar): user bindings for module clicks and wheel`.

### Task 3: Tooltip texts (pure)

**Files:** create `crates/alpheus-daemon/src/tooltips.rs` (+ `src/tooltips/tests.rs`), modify
`src/lib.rs` (module), `src/modules.rs` (`ModuleSpec.tooltip`, `row`), `src/layout.rs`
(`wants_zone`'s tooltip half), `tests/guardrails.rs`, `docs/measurements.md`.

**Interfaces — Produces:**
```rust
pub(crate) struct ModuleSpec { /* … */ pub tooltip: Option<fn(&Bar) -> Option<String>> }
// tooltips.rs
pub(crate) fn clock(bar: &Bar) -> Option<String>;       // clock::format("%A %e %B %Y"), trimmed
pub(crate) fn battery(bar: &Bar) -> Option<String>;
pub(crate) fn volume(bar: &Bar) -> Option<String>;
pub(crate) fn network(bar: &Bar) -> Option<String>;
pub(crate) fn bluetooth(bar: &Bar) -> Option<String>;
pub(crate) fn resources(bar: &Bar) -> Option<String>;   // the items of [resources] show, in order
pub(crate) fn window(bar: &Bar) -> Option<String>;      // the focused window's whole title
pub(crate) fn failed(bar: &Bar) -> Option<String>;
pub(crate) fn notifications(bar: &Bar) -> Option<String>;
```
`Feed::Waiting` → `None`; `Feed::Down` → `Some("<Module> unavailable")`. Rows filled for these nine
modules only; the others `None`.

- [ ] Failing tests (`tooltips/tests.rs`), one per function with its exact text:
  `the_clock_tooltip_is_the_full_date` (2026-11-27 → `Friday 27 November 2026`);
  `the_volume_tooltip_says_percent_or_muted`; `the_network_tooltip_says_link_vpns_and_limits`
  (`Wi-Fi`, `Ethernet · 2 VPNs`, `Wi-Fi · limited connectivity`, `Offline`, Down → `Network
  unavailable`); `the_battery_tooltip_names_kinds_never_devices` (a peripheral named `Kettle MX` of
  kind Mouse → contains `Mouse 40 %`, never `Kettle`; a coarse-only peripheral hidden when
  `[battery] coarse` hides it); `the_bluetooth_tooltip_shows_the_drawn_device_whole`;
  `the_resources_tooltip_follows_show`; `the_window_tooltip_is_the_title_never_the_class`;
  `the_failed_tooltip_counts_never_names`; `the_bell_tooltip_counts_and_dnd`;
  `waiting_feeds_have_no_tooltip`; `only_nine_modules_have_a_tooltip` (the `MODULES` rows with
  `tooltip.is_some()` are exactly those nine).
- [ ] Failing guardrail: `tooltips_name_no_private_field` — `tooltips.rs` (production part) contains
  none of `ssid`, `sink_name`, `source_name`, `sinks`, `toplevels`, `identity`, `bus_name`, `class`,
  no `log(`, no `unwrap`/`expect`/`panic!`/`[..` slicing (reuse the file-list helpers of
  `the_tools_pure_parts_have_no_panicking_path_and_log_nothing`).
- [ ] Run → compile errors. Implement; ` · ` separates parts; percentages `N %` as the bar's own
  figures.
- [ ] Layout test: `a_tooltip_module_without_a_zone_gets_a_passive_one` (window, clock without calendar,
  `tooltips` on → `Passive` zone; `tooltips = false` → none).
- [ ] Mutate: let `battery` print the peripheral's `name` → `the_battery_tooltip_names_kinds_never_devices`
  fails; restore.
- [ ] MEASUREMENT: `startup_pss_per_source` (`everything`) back to back against `main` (texts are code
  only, the two `Passive` zones per grid the only data), binary size; record a row.
- [ ] Commit `feat(bar): tooltip texts for nine modules`.

### Task 4: Module tooltips on hover

**Files:** `crates/alpheus-daemon/src/session_bus/hover.rs`, `src/wayland.rs` (`hover_moved`,
`hover_fire`, `hover_target_moved`, `tooltip_item_gone`), the menu state holding `tooltip_item`,
`src/popup.rs` (`popup_close` resets the owner), `tests/guardrails.rs`
(`hover_fire_finds_its_bar_by_surface_identity_never_by_name` extended), `tests/headless_session.rs`,
`tests/memory.rs`, `docs/features/tray.md`, `docs/features/bar.md`, `docs/measurements.md`.

**Interfaces — Consumes:** `ModuleSpec.tooltip` (Task 3), `Target::Passive` (Task 2),
`BarConfig.tooltips` (Task 1). **Produces:**
```rust
#[derive(Debug, Clone, PartialEq, Eq)] pub enum HoverKey { Tray(ItemKey), Module(ModuleId) }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum HoverRef<'a> { Tray(&'a ItemKey), Module(ModuleId) }
impl Hover {
    pub fn moved(&mut self, found: Option<(HoverRef<'_>, u16, u16)>, output: &str, now: Instant) -> HoverChange;
    pub fn fire(&mut self, now: Instant) -> Option<(HoverKey, Anchor)>;
    pub fn target(&self) -> Option<(HoverRef<'_>, &Anchor)>;
}
pub fn over(tooltips: bool, grid: &Grid, column: u16) -> Option<(HoverRef<'_>, u16, u16)>;
pub fn owner_gone(owner: Option<&HoverKey>, item_exists: impl Fn(&ItemKey) -> bool) -> bool; // Module → false
```
`menu.tooltip_item: Option<ItemKey>` becomes `tooltip_owner: Option<HoverKey>`.

- [ ] Failing tests (`hover.rs`): `module_hover_arms_once_and_cancels_off_the_zone`;
  `moving_from_a_module_to_a_tray_icon_rearms`; `a_module_with_no_tooltip_arms_nothing` (a
  `Module(Launcher)` zone → `over` `None`); `a_passive_zone_with_a_tooltip_arms` (`Passive(Window)`);
  `tooltips_false_arms_no_timer_anywhere` (tray and module zones → `None`);
  `a_module_tooltip_is_never_gone_with_a_tray_item` (`owner_gone(Some(&Module(Clock)), |_| false)` is
  false; `Some(&Tray(k))` with `|_| false` true); existing hover tests ported to `HoverRef::Tray`.
- [ ] Extend `hover_fire_finds_its_bar_by_surface_identity_never_by_name` to pin that the module
  branch also locates the surface through `hover_surface` (no `anchor.output` lookup).
- [ ] Run → compile errors. Implement: `hover_moved` uses `bar.config.tooltips`; `hover_fire` matches
  the key — `Tray` as today, `Module(m)` → `(m.spec().tooltip?)(&app.bar)`, then the same popup path,
  log stays `tooltip shown`; `hover_target_moved` finds `Module(m)`/`Passive(m)` zones;
  `tooltip_item_gone` → `owner_gone`.
- [ ] Existing headless test `no_timer_while_the_pointer_is_elsewhere` rests on the clock, which now
  has a tooltip: move its rest point to the bar's empty middle (no zone), assertions unchanged, and
  say why in the commit body. Any other headless or memory test that now sees an extra tooltip on a
  module it rests on gets `tooltips = false` in its `bar.toml` (setup, not assertions), listed in the
  report.
- [ ] Headless (`#[ignore]`): `a_module_tooltip_appears_after_half_a_second_and_goes_on_leave` (clock,
  `modules = ["clock"]`: `tooltip shown` after ≥ 450 ms, `popup closed (hover ended)` off the bar);
  `a_module_tooltip_fires_once_then_the_loop_sleeps` (rest on the clock away from the minute: one
  `tooltip shown`, then 0 main-thread wake-ups in 5 s); `tooltips_false_shows_nothing_on_a_module`.
  `pgrep` after.
- [ ] Memory (`#[ignore]`, release): `module_tooltips_free_everything_on_close` — one warm-up hover
  cycle on the clock, then 20; anonymous memory ≤ warm-up + 128 KB, PSS printed;
  `at_rest_no_wakeups_after_a_module_tooltip`.
- [ ] Mutate: make `owner_gone` return true for `Module` → its test fails; make `over` ignore
  `tooltips` for modules → `tooltips_false_arms_no_timer_anywhere` fails; restore.
- [ ] MEASUREMENT: the two new memory tests (3 runs) and `startup_pss_per_source`,
  `pss_under_budget_and_stable`, `at_rest_no_wakeups` back to back against `main`; row "Module
  tooltips" (shown, after 20 cycles, wake-ups).
- [ ] Docs: `docs/features/tray.md` "Tooltips" (the key is now the top-level `tooltips`, old spelling
  kept), `docs/features/bar.md` "Using it" (which modules have one, what each says, no SSID and why),
  known limit (a shown tooltip is a snapshot; it closes if its module's cells move), "What it costs".
- [ ] Commit `feat(bar): module tooltips on the tray's hover timer`.

### Task 5: Branch proof and docs

**Files:** `docs/decisions.md`, `CHANGELOG.md`, `docs/measurements.md`, `docs/features/bar.md`
("What it costs" final figures).

- [ ] `nix develop -c cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, the headless suite and the memory suite (`nice -n 19`) clean; `pgrep`
  shows only the owner's processes.
- [ ] MEASUREMENT: the whole branch against `main`, release, back to back, 3 runs: `startup_pss_per_source`
  (`everything`, `bar only`), `pss_under_budget_and_stable`, binary size; replace the per-task rows by
  one summary row if they agree, keeping the method line.
- [ ] `docs/decisions.md`: "A user binding replaces a module's default for that button only; a verb
  runs no new command" and "A tooltip spells out only what the bar draws (no SSID, device label or
  window class)".
- [ ] `CHANGELOG.md` (Unreleased): bindings, module tooltips, the top-level `tooltips` key.
- [ ] Note for the owner in the report: check once on Mango that a module tooltip shows and goes, and
  that a bound right click runs (no Mango harness).
- [ ] Commit `docs(bar): bindings and module tooltips, measured`.
