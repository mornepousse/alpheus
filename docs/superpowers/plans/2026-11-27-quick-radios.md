# Quick settings: keyboard backlight and airplane mode — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two rows in the quick panel: a stepped keyboard backlight slider and an airplane mode
switch, read from sysfs when the panel opens, written through `brightnessctl` and `rfkill`.

**Architecture:** Two read-only sysfs readers next to `backlight.rs` (`kbd_backlight.rs`,
`rfkill.rs`); two `Row`s in the pure model `quick/mod.rs`, drawn in `quick/draw.rs` and wired in
`quick/app.rs` like the brightness row and the configured toggles: built at open, re-read by the
panel's one timer, dropped at close. Commands are `QuickAction::Run` through the executor.

**Tech Stack:** Rust, the daemon's quick panel (`Quick`, `ROWS`, `Pace`, `Due`), `read_capped`, the
executor. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-27-quick-radios-design.md`.

## Global Constraints

- Every cargo command: `nix develop -c …`; the headless and memory suites at `nice -n 19`.
- No new dependency, thread, timer, fd or polling; nothing of either row survives a close; a key
  set to `false` reads nothing.
- **Never run `brightnessctl` or `rfkill` for real**, nor any session tool: `FakeExecutor` in unit
  tests, `fake_recorder` on the harness's private `PATH` in headless tests. `rfkill block wlan`
  acts by type, whatever `rfkill_dir` says: a fake directory is no protection, only the private
  `PATH` is. Never touch the owner's daemon; `pgrep` after a headless run, kill only your own PIDs.
- Test bars never read the real `/sys/class/leds` or `/sys/class/rfkill`: `harness_bar_toml`
  injects fake empty directories (only for a non-base build: a base build refuses unknown keys).
- Logs are fixed words: never a device name, an rfkill index, an argv.
- Tests first; each new test is mutated once (break the code, watch it fail) before the commit.
- Never loosen a guardrail test. Adding the new exact command shapes to
  `every_quick_command_stays_on_the_allowlist` is extending it, not loosening it; nothing else in it
  changes.
- No `unwrap`, `expect` or `panic!` in production code of the new files and of `quick/`.
- English everywhere. One logical change per commit, `type(scope):` subject, body says why,
  trailers `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and the session line.
- Stage by name; read `git diff --cached` before every commit; never `flake.lock`.
- **Every task ends with a RAM measurement of each part it touches, recorded in
  `docs/measurements.md`**: the daemon through `tests/memory.rs`, release, back to back against
  `main` (07227887, or the merge base at the time) built in its own target directory, 3 runs each
  (method in `docs/measurements.md`). `brightnessctl` and `rfkill` are external system tools that
  tests never run: the row says so instead of a figure.

## Review Focus

1. **A headless test reaching the real `rfkill` or `brightnessctl`** (a key walk over every row,
   now with an airplane row in it): the owner's radios cut → Task 2
   (`coreutils_dir` asserts no `rfkill`/`brightnessctl`; `harness_bar_toml` injects fake dirs;
   `quick_airplane_space_blocks_through_rfkill` uses a recorder) and Task 1 for `brightnessctl`.
2. **The wrong LED matched** (`input3::capslock`, `platform::micmute`, a zero or huge max): a
   capslock LED driven as a backlight → Task 1 (`kbd_backlight_reads_level_and_max`).
3. **Airplane state wrong with a hard-blocked radio** (a wlan kill switch on, Bluetooth soft-on):
   the row must read off and stay actionable; all hard-blocked: `hard blocked`, Space inert → Task 2
   (`airplane_state_from_sysfs`, `airplane_space_does_nothing_when_hard_blocked`).
4. **A held arrow on a 255-level keyboard**: one `brightnessctl` per 150 ms window, the last level
   always sent, never lost at close → Task 1 (`kbd_backlight_is_paced_and_flushed`).
5. **The airplane row contradicting a radio switched from its own row** (Wi-Fi Space while airplane
   on) → Task 2 (`a_radio_row_switch_rereads_airplane`).

---

### Task 1: The keyboard backlight row

**Files:**
- Create: `crates/alpheus-daemon/src/kbd_backlight.rs` (+ its `#[cfg(test)] mod tests`)
- Modify: `crates/alpheus-daemon/src/lib.rs` (module), `src/config.rs` (+ `config/tests.rs`),
  `src/quick/mod.rs`, `src/quick/draw.rs`, `src/quick/app.rs`, `src/quick/tests.rs`,
  `tests/guardrails.rs`, `tests/common/mod.rs`, `tests/memory.rs`
- Docs: `docs/configuration.md`, `docs/features/quick-settings.md`, `docs/measurements.md`

**Interfaces — Produces:**
```rust
// kbd_backlight.rs
pub const SUFFIX: &str = "::kbd_backlight";
pub const MAX_LEVEL: u8 = 255;            // a larger max_brightness hides the row
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kbd { pub device: String, pub level: u8, pub max: u8 }
impl Kbd {
    pub fn percent(&self) -> u8;                       // round(level * 100 / max)
    pub fn stepped(&self, up: bool, step_pct: u8) -> u8; // ±1 when max <= 10, else ±max(1, round(max*step/100)); clamped 0..=max
    pub fn nearest(&self, percent: u8) -> u8;          // round(percent * max / 100)
    pub fn cycled(&self) -> u8;                        // level + 1, max wraps to 0
}
pub fn find(dir: &Path) -> Option<Kbd>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdReread { Level(u8), Unreadable, Gone }
pub fn reread(dir: &Path, device: &str) -> KbdReread;
// config.rs, QuickConfig
pub kbd_backlight: bool,          // default true
pub kbd_backlight_dir: PathBuf,   // default "/sys/class/leds", absolute
// quick/mod.rs
Row::KbdBacklight                 // group 0, right after Row::Brightness; ROWS: 15 entries
Quick { kbd: Option<Kbd>, kbd_pace: Pace, kbd_settle: Option<Instant>, .. }
impl Quick { pub fn set_kbd(&mut self, k: Option<Kbd>); pub fn set_kbd_reread(&mut self, r: KbdReread); }
Due { kbd: bool, .. }
fn kbd_command(k: &Kbd) -> QuickAction; // ["brightnessctl","--class=leds","--device=<name>","set","<N>"], what "keyboard backlight <N>"
```
**Consumes:** `backlight::MAX_ENTRIES`, `read_capped`, `Pace`, `COALESCE`, `SETTLE`, `slider`,
`bar_value`, `spawn_logged`.

- [ ] **Failing tests, reader** (`kbd_backlight.rs`, fake dirs under `temp_dir`, never `/sys`):
  - `kbd_backlight_row_hidden_without_a_led`: empty dir, missing dir, only `input0::capslock` → `find` is `None`.
  - `kbd_backlight_reads_level_and_max`: `tpacpi::kbd_backlight` (1/2) beside `input0::capslock` → `Some(Kbd { device: "tpacpi::kbd_backlight", level: 1, max: 2 })`; two matches → the first sorted; max `0`, `256`, words, a FIFO → `None`; a level above max is clamped to max.
  - `kbd_backlight_reread_tells_gone_from_unreadable`: as `backlight.rs`'s re-read test.
  - `kbd_backlight_steps_by_level_or_by_percent`: max 2: `stepped(true,5)` from 1 → 2, at 2 → 2, down from 0 → 0; max 255, step 5 → ±13; `nearest(50)` on max 2 → 1; `percent()` 1/2 → 50.
  - `kbd_backlight_cycles_and_allows_zero`: 0→1→2→0 on max 2.
- [ ] Run `nix develop -c cargo test -q -p alpheus-daemon kbd_backlight` → fails to compile (no module).
- [ ] **Failing tests, config**: `kbd_backlight_keys_default_and_parse` (defaults `true`,
  `/sys/class/leds`; `kbd_backlight = false` parses); `a_relative_kbd_backlight_dir_is_refused`
  (message names `quick.kbd_backlight_dir`).
- [ ] **Failing tests, model** (`quick/tests.rs`, `FakeExecutor` nowhere needed: actions are values):
  - `kbd_backlight_row_shows_only_with_a_device`: `set_kbd(None)` → no `Row::KbdBacklight` in `shown`; `Some` → right after `Brightness` (or first of group 0 without a backlight).
  - `kbd_backlight_sets_with_brightnessctl_leds_class`: Right on the row → `Run { argv: ["brightnessctl","--class=leds","--device=tpacpi::kbd_backlight","set","2"], what: "keyboard backlight 2" }`; Left to `0` sends `set 0`.
  - `kbd_backlight_enter_and_space_cycle`: Enter at 2 → `set 0`.
  - `kbd_backlight_press_on_the_bar_sets_the_nearest_level`.
  - `kbd_backlight_is_paced_and_flushed`: max 255, five Rights within `COALESCE` → one command now, the last level from `due(t + COALESCE)`, and `flush()` sends a waiting level.
  - `kbd_backlight_logs_no_device`: every `what` the row produces contains no `kbd_backlight` nor `tpacpi`.
  - `kbd_backlight_reread_gone_hides_the_row`: `set_kbd_reread(Gone)` → row gone, selection falls back.
- [ ] Run `nix develop -c cargo test -q -p alpheus-daemon quick::` → the new tests fail (no row).
- [ ] **Implement** `kbd_backlight.rs` (header in the house style: what, Never, See), the config
  fields (+ `check_quick` absolute path, + the `//! [quick]` header lines), the row (`ROWS`,
  `Row::name` `"kbd-backlight"`, `act`/`press`/`scroll`/`due`/`flush` arms mirroring brightness
  but in levels, `draw_kbd` via `slider(g, glyph, Volume { percent: k.percent(), muted: false }, …)`),
  the wiring (`quick_toggle` calls `kbd_backlight::find` when `kbd_backlight` is on and
  `q.set_kbd(..)`; `quick_tick` re-reads on `due.kbd`).
- [ ] **Guardrails** (`tests/guardrails.rs`): add `kbd_backlight.rs` to `NO_THREAD_NOR_COMMAND`,
  `OSD_FILES`' sibling read-only check (a new `the_radio_readers_only_read` with the same `never`
  list as `the_quick_model_and_its_state_reader_only_read`, listing `kbd_backlight.rs`) and a
  no-panic check; extend `every_quick_command_stays_on_the_allowlist`: `fresh()` calls
  `q.set_kbd(Some(Kbd { device: "fake::kbd_backlight".into(), level: 1, max: 2 }))`; new shape
  `["brightnessctl", "--class=leds", "--device=fake::kbd_backlight", "set", n]` with `n` in `0..=2`;
  `"brightnessctl --class=leds --device=fake::kbd_backlight set 2"` and `… set 0` in the reached list.
- [ ] **Harness** (`tests/common/mod.rs`): `harness_bar_toml` adds, when `!base` and the text has no
  `kbd_backlight_dir`, `kbd_backlight_dir = "<runtime>/quick-leds"` under `[quick]` (created empty
  in `start_daemon_with`); `coreutils_dir()` asserts no `brightnessctl` in it.
- [ ] **Mutate**: drop the `ends_with(SUFFIX)` filter → `kbd_backlight_reads_level_and_max` fails;
  send `level` before clamping → `kbd_backlight_sets_with…` fails. Restore.
- [ ] `nix develop -c cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` clean.
- [ ] **MEASUREMENT**: add `quick_pss_with_the_radio_rows` to `tests/memory.rs`: one daemon with a
  fake leds dir holding `fake::kbd_backlight` (1/2) and `[quick] kbd_backlight = true`, one with
  `false`; open/close the panel by the verb as `quick_pss_idle_open_and_closed`; print rest, open,
  closed, 20 cycles; assert open-with − open-without ≤ 64 KB and anonymous closed ≤ rest + 256 KB.
  Then, back to back against `main` in its own target dir, 3 runs each:
  `quick_pss_idle_open_and_closed` and `startup_pss_per_source` (`everything`: linked in, unused).
  Command: `nice -n 19 nix develop -c cargo test -q --release -p alpheus-daemon --test memory -- --include-ignored --test-threads=1 --exact <name>`.
  Record rows "Quick settings' keyboard backlight, linked in" and "Quick settings, keyboard
  backlight row shown" in `docs/measurements.md`, plus the binary size (stripped) both sides.
- [ ] **Docs**: `docs/configuration.md` (`[quick]` table: `kbd_backlight`, `kbd_backlight_dir`);
  `docs/features/quick-settings.md` (What it does: the row; Keys; Configuration; How it is built:
  `kbd_backlight.rs`; Rules; Cost with the figures; "Not yet verified: logind `SetBrightness` on
  `leds` through brightnessctl").
- [ ] **Commit** `feat(quick): a keyboard backlight row`.

### Task 2: The airplane mode row

**Files:**
- Create: `crates/alpheus-daemon/src/rfkill.rs` (+ tests)
- Modify: `src/lib.rs`, `src/config.rs` (+ tests), `src/quick/mod.rs`, `src/quick/draw.rs`,
  `src/quick/app.rs`, `src/quick/tests.rs`, `tests/guardrails.rs`, `tests/common/mod.rs`,
  `tests/memory.rs`
- Docs: `docs/configuration.md`, `docs/features/quick-settings.md`, `docs/measurements.md`, `docs/install.md`

**Interfaces — Produces:**
```rust
// rfkill.rs
pub const MAX_ENTRIES: usize = 32;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Wlan, Bluetooth, Wwan }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Radio { pub kind: Kind, pub soft: bool, pub hard: bool }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Radios(pub Vec<Radio>);   // never empty
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Airplane { On, Off, HardBlocked }
impl Radios { pub fn state(&self) -> Airplane; } // usable = !hard; none usable → HardBlocked; all usable soft → On; else Off
pub fn read(dir: &Path) -> Option<Radios>;       // None: no radio of the three kinds
// config.rs, QuickConfig
pub airplane: bool,               // default true
pub rfkill_dir: PathBuf,          // default "/sys/class/rfkill", absolute
// quick/mod.rs
Row::Airplane                     // group 1, right before Row::Wifi; ROWS: 16 entries
Quick { airplane: Option<Airplane>, reread_airplane: Option<Instant>, .. }
impl Quick { pub fn set_airplane(&mut self, a: Option<Airplane>); }
Due { airplane: bool, .. }
fn airplane_command(block: bool) -> QuickAction; // ["rfkill","block"|"unblock","wlan","bluetooth","wwan"], what "airplane on"|"airplane off"
```
**Consumes:** `read_capped`, `TOGGLE_REREAD`, `draw_mark`, `text_row`, `write_cell`, the Wi-Fi row's
radio command and `Bt::toggle`'s `rfkill unblock bluetooth` (both on main).

- [ ] **Failing tests, reader** (`rfkill.rs`, fake dirs `rfkill0/{type,soft,hard}`):
  - `airplane_hidden_without_a_radio`: empty dir, missing dir, only `type = gps`/`nfc` → `None`.
  - `airplane_state_from_sysfs`: wlan+bt both soft 1 → `On`; one soft 0 → `Off`; wlan hard 1 + bt soft 1 → `On`; wlan hard 1 + bt soft 0 → `Off`; all hard → `HardBlocked`; `gps` soft 0 ignored; a `soft` reading `2`, words or a FIFO → that entry skipped; 40 entries → only 32 read.
- [ ] **Failing tests, config**: `airplane_keys_default_and_parse`, `a_relative_rfkill_dir_is_refused`.
- [ ] **Failing tests, model**:
  - `airplane_row_sits_above_wifi`: with the Wi-Fi row shown, `Airplane` is right before it in `shown`.
  - `airplane_toggle_runs_one_rfkill_for_the_three_types`: Space when `Off` → `Run { argv: ["rfkill","block","wlan","bluetooth","wwan"], what: "airplane on" }`; when `On` → `unblock`, `"airplane off"`; Enter acts as Space; a re-read is due at `now + TOGGLE_REREAD`.
  - `airplane_space_does_nothing_when_hard_blocked`: `HardBlocked` → `Nothing`, drawn `hard blocked`.
  - `a_radio_row_switch_rereads_airplane`: Space on the Wi-Fi row (radio command) and on a blocked Bluetooth row (`rfkill unblock bluetooth`) both set `reread_airplane`; `due(now + TOGGLE_REREAD).airplane` is true.
  - `airplane_off_reads_nothing`: with `airplane = false` the row never shows (the wiring test below checks no read).
- [ ] Run `nix develop -c cargo test -q -p alpheus-daemon rfkill quick::` → fails (no module, no row).
- [ ] **Implement** `rfkill.rs` (read only, header: what, Never, See), the config fields and header
  lines, the row (`ROWS`, name `"airplane"`, `act` arm, `due`, `set_airplane`, `draw_airplane`),
  the wiring (open: `rfkill::read(&q.rfkill_dir).map(|r| r.state())` only when `airplane` is on;
  `quick_tick` re-reads on `due.airplane`; a read giving `None` hides the row).
- [ ] **Guardrails**: `rfkill.rs` on `NO_THREAD_NOR_COMMAND`, `the_radio_readers_only_read` and the
  no-panic list. `every_quick_command_stays_on_the_allowlist`: `fresh()` sets
  `set_airplane(Some(Airplane::Off))`, and a second fixture with `On`; new shapes
  `["rfkill", "block" | "unblock", "wlan", "bluetooth", "wwan"]`; both in the reached list.
- [ ] **Harness** (session safety, Review Focus 1): `harness_bar_toml` injects
  `rfkill_dir = "<runtime>/quick-rfkill"` (empty, created in `start_daemon_with`) when `!base` and
  absent; `coreutils_dir()` asserts no `rfkill` in it. Grep `tests/` for every bar text that sets
  `[quick]` and check each goes through `harness_bar_toml` (report the list).
- [ ] **Mutate**: count hard-blocked radios as usable → `airplane_state_from_sysfs` fails; drop the
  re-read from the Wi-Fi row → `a_radio_row_switch_rereads_airplane` fails. Restore.
- [ ] fmt, clippy `-D warnings`, `cargo test --workspace` clean.
- [ ] **MEASUREMENT**: extend `quick_pss_with_the_radio_rows` with a fake rfkill dir (`rfkill0` wlan,
  `rfkill1` bluetooth) and `airplane = true` vs `false`; same bounds. Back to back against `main`,
  3 runs: `quick_pss_idle_open_and_closed`, `startup_pss_per_source` (`everything`), binary size.
  Record "Quick settings' airplane mode, linked in" and "Quick settings, airplane row shown" in
  `docs/measurements.md`; note `rfkill` itself is an external tool never run by the tests.
- [ ] **Docs**: `docs/configuration.md` (`airplane`, `rfkill_dir`); `docs/features/quick-settings.md`
  (the row, `hard blocked`, Wi-Fi and Bluetooth follow through their feeds, leaving unblocks all
  three types, rules, cost, "Not yet verified: `rfkill` unprivileged through uaccess");
  `docs/install.md` (`rfkill` also for airplane mode).
- [ ] **Commit** `feat(quick): an airplane mode row`.

### Task 3: Headless proof, decisions and changelog

**Files:** `crates/alpheus-daemon/tests/headless_session.rs`, `docs/decisions.md`, `CHANGELOG.md`,
`docs/measurements.md`, `docs/features/quick-settings.md`

**Consumes:** `fake_recorder`, `harness_bar_toml`, the rows of Tasks 1–2, `Request::Quick(QuickVerb::Sound)`.

- [ ] **Failing headless tests** (`#[ignore]`, isolated harness, recorders on the private `PATH`
  *before* any key is sent; the test asserts `which`-style that `<runtime>/bin/rfkill` and
  `<runtime>/bin/brightnessctl` are the recorders):
  - `quick_kbd_backlight_right_sends_brightnessctl_leds`: fake leds dir `fake::kbd_backlight` (0/2), open by the verb, select the row, Right → `calls-brightnessctl` reads `brightnessctl --class=leds --device=fake::kbd_backlight set 1`; the log holds `quick: keyboard backlight 1` and no `fake::`.
  - `quick_airplane_space_blocks_through_rfkill`: fake rfkill dir (wlan, bluetooth, soft 0), Space on the row → `calls-rfkill` reads `rfkill block wlan bluetooth wwan`; the log holds `quick: airplane on`.
  - `at_rest_no_wakeups_after_the_radio_rows`: after both, the panel closed, 0 loop and 0 whole-daemon wake-ups in 5 s.
- [ ] Run `nice -n 19 nix develop -c cargo test -q -p alpheus-daemon --test headless_session -- --include-ignored quick_` with `kbd_backlight = false` / `airplane = false` in the test bar first → they fail (no row, nothing recorded); then with the keys on → they pass. Mutate: change the airplane argv's order → the second fails. Restore. `pgrep -u "$USER" -a sway` / `alpheus-demon`: only the owner's processes remain.
- [ ] **MEASUREMENT**: re-run `quick_pss_with_the_radio_rows` (both rows), `quick_pss_idle_open_and_closed`
  and `at_rest_no_wakeups_after_the_quick_panel` back to back against `main`, 3 runs; update the two
  rows' figures in `docs/measurements.md` and the feature page's cost section.
- [ ] **Decisions** (`docs/decisions.md`, appended): "Airplane mode blocks Wi-Fi, Bluetooth and WWAN
  through rfkill, and leaving it unblocks all three" (why: decisions 1–2, rfkill over NM/BlueZ and
  `/dev/rfkill`); "The keyboard backlight steps through the device's own levels, and 0 is allowed"
  (why: decision 3, unlike the screen's 1 % floor).
- [ ] **CHANGELOG** under Unreleased › 2026-11 › Added: both rows, keys, cost against main, links
  to the page and the configuration.
- [ ] fmt, clippy, workspace tests clean.
- [ ] **Commits** `test(quick): the radio rows on a headless session`, then
  `docs(quick): keyboard backlight and airplane mode decisions and changelog`.

## Self-review

- Spec coverage: rows, keys, defaults, files, re-read after radio rows, hard block, cost, tests,
  not-yet-verified items: Tasks 1–3. Night light: non-goal, no task.
- Types: `Kbd`, `KbdReread`, `Radios`, `Airplane`, `set_kbd`, `set_airplane`, `Due.kbd`,
  `Due.airplane` are the same names in the spec and every task.
- Order: Task 2 depends on Task 1 only for the shared guardrail `the_radio_readers_only_read` and
  `quick_pss_with_the_radio_rows`; Task 3 on both.
