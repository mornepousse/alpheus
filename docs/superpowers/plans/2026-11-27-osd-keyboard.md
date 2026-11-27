# More OSD kinds and a keyboard module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Five opt-in passive OSD kinds (`bluetooth`, `dnd`, `profile`, `layout`, `track`) and a
display-only `keyboard` bar module (layout and bind mode), on Mango first, then Sway.

**Architecture:** The compositor sources fill two new `CompositorState` fields (`layout`, `mode`)
only when something wants them: Mango from the `all-monitors` snapshot it already parses, Sway
from one `GET_INPUTS` per session and the `input` / `mode` events. The `keyboard` segment reads
them. The OSD gains a generic `Passive<T>` watch (baseline, grace, change) fed from the messages
that already reach `App::on_bar_message`; new `Pill` variants draw as glyph-and-text rows.

**Tech Stack:** Rust, serde_json, the daemon's `Grid`, `Segment`, `osd` and the i3 IPC framing
already in `sources/sway.rs`. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-27-osd-keyboard-design.md`

**Base for every measurement:** `main` at `07227887`, built in its own target directory
(`CARGO_TARGET_DIR=$(mktemp -d)`), handed to the memory suite with `ALPHEUS_DEMON_UNDER_TEST`.

## Global Constraints

- Every cargo command: `nix develop -c …`; the headless and memory suites at `nice -n 19`.
- No new dependency, thread, timer or polling. A kind or module not configured costs nothing: no
  subscription, no source, no field published, no extra redraw.
- Session safety: never run `alpheus`, `alpheus-demon` or `alpheus-launch` on the session, never a
  session tool (`mmsg`, `swaymsg` on the real socket, `bluetoothctl`, `powerprofilesctl`…). Unit
  tests use `FakeMango` / `fake::FakeSway`; headless tests only through
  `crates/alpheus-daemon/tests/common` (isolated Sway, private bus, `FakeMango`,
  `FakePowerProfiles`). After a headless run, `pgrep -u $USER -a alpheus` shows only the owner's
  daemon. No `pkill`/`killall`; kill only a PID you started.
- The daemon never writes to the compositor beyond `Message`'s entries and Mango's watch lines;
  the Sway guardrail is extended only as decision 1 allows (one read, two events).
- Logs are fixed words: never a layout name, a mode, a track title, a profile, a device name.
- Colours only through `Style`/`Tint` roles. English everywhere. Comments follow `docs/STYLE.md`
  (no forbidden token in a comment of a guarded file).
- Tests first; each new test is mutated once (break the code, watch it fail) before the commit.
- Never loosen a guardrail test; the Sway one is extended (decision 1), never relaxed.
- One logical change per commit, `type(scope): …`, body says why, trailers
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01CeSMKdJbo2DSLPZZAAn4XX`. Stage by name, read
  `git diff --cached` first. Never commit `flake.lock`. Work only in `.worktrees/osd-keyboard`.
- **Every task ends with a RAM measurement of each part it touches, recorded in
  `docs/measurements.md`** (daemon: `tests/memory.rs`, release, back to back against the base,
  3 runs each; leak bounds on `Anonymous:`, total PSS printed). A task with no measurement is not
  done.
- Each task updates `docs/configuration.md` (and the `//!` key list at the top of `config.rs`) for
  any key it adds, and the feature page it touches (`docs/features/osd.md`, the new
  `docs/features/keyboard.md`, `docs/features/bar.md`).

## Review Focus

1. **The keyboard fields leak into the state with the module off** (Mango publishes and redraws on
   every layout change although nothing reads it) → Task 1
   (`mango_without_keyboard_publishes_no_layout_nor_mode`, and the `FakeMango` push test
   `a_layout_change_without_keyboard_publishes_nothing`).
2. **Sway sends the keyboard subscription or `GET_INPUTS` with the module off, or re-reads the tree
   on every `input` event** → Task 3 (`keyboard_off_subscribes_to_two_events_and_never_asks_inputs`,
   `an_input_event_updates_the_layout_without_reading_the_tree`).
3. **A passive kind shows at start or right after its source restarts** (a pill at every login, or
   when BlueZ restarts) → Task 4 (`passive_first_value_is_the_baseline`,
   `passive_a_source_going_down_takes_a_new_baseline`, `passive_inside_the_grace_nothing_shows`).
4. **The track pill fires on pause/resume or on a metadata flicker** → Task 5
   (`track_pause_and_resume_of_the_same_title_show_nothing`,
   `track_titles_inside_the_grace_after_a_player_appears_show_nothing`).
5. **A private string reaches the log** (layout, title, profile, mode) → Task 4 and Task 5
   (`osd_passive_pills_are_described_in_fixed_words`, guardrail
   `the_osd_logs_are_fixed_words`).

---

### Task 1: Keyboard layout and mode in the compositor state, on Mango

**Files:**
- Modify: `crates/alpheus-daemon/src/compositor/mod.rs` (`CompositorState`)
- Modify: `crates/alpheus-daemon/src/compositor/mango.rs` (`Monitor`, `Model`, tests)
- Modify: `crates/alpheus-daemon/src/sources/mango.rs` (`session` signature), `src/sources/mod.rs`
  (`start`), `src/sources/tests.rs` (callers)
- Modify: `crates/alpheus-daemon/src/modules.rs` (`Needs::KEYBOARD`), `src/startup.rs`
  (`keyboard_wanted`, the `sources::start` call at line ~812)
- Modify: `crates/alpheus-daemon/tests/support/fake_mango.rs` (a push of a monitors line)
- Create: `crates/alpheus-daemon/tests/fixtures/mango-monitors-layout-fr.json` (the
  `mango-all-monitors.json` capture with `"keyboardlayout": "French (AZERTY)"`, `"keymode":
  "resize"` on the active monitor; anonymised like its source)

**Interfaces — Produces:**
```rust
// compositor/mod.rs
pub struct CompositorState { /* … */ pub layout: Option<String>, pub mode: Option<String> }
// compositor/mango.rs
impl Model { pub fn new(keyboard: bool) -> Model; }   // Default stays: keyboard = false
// sources/mango.rs, sources/mod.rs
pub fn session(socket: &Path, keyboard: bool, publish: &mut dyn FnMut(CompositorState) -> bool) -> io::Result<()>;
pub fn start(c: Option<Compositor>, keyboard: bool, send: impl Fn(Option<CompositorState>) -> bool + Send + 'static);
// modules.rs / startup.rs
pub(crate) const KEYBOARD: Needs = Needs(1 << 12);
pub fn keyboard_wanted(modules: &[ModuleId], osd: &OsdConfig) -> bool; // Needs::of(modules) has KEYBOARD; Task 5 adds the layout kind
```
Hyprland and niri sessions ignore `keyboard` (fields stay None).

- [ ] **Step 1: failing tests** in `compositor/mango.rs`:
  - `mango_the_monitors_carry_keymode_and_keyboardlayout`: `Model::new(true)` fed
    `mango-all-monitors.json` and `mango-all-clients.json` → `state().layout ==
    Some("English (US)")`, `state().mode == Some("default")`.
  - `mango_layout_comes_from_the_active_monitor`: the new fixture → `French (AZERTY)` / `resize`
    (the active monitor's, not the first's).
  - `mango_without_keyboard_publishes_no_layout_nor_mode`: `Model::default()` on the same lines →
    both None.
  - `mango_a_snapshot_without_the_fields_gives_none`: `mango-unknown-fields.json` /
    `mango-monitors-multi.json` (no such keys) with `keyboard` → None, `apply` is `Ok`.
  - `mango_layout_is_cleaned_and_capped`: a 2,000-character name with a control char → `clean`'s
    result.
  In `sources/mango.rs` tests: `a_layout_change_without_keyboard_publishes_nothing` (FakeMango
  pushes the same monitors with another `keyboardlayout`, `keyboard = false` → no second publish
  within 300 ms) and `a_layout_change_with_keyboard_publishes_it` (→ second state carries the new
  layout).
- [ ] **Step 2: run** `nix develop -c cargo test -p alpheus-daemon mango` → compile errors (no
  `layout` field, no `Model::new`), then the assertions fail.
- [ ] **Step 3: implement.** Two `#[serde(default)] keymode: Option<String>, keyboardlayout:
  Option<String>` on `Monitor`; `Model.keyboard: bool`; `state()` fills the fields from
  `monitors.iter().find(|m| m.active).or(monitors.first())` through `label`. Thread `keyboard`
  from `startup.rs` (`keyboard_wanted`: `Needs::of(modules).contains(Needs::KEYBOARD)`; no row
  asks for it until Task 2, so the shipped bar passes `false`; test `keyboard_wanted_follows_needs`
  through `wanted_for`-style injected `Needs`). No change to the `the_mango_and_sway_backends_have_no_panicking_path`
  list (still four files, still passing).
- [ ] **Step 4: mutate:** fill the fields regardless of `keyboard` →
  `mango_without_keyboard_publishes_no_layout_nor_mode` and
  `a_layout_change_without_keyboard_publishes_nothing` fail. Restore.
- [ ] **Step 5: measure.** `startup_pss_per_source` (the `bar only` and `everything` cases) and
  `at_rest_no_wakeups_under_sway_and_mango`, release, 3 runs, this branch against the base. Expect
  within noise (no field published). Record one row "Keyboard state, linked in, module off" in
  `docs/measurements.md` (component table), with the binary size delta (stripped).
- [ ] **Step 6: docs.** `docs/compositors.md` support matrix: a row "Keyboard layout and mode" —
  Mango: "yes: the monitors snapshot (`keyboardlayout`, `keymode`)"; Sway: "next task"
  (filled in Task 3); Hyprland/niri: "no (retired)". `docs/decisions.md` entry "The keyboard layout
  comes from the compositor's own state, never from the keyboard" (Mango's snapshot, Sway's IPC; no
  `wl_keyboard`, no evdev, no `mmsg` child; lock keys left out, see the spec). Date 2026-11-27,
  Source: the Task 1 tests.
- [ ] **Step 7: commit** `feat(compositor): read the keyboard layout and mode from Mango's monitors`.

### Task 2: The `keyboard` bar module

**Files:**
- Modify: `crates/alpheus-daemon/src/config.rs` (`ModuleId::Keyboard`, `KeyboardConfig`,
  `BarConfig.keyboard`, the `//!` key list), `src/config/tests.rs`
- Modify: `crates/alpheus-daemon/src/modules.rs` (`COUNT` 20 → 21, the row after `Window`),
  `src/layout.rs` (`REMOVAL`: 14 → 15, `Keyboard` right after `Window`)
- Modify: `crates/alpheus-daemon/src/segments.rs` (`KEYBOARD` glyph, `keyboard`, `short_layout`)
- Modify: `crates/alpheus-daemon/src/startup.rs` (`keyboard_wanted` now reads the module)
- Modify: `crates/alpheus-daemon/tests/guardrails.rs` (`full_bar()` gives the module a state,
  so `every_click_on_a_full_bar_stays_on_the_allowlist` covers it), `tests/headless_session.rs`,
  `tests/memory.rs`
- Create: `docs/features/keyboard.md`; modify `docs/features/README.md`, `docs/features/bar.md`
  (table of modules, narrow-bar order), `docs/configuration.md` (`[keyboard]`),
  `docs/comparison.md` (row)

**Interfaces — Consumes:** `CompositorState.layout`, `.mode`, `keyboard_wanted` (Task 1).
**Produces:**
```rust
pub struct KeyboardConfig { pub short: bool /* true */, pub show_mode: bool /* true */ } // deny_unknown_fields
pub const KEYBOARD: char = '\u{f030c}'; // md-keyboard (already drawn for a peripheral, segments.rs)
pub fn short_layout(name: &str) -> String;
pub(crate) fn keyboard(feed: &Feed<CompositorState>, cfg: &KeyboardConfig) -> Option<Segment>;
```

- [ ] **Step 1: failing tests.**
  - `segments` tests: `short_layout_takes_the_letters_in_parentheses` (`English (US)` → `us`,
    `German (CH)` → `ch`), `short_layout_falls_back_to_the_first_two_letters` (`French (AZERTY)` →
    `fr`, `English (Dvorak)` → `en`, `Russian` → `ru`), `short_layout_of_an_empty_or_symbol_name`
    (`""` → `""`, `"(é)"` → the first two characters lower-cased, never a panic);
    `keyboard_segment_shows_the_short_layout` (glyph then `us`), `keyboard_segment_full_name_when_not_short`,
    `keyboard_segment_shows_a_mode_other_than_default` (`us resize`, the mode in `Style::ACCENT`),
    `keyboard_segment_hides_default_and_respects_show_mode`, `keyboard_segment_absent_without_layout_nor_mode`
    (both None → `None`; `Feed::Down` → None, the module is not a status that can be "down").
  - `config` tests: `keyboard_defaults` (`short` and `show_mode` true), `keyboard_unknown_key_is_invalid`,
    `keyboard_is_not_a_default_module`.
  - `modules` tests already pin each row's `id` to its index and `layout` tests the `REMOVAL`
    length: update both, and add `keyboard_needs_the_keyboard_fields_only` (`Needs::of(&[Keyboard])
    == Needs::KEYBOARD`).
  - `startup` tests: `keyboard_wanted_with_the_module`.
  - Headless: `keyboard_under_mango_shows_the_layout_and_follows_a_change` (isolated Sway for the
    outputs, `Backend::Mango(&fake.socket)`, `modules` with `keyboard`; the bar's cells read `us`;
    `FakeMango::push_monitors` the `layout-fr` fixture → cells read `fr resize`); and
    `keyboard_off_under_mango_redraws_nothing_on_a_layout_change` (no redraw log line / frame count
    unchanged after the push).
- [ ] **Step 2: run** `nix develop -c cargo test -p alpheus-daemon keyboard` → compile errors,
  then failures; headless with `--include-ignored keyboard_under_mango`.
- [ ] **Step 3: implement** per the recipe (`docs/adding-a-module.md` steps 3–5, 9–12): no popup,
  no click (nothing in `actions.rs`), `GroupKind::Status`, no hue *(writer's call; revisit only if
  the bar's colour pass asks)*, `needs: Needs::KEYBOARD`.
- [ ] **Step 4: mutate:** make `short_layout` always take the first two letters →
  `short_layout_takes_the_letters_in_parentheses` fails; show `default` →
  `keyboard_segment_hides_default…` fails. Restore.
- [ ] **Step 5: measure.** New `tests/memory.rs` test `keyboard_module_pss_under_mango`
  (`start_reading(…, mango = true)`, bar with and without `keyboard`, then 100 `push_monitors`
  alternating two layouts): print PSS for off / on / after 100 changes; assert anonymous on − off
  ≤ 64 KB and after − on ≤ 64 KB; and `at_rest_no_wakeups_with_the_keyboard_module` (Mango, 5 s
  window after a change, 0 loop wake-ups). Release, 3 runs; the base has no module, so compare
  `startup_pss_per_source` `everything` back to back for the linked-in cost. Record rows
  "Keyboard module, linked in", "Keyboard module on (Mango)", "100 layout changes" in
  `docs/measurements.md`.
- [ ] **Step 6: docs.** `docs/features/keyboard.md` in the neighbours' shape (what it does, using
  it, configuration, how it is built, rules, cost), saying: display only, no lock keys (why), no
  click (why), Mango works, Sway in Task 3's commit, Hyprland/niri nothing.
- [ ] **Step 7: commit** `feat(keyboard): a bar module that shows the layout and the bind mode`.

### Task 3: The keyboard layout and mode on Sway (guardrail extended)

**Files:**
- Modify: `crates/alpheus-daemon/src/sources/sway.rs` (`Message`, `SUBSCRIPTION_KEYBOARD`,
  `INPUT_EVENT`, `MODE_EVENT`, `WORKSPACE_EVENT`, `session`, `follow`, the header's "Never" list),
  `src/sources/sway/tests.rs`, `src/sources/mod.rs` (pass `keyboard`)
- Modify: `crates/alpheus-daemon/src/compositor/sway.rs` (`Model.layout`, `Model.mode`, `inputs`,
  `input_event`, `mode_event`; `state` copies them)
- Create: `crates/alpheus-daemon/tests/fixtures/sway-inputs.json` (a `GET_INPUTS` reply from the
  isolated headless Sway: a pointer, two keyboards with `xkb_active_layout_name`, anonymised
  identifiers), `sway-event-input-xkb-layout.json`, `sway-event-mode.json`
- Modify: `crates/alpheus-daemon/tests/guardrails.rs` (`sway_writes_only_its_three_messages` →
  `sway_writes_only_its_messages`), `docs/features/bar.md` (the test's name), `docs/compositors.md`
  (the support row's Sway cell, the "Workspaces in the bar" cell's event list),
  `tests/headless_session.rs`, `tests/memory.rs`

**Interfaces — Produces:**
```rust
pub enum Message { GetWorkspaces, Subscribe, SubscribeKeyboard, GetTree, GetInputs }
impl Message { pub const ALL: [Message; 5]; }            // codes 1, 2, 2, 4, 100
pub const SUBSCRIPTION_KEYBOARD: &str = "[\"workspace\",\"window\",\"input\",\"mode\"]";
const WORKSPACE_EVENT: u32 = 0x8000_0000; const MODE_EVENT: u32 = 0x8000_0002;
const INPUT_EVENT: u32 = 0x8000_0015;
pub fn session(socket: &Path, keyboard: bool, publish: &mut dyn FnMut(CompositorState) -> bool) -> io::Result<()>;
// compositor/sway.rs
impl Model {
    pub fn keyboard(keyboard: bool) -> Model;
    pub fn inputs(&mut self, reply: &[u8]);       // unreadable: unchanged, never an error
    pub fn input_event(&mut self, payload: &[u8]);
    pub fn mode_event(&mut self, payload: &[u8]);
}
fn follow(model: &mut Model, frame: (u32, Vec<u8>)) -> bool; // true: the tree must be read again
```

- [ ] **Step 1: failing tests.**
  - `compositor/sway.rs`: `sway_get_inputs_picks_the_first_keyboards_active_name` (fixture → the
    first `type == "keyboard"` with a non-null name), `sway_input_xkb_layout_event_changes_the_layout`,
    `sway_input_removed_or_a_pointer_event_changes_nothing`, `sway_mode_event_names_the_mode`,
    `sway_no_mode_before_the_first_mode_event`, `sway_keyboard_fields_stay_none_without_keyboard`,
    `sway_inputs_unreadable_or_too_deep_changes_nothing` (MAX_NESTING + 1, garbage).
  - `sources/sway/tests.rs` (FakeSway): `keyboard_off_subscribes_to_two_events_and_never_asks_inputs`
    (received: SUBSCRIBE with `SUBSCRIPTION`, GET_WORKSPACES, GET_TREE; never type 100);
    `keyboard_on_subscribes_to_four_events_and_asks_inputs_once` (SUBSCRIBE payload
    `SUBSCRIPTION_KEYBOARD`, exactly one type-100 message per session, after two pushed workspace
    events still one); `an_input_event_updates_the_layout_without_reading_the_tree` (push an input
    event → a state with the new layout is published, and GET_TREE count unchanged);
    `a_mode_event_is_published_without_reading_the_tree`; `a_burst_of_input_and_window_events_reads_the_tree_once`.
  - `tests/guardrails.rs`: `sway_writes_only_its_messages` — the exhaustive `name` match over five
    variants (`GET_WORKSPACES`, `SUBSCRIBE`, `SUBSCRIBE` with input and mode, `GET_TREE`,
    `GET_INPUTS`); assert `Message::GetInputs.code() == 100` and its payload empty; assert
    `SUBSCRIPTION` and `SUBSCRIPTION_KEYBOARD` exactly as above; the one `write_all` and
    `OTHER_WRITES` checks unchanged (decision 1: test and docs in the same commit).
  - Headless: `keyboard_under_sway_follows_a_layout_switch` (isolated Sway configured with
    `input type:keyboard xkb_layout us,fr`; the harness's own `sway.command("input type:keyboard
    xkb_switch_layout next")` on its private socket → the module reads `fr`) and
    `keyboard_under_sway_shows_a_mode` (the harness defines `mode "resize" {…}` and enters it →
    `us resize`, back to `default` → `us`).
- [ ] **Step 2: run** `nix develop -c cargo test -p alpheus-daemon sway` and the guardrail →
  compile errors, then failures.
- [ ] **Step 3: implement.** `session`: `Subscribe` or `SubscribeKeyboard` by `keyboard`;
  `snapshot` asks `GetInputs` only on the first snapshot of a session with `keyboard`; the event
  loop drains the burst with `follow`, ORing its result; tree read only when true (workspace or
  window event); otherwise the published state is `last` with the model's `layout` / `mode`. The
  header's "Never" list says the five messages and that `GET_INPUTS` is a read.
- [ ] **Step 4: mutate:** send `GetInputs` after every input event → `…asks_inputs_once` fails;
  make `follow` return true for an input event → `…without_reading_the_tree` fails. Restore.
- [ ] **Step 5: measure.** `keyboard_module_pss_under_sway` (as Task 2's, Sway backend, 100
  `xkb_switch_layout` through the harness), `at_rest_no_wakeups_with_the_keyboard_module` extended
  to Sway, and `at_rest_no_wakeups_under_sway_and_mango` with the module off, release, 3 runs, back
  to back against the base. Record "Keyboard module on (Sway)" and "100 layout switches (Sway)"
  in `docs/measurements.md`.
- [ ] **Step 6: docs.** `docs/features/keyboard.md` (Sway part: `GET_INPUTS` once per session, the
  two events, no mode until the first `mode` event, several keyboards: the last that changed),
  `docs/compositors.md` row, `docs/features/bar.md` test name, `docs/decisions.md` entry "Sway is
  asked for its inputs once per session, and only with the keyboard" (decision 1 of 2026-11-27).
- [ ] **Step 7: commit** `feat(sway): follow the keyboard layout and the bind mode, read only`.

### Task 4: `[osd] kinds`, the passive watch, and the Bluetooth, do-not-disturb and profile pills

**Files:**
- Modify: `crates/alpheus-daemon/src/config.rs` (`OsdKind`, `OsdConfig.kinds`, validation: `dnd`
  needs `notifications`), `src/config/tests.rs`
- Modify: `crates/alpheus-daemon/src/modules.rs` (the `osd` row's needs move to `OsdKind::needs`),
  `src/startup.rs` (`wanted` / `wanted_for` / `sound_scope` take the kinds), `src/startup/tests.rs`
- Modify: `crates/alpheus-daemon/src/osd/mod.rs` (`Passive<T>`, `Kinds`, `Pill` variants, `rows`,
  `describe`), `src/osd/tests.rs`, `src/osd/surface.rs` (`osd_passive`, the brightness verb reads
  `kinds`), `src/startup.rs` (`on_bar_message` calls `osd_passive`)
- Modify: `tests/guardrails.rs` (fixed-word log guardrail for the OSD's files),
  `tests/headless_session.rs`, `tests/memory.rs`; `docs/features/osd.md`, `docs/configuration.md`

**Interfaces — Produces:**
```rust
#[serde(rename_all = "lowercase")]
pub enum OsdKind { Volume, Mic, Brightness, Bluetooth, Dnd, Profile, Layout, Track }
impl OsdKind { pub(crate) fn needs(self) -> Needs; } // Volume|Mic: SOUND|OSD_SCOPE, Bluetooth: BLUETOOTH,
                                                     // Profile: POWER_PROFILE, Track: MEDIA, Layout: KEYBOARD, others NONE
pub struct OsdConfig { /* … */ pub kinds: Vec<OsdKind> /* [Volume, Mic, Brightness] */ }
pub(crate) fn needs_of(modules: &[ModuleId], osd: &OsdConfig) -> Needs; // osd's row adds kinds' needs only when `osd` is in modules

pub struct Passive<T> { last: Option<T>, grace: Option<Instant> }
impl<T: PartialEq + Clone> Passive<T> {
    pub fn hear(&mut self, value: Option<T>, now: Instant) -> Option<T>; // Some: show it
}
pub struct Kinds { pub bluetooth: Passive<bool>, pub dnd: Passive<bool>, pub profile: Passive<Profile>,
                   pub layout: Passive<String>, pub track: TrackWatch /* Task 5 */ }
pub enum Pill { /* … */ Bluetooth(bool), Dnd(bool), Profile(Profile), Layout(String), Track(String) }
// osd/surface.rs
impl App { pub(crate) fn osd_passive(&mut self, m: &Msg); }
```
Mapping: `BtState::On{..}` → `Some(true)`, `Off` → `Some(false)`, `Absent` or `None` → `None`;
`NotifyView { serving: true, dnd, .. }` → `Some(dnd)`, not serving → `None`;
`ProfileState { active: Some(p), .. }` → `Some(p)`, else `None`.

- [ ] **Step 1: failing tests.**
  - `osd/tests.rs`: `passive_first_value_is_the_baseline`, `passive_inside_the_grace_nothing_shows`
    (a change at `GRACE − 1 ms` → None, and that value becomes the last), `passive_at_the_grace_end_a_change_shows`
    (exactly `GRACE` → Some), `passive_the_same_value_never_shows`,
    `passive_a_source_going_down_takes_a_new_baseline` (None, then a value → None);
    `bluetooth_device_changes_do_not_show` (`On{[]}` → `On{[a]}` → nothing),
    `bluetooth_a_power_flip_shows_on_and_off`, `dnd_shows_only_while_serving`,
    `profile_a_change_shows_its_label`; `rows_of_a_text_pill` (glyph, label, the right word in
    the last `PERCENT_CELLS`… or its own width, "off" dims the glyph: `Style::MUTED`);
    `osd_passive_pills_are_described_in_fixed_words` (`describe` gives `bluetooth on`, `dnd off`,
    `profile changed` — no label).
  - `config/tests.rs`: `osd_kinds_default_is_the_three_levels`, `osd_kinds_parses_every_name`,
    `osd_kinds_unknown_name_is_invalid`, `osd_dnd_kind_needs_the_notifications_module`.
  - `startup/tests.rs`: `osd_without_sound_kinds_needs_no_sound` (`modules = [osd]`, `kinds =
    ["bluetooth"]` → `wanted.sound == false`, `dbus == [Bluetooth]`), `osd_kinds_bring_their_source`
    (profile → `Domain::Power`, track → `media`), `kinds_without_the_osd_module_bring_nothing`.
  - Guardrails: `the_osd_logs_are_fixed_words` — every `log(` in `OSD_FILES` takes a
    string literal or `osd::describe`, and `describe`'s arms
    are literals plus numbers only (the shape of the existing fixed-word guardrails).
  - Headless: `osd_a_bluetooth_flip_shows_its_pill` (private bus fake BlueZ powered off → on after
    the grace → log `osd: shown (bluetooth on)` or the existing "surface appears" line with that
    word), `osd_a_profile_change_shows_its_pill` (`FakePowerProfiles::set_active`),
    `osd_a_dnd_toggle_shows_its_pill` (`alpheus-launch --dnd toggle` on the isolated socket),
    `osd_kinds_off_read_no_bluez` (default kinds, no `bluetooth` module: no BlueZ call on the bus).
- [ ] **Step 2: run** `nix develop -c cargo test -p alpheus-daemon osd` → compile errors, then
  failures.
- [ ] **Step 3: implement.** `Passive::hear` mirrors `Watch::sound`'s baseline and grace (reuse
  `GRACE`). `osd_passive` returns at once unless `osd` is in `modules` and the kind is listed;
  it is called in `on_bar_message` right after `osd_sound`, before the bar applies the message.
  `osd_verb` shows brightness only with `Brightness` in `kinds`; `osd_sound` only with `Volume` /
  `Mic` (each pill filtered by its kind). Text pills share one helper in `osd/mod.rs`
  (`text_row(g, glyph, style, label, right)`), modelled on the sink's name card.
- [ ] **Step 4: mutate:** drop the grace in `Passive::hear` → `passive_inside_the_grace…` fails;
  show on any `BtState` change → `bluetooth_device_changes_do_not_show` fails. Restore.
- [ ] **Step 5: measure.** New `osd_kinds_pss_off_and_on` in `tests/memory.rs`: `modules` with
  `osd`, `bluetooth`, `quick`, `notifications`; default kinds vs `kinds` + `bluetooth`, `dnd`,
  `profile`; print PSS, assert anonymous delta ≤ 64 KB; one Bluetooth pill shown then gone, then
  `at_rest_no_wakeups_after_the_osd` extended with a passive pill (0 wake-ups in 5 s). Also
  `startup_pss_per_source` case `+ osd kinds` (`osd` with only `bluetooth` and no `bluetooth`
  module: the BlueZ domain's cost, and no `pw-dump`). Release, 3 runs, back to back against the
  base. Record rows in `docs/measurements.md`.
- [ ] **Step 6: docs.** `docs/features/osd.md`: the new kinds, their rules, `[osd] kinds`, what a
  kind brings, the brightness verb now needing `brightness` in kinds; `docs/configuration.md`
  `[osd] kinds`; `docs/decisions.md` entry "An OSD kind is opt-in and brings the source it reads"
  (with `dnd` needing the notifications module, and a list without `volume` and `mic` starting no
  `pw-dump`).
- [ ] **Step 7: commit** `feat(osd): opt-in kinds, with Bluetooth, do-not-disturb and profile pills`.

### Task 5: The layout and track pills

**Files:**
- Modify: `crates/alpheus-daemon/src/osd/mod.rs` (`TrackWatch`, `Kinds.layout` / `.track` wired,
  rows), `src/osd/tests.rs`, `src/osd/surface.rs` (`osd_passive` for `Msg::Compositor`,
  `Msg::Media`), `src/startup.rs` (`keyboard_wanted` adds `osd` with `Layout`), `src/startup/tests.rs`
- Modify: `tests/headless_session.rs`, `tests/memory.rs`; `docs/features/osd.md`,
  `docs/features/keyboard.md` (the pill), `docs/configuration.md`

**Interfaces — Consumes:** `Passive<T>`, `Kinds`, `Pill::Layout`, `Pill::Track`, `OsdKind::Layout`
/ `Track` (Task 4); `CompositorState.layout` (Tasks 1, 3); `MediaView { playing, title }`.
**Produces:**
```rust
pub struct TrackWatch { last_title: Option<String>, grace: Option<Instant> }
impl TrackWatch { pub fn hear(&mut self, view: Option<&MediaView>, now: Instant) -> Option<String>; }
pub fn keyboard_wanted(modules: &[ModuleId], osd: &OsdConfig) -> bool; // + osd with Layout in kinds
```
`TrackWatch::hear`: `None` → reset (next view is a baseline with a grace); first view → baseline;
title differs from `last_title` (kept whatever `playing` says) and `playing` and outside the
grace → `Some(title)`; `last_title` always updated.

- [ ] **Step 1: failing tests.**
  - `osd/tests.rs`: `track_first_view_is_the_baseline`, `track_a_new_title_while_playing_shows`,
    `track_a_new_title_while_paused_shows_nothing_and_resume_shows_nothing`
    (the title is learnt while paused), `track_pause_and_resume_of_the_same_title_show_nothing`,
    `track_titles_inside_the_grace_after_a_player_appears_show_nothing`,
    `track_the_player_leaving_takes_a_new_baseline`; `layout_a_new_name_shows_the_full_name`
    (pill text `French (AZERTY)` whatever `[keyboard] short`), `layout_a_hotplug_burst_shows_one_pill_with_the_last_name`
    (three changes in one pass → `Osd::shown` is the last, one surface: the existing replace
    behaviour), `rows_cut_a_long_title_to_the_pill` (`truncate_title`).
  - `startup/tests.rs`: `keyboard_wanted_by_the_layout_kind_alone` (`modules = [osd]`, kinds
    `["layout"]` → true), `layout_kind_without_osd_wants_nothing`.
  - Headless: `osd_a_layout_change_shows_the_pill` (Mango backend, `modules = [osd]` only, kinds
    `["layout"]`, `push_monitors` the `layout-fr` fixture → the pill surface appears; then the same
    under Sway with `xkb_switch_layout`), `osd_a_track_change_while_playing_shows_the_pill` (private
    session bus fake player from `media_private_bus.rs`'s support: title A playing, then B → pill;
    pause/resume B → no second pill), `osd_the_new_kinds_off_cost_no_wakeup` (default kinds, layout
    and track changes pushed → no surface, 0 loop wake-ups in 5 s).
- [ ] **Step 2: run** `nix develop -c cargo test -p alpheus-daemon osd` → failures.
- [ ] **Step 3: implement.** `osd_passive` maps `Msg::Compositor(Some(s))` → `s.layout.clone()`,
  `Msg::Compositor(None)` → None; `Msg::Media(v)` → `TrackWatch::hear(v.as_ref(), now)`.
- [ ] **Step 4: mutate:** drop the `playing` check → `track_a_new_title_while_paused…` fails;
  forget to update `last_title` while paused → the resume test fails. Restore.
- [ ] **Step 5: measure.** `osd_layout_and_track_pills_no_growth` in `tests/memory.rs`: kinds
  `layout`, `track`; 100 layout pushes (Mango) and 100 track changes with pills, anonymous growth
  ≤ 64 KB, PSS printed, 0 loop wake-ups after; plus `osd_kinds_pss_off_and_on` rerun with the two
  kinds added. Release, 3 runs, back to back against the base. Record rows in
  `docs/measurements.md`.
- [ ] **Step 6: docs.** `docs/features/osd.md` (layout and track rules, the title on screen only
  when `track` is listed, never logged), `docs/features/keyboard.md` (the `layout` pill without the
  bar module), `docs/configuration.md`.
- [ ] **Step 7: commit** `feat(osd): pills for a keyboard layout change and a new track`.

### Task 6: Branch proof and the remaining documents

**Files:** `CHANGELOG.md`, `docs/comparison.md`, `docs/measurements.md`, `docs/features/osd.md`,
`docs/features/keyboard.md` (cost sections), `docs/ARCHITECTURE.md` / `docs/tour.md` only if a
sentence there names the sources' `start` signature or the three Sway messages.

- [ ] **Step 1:** `nix develop -c cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace`: clean.
- [ ] **Step 2:** the whole headless suite at `nice -n 19` (`--include-ignored`); `pgrep -u $USER
  -a alpheus` afterwards shows only the owner's daemon.
- [ ] **Step 3: measure** the branch as a whole against the base, release, 3 runs:
  `startup_pss_per_source` (all cases, plus `+ keyboard` and `+ osd kinds`),
  `pss_under_budget_and_stable`, `at_rest_no_wakeups_under_sway_and_mango`; everything stays
  under 15 MB at rest. A total-PSS failure is rerun alone three times with `--exact`. Record the
  branch row (linked-in cost, binary size delta stripped) in `docs/measurements.md` and the cost
  sections of both feature pages.
- [ ] **Step 4:** `CHANGELOG.md` (Unreleased: the module, the kinds, Sway's `GET_INPUTS`),
  `docs/comparison.md` rows (keyboard layout, OSD kinds).
- [ ] **Step 5: commit** `docs(osd,keyboard): costs, changelog and comparison for the branch`.
