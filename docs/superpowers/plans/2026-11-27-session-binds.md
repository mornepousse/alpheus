# Session menu rows and keybinding cheatsheet Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A keybinding cheatsheet as a level of the Super+X command panel (Mango, then Sway), and
a configurable ⏻ menu: user rows, digits, an optional countdown.

**Architecture:** A pure parser folder `launcher/tools/binds/` reads the compositor's config
files through a `Files` seam; the glue (`launcher/tools/app.rs`) does the one synchronous read
when the level opens; `nav.rs` holds the level (`Level::Binds`), whose rows feed the existing
Tools ranker; `view/binds.rs` draws it with the panel's pieces. The ⏻ menu gains `PowerEntry`
(a fixed `Session` or a user row of `[power]`), digits, and a countdown kept in the menu's state
and its one `DeadlineTimer`.

**Tech Stack:** Rust, the daemon's `Grid`, `Boxed`, shapes and painter, `serde`/`toml` already in
the tree. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-27-session-binds-design.md`.

## Global Constraints

- Every cargo command in `nix develop -c …`; heavy suites (headless, memory) at `nice -n 19`.
- No new dependency, thread, timer or polling. The cheatsheet's read is synchronous in the glue,
  at open only; the countdown reuses `power_timer`.
- Tests first, run red for the right reason, then the least code, then a mutation that turns the
  test red, restored. Name the mutation in the report.
- Never a real session tool or the user's files in a test: `FakeExecutor`, the `Files` seam
  (`FakeFiles`), temporary directories, the headless harness (`tests/common`: cleared environment,
  temporary `XDG_RUNTIME_DIR` and `XDG_CONFIG_HOME`). `pgrep` after a headless run: nothing of
  yours left. No `pkill`/`killall`; kill only a PID you started.
- Never run `alpheus`, `alpheus-demon` or `alpheus-launch` on the session; never read or edit the
  owner's `~/.config` (fixtures are invented: neutral programs `foo-term`, `browser`, hosts
  `host-a`).
- Logs are fixed words, positions and counts: never a key, combo, command, path, file name, group
  name or row name. A new log line is added to its feature's guardrail.
- Never loosen a guardrail; adapting one to new code keeps it at least as strict, in the same
  commit, and the report says how. `FORBIDDEN_WORDS` (`swaymsg`, `mmsg`, …) stays untouched.
- No sleep: no row, built-in or user, may run `suspend`, `hibernate`, `hybrid-sleep` or `dpms`.
- English everywhere; one logical change per commit, `type(scope): …`, body says why, trailers
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` then the session line. Stage by name,
  read `git diff --cached`, never `flake.lock`.
- Work on `feat/session-binds` in `.worktrees/session-binds`; scratch files in your own `mktemp -d`.
- `nix develop -c cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  warnings` and `cargo test --workspace` clean at every commit.
- **Every task ends with a RAM measurement of each part it touches, recorded in
  `docs/measurements.md`**: the daemon through `tests/memory.rs` in release, back to back against
  `main` built in its own target directory (`docs/measurements.md`, "Method"), 3 runs each,
  deltas reported. A task with no measurement is not done. 15 MB resident at rest for everything.
- The command panel's look is being polished on another branch: build on its model and render
  functions (`view/panel.rs`, `view.rs`'s `tools_chip`, `key_hints`, `ring_shape`), never on pixel
  details; rebase conflicts in `view/panel.rs` are resolved in favour of the polish branch's look.

## Review Focus

1. **A user row that sleeps, or a row run without its question** (a `confirm` row, Reboot, Power
   off reached by a digit): only a configured argv runs, never a sleep word, never without the
   0.4 s second activation → Task 6 (`every_power_menu_activation_is_a_configured_session`
   extended, `a_power_entry_naming_sleep_makes_bar_toml_invalid`) and Task 7
   (`a_digit_on_a_row_that_asks_needs_the_same_digit_again`).
2. **A countdown that outlives the menu or cannot be cancelled**: Esc, any close and a hover on
   another row drop it; the timer goes with the popup → Task 8 (`esc_cancels_the_countdown_and_runs_nothing`,
   `power_countdown_closed_popup_disarms_the_timer`, headless `power_countdown_escape_runs_nothing`).
3. **An include/source cycle or a huge config** hanging or bloating the daemon: 8 files, 512 KiB,
   1,000 binds, 32 groups, 1 KiB lines, a file seen twice read once → Task 1
   (`binds_include_cycle_and_depth_are_bounded`) and Task 4 (`sway_include_glob_and_cycle_are_bounded`).
4. **A bind's command, combo or path in a log line**, or the parser reading/writing beyond its
   seam → Task 1 (`the_binds_folder_is_pure`), Task 2 (`binds_logs_are_fixed_words_and_counts`).
5. **The level leaking past its box**: the parsed binds kept after Esc or close, a letter running a
   tool from the cheatsheet, Esc from `--binds` landing on an empty panel → Task 2
   (`the_binds_level_is_dropped_on_up_and_on_close`, `letters_are_text_in_the_binds_level`) and
   Task 5 (`esc_on_a_binds_open_closes_the_box`).

---

### Task 1: The binds parser's core and Mango

**Files:** Create `crates/alpheus-daemon/src/launcher/tools/binds/mod.rs`, `binds/mango.rs`,
`binds/describe.rs`, `binds/tests.rs`, fixtures under `binds/fixtures/mango/` (invented:
`config.conf` sourcing `keys.conf` and `extra.conf`); modify `launcher/tools/mod.rs` (`pub mod
binds;`), `tests/guardrails.rs` (pure-file list).

**Interfaces — Produces:**
```rust
pub enum Dialect { Mango, Sway }
pub enum Roots { Each(Vec<PathBuf>), FirstOf(Vec<PathBuf>) } // binds_files: Each; defaults: FirstOf
pub struct Bind { pub combo: String, pub description: String, pub command: String, pub group: usize, pub file: usize }
pub struct Cut { pub files: usize, pub bytes: usize, pub binds: usize, pub groups: usize, pub lines: usize }
pub struct Binds { pub groups: Vec<String>, pub binds: Vec<Bind>, pub files: Vec<String> /* last path component */, pub not_understood: usize, pub found: bool, pub cut: Cut }
pub trait Files { fn read(&self, path: &Path, max: usize) -> Option<String>; fn list(&self, dir: &Path) -> Vec<PathBuf>; }
pub const MAX_FILES: usize = 8; pub const MAX_BYTES: usize = 512 * 1024;
pub const MAX_BINDS: usize = 1000; pub const MAX_GROUPS: usize = 32; pub const MAX_LINE: usize = 1024;
pub fn roots(dialect: Dialect, var: impl Fn(&str) -> Option<OsString>, binds_files: &[String]) -> Roots;
pub fn load(dialect: Dialect, roots: &Roots, files: &dyn Files) -> Binds;
// describe.rs
pub fn combo(dialect: Dialect, mods: &str, key: &str) -> String;           // "SUPER+SHIFT","a" -> "Super+Shift+A"
pub fn humanise(dialect: Dialect, function: &str, args: &str) -> String;   // "spawn","firefox" -> "Run firefox"
```
`Dialect::Sway` parses nothing yet (an empty `Binds` with `found` as read); Task 4 fills it.

- [ ] Failing tests (`binds/tests.rs`, a `FakeFiles` map of path → text):
  `binds_mango_parses_bind_bindl_bindsl_and_source_chain` (fixture: 3 files, `source=./keys.conf`
  relative to the including file; 12 binds in file order, `bindl`/`bindsl`/`bindr` kept,
  `mousebind`/`axisbind`/`gesturebind` absent and not counted as not understood, `code:38` kept as
  written); `binds_section_comments_become_groups` (`# ── Windows ──` opens group "Windows"; binds
  before any header in group `General`); `binds_description_prefers_trailing_then_above_then_humanised`
  (trailing `# open term` wins; a comment line right above wins next; not across a blank line, not a
  header; else `Run browser`, `Go to desktop 2`, `Close window`; unknown `fancyfn,x` → `fancyfn x`);
  `env_variables_of_the_chain_expand_others_stay` (`env=TERMINAL,foo-term` makes `$TERMINAL` →
  `foo-term`; `$UNSET` kept); `combos_print_in_title_case` (`SUPER+SHIFT,a` → `Super+Shift+A`,
  `NONE,XF86AudioMute` → `XF86AudioMute`, `ALT,Tab` → `Alt+Tab`, `CTRL` → `Ctrl`);
  `a_line_not_understood_is_counted` (`bind=SUPER` alone → `not_understood == 1`);
  `binds_include_cycle_and_depth_are_bounded` (a.conf sources b.conf sources a.conf: each read once;
  10 chained files → 8 read, `cut.files == 2`; 1,200 binds → 1,000 kept, `cut.binds == 200`; a
  2 KiB line cut at 1 KiB, counted in `cut.lines`; 600 KiB total → reads stop at 512 KiB, `cut.bytes`);
  `no_root_readable_means_not_found` (`found == false`, no bind); `roots_follow_xdg_then_home`
  (`XDG_CONFIG_HOME=/x` → `FirstOf([/x/mango/config.conf])`; unset → `$HOME/.config/…`;
  `binds_files = ["~/k.conf"]` → `Each([$HOME/k.conf])`).
- [ ] Guardrail: `the_binds_folder_is_pure` in `tests/guardrails.rs`: no file under
  `launcher/tools/binds/` contains `File::create`, `OpenOptions`, `std::fs::`, `Command`,
  `thread::`, `log(` or a panicking call in production code; add the folder to
  `the_tools_pure_parts_have_no_panicking_path_and_log_nothing`'s list.
- [ ] Run `nix develop -c cargo test -p alpheus-daemon binds` → fails to compile (no module).
- [ ] Implement: `load` walks roots breadth-first in file order with a `seen` set of paths;
  `mango::parse(text, here, st)` returns the `source=` paths it found; the humanised table covers
  at least `spawn`, `spawn_shell`, `view`, `tag`, `tagsilent`, `focusdir`, `focusmon`, `tagmon`,
  `exchange_client`, `resizewin`, `killclient`, `togglefullscreen`, `togglemaximizescreen`,
  `togglefloating`, `toggleoverview`, `switch_layout`, `viewtoleft`, `viewtoright`, `tagtoleft`,
  `tagtoright`, `reload_config`, `quit`.
- [ ] Mutate: drop the `seen` check → `binds_include_cycle_and_depth_are_bounded` fails; prefer the
  line above over the trailing comment → the description test fails.
- [ ] MEASUREMENT: the module is not linked into any path yet: record the stripped binary size of
  `alpheus-demon` (`nix build .#alpheus-demon`, `stat -c %s`) against `main`, and
  `pss_under_budget_and_stable` + `at_rest_no_wakeups` back to back against `main` (3 runs each),
  in `docs/measurements.md` under a new "Session menu and keybindings" heading.
- [ ] Docs: none user-facing yet (no entry shows it).
- [ ] Commit `feat(binds): parse Mango's key bindings from its config files`.

### Task 2: The Keybindings level in the Tools model

**Files:** `launcher/tools/catalog.rs` (`BuiltinAction::Binds`, built-in `tools.binds`, System,
glyph `\u{f030c}` keyboard, description `Show the compositor's key bindings`, `Context.binds`),
`launcher/tools/config.rs` (`ToolsConfig.binds_files`), `launcher/tools/nav.rs`
(`Level::Binds`, `RowId::Bind`, `Step::Binds`, the held binds and their tab),
`launcher/tools/panel.rs` (`Label::Group(usize)`), `launcher/tools/binds/mod.rs` (`rows`,
`group_hits`), `launcher/model.rs` (`Outcome::OpenBinds`, `Launcher::open_binds`, Tab at the
level, rank grouping), `launcher/tools/app.rs` (`RealFiles`, the load, the dialect, `context`),
tests in `nav/tests.rs`, `model/tests.rs`, `tools/app/tests.rs`, `binds/tests.rs`;
`tests/guardrails.rs` (log lines).

**Interfaces — Consumes:** Task 1's `Binds`, `load`, `roots`, `Files`, `Dialect`.
**Produces:**
```rust
// nav.rs
Level::Binds; RowId::Bind(usize /* index into Binds::binds */); Step::Binds;
impl ToolsState {
    pub fn open_binds(&mut self, binds: Binds, selected: usize);   // pushes the level (stack empty when opened by --binds)
    pub fn binds(&self) -> Option<&Binds>;                          // Some only at Level::Binds
    pub fn bind_tab(&self) -> usize;                                // 0 = All, k = group k-1
    pub fn next_bind_tab(&mut self, step: i32) -> bool;             // wraps; false with one group
}
// binds/mod.rs
pub fn rows(binds: &Binds, tab: usize) -> Vec<PanelRow>;           // All: Section(Label::Group(g)) + Tool(bind) per group; tab k: group k-1's Tool rows
pub fn group_hits(hits: &[Hit], binds: &Binds) -> Vec<Hit>;        // stable: groups by best rank, hits by rank
// model.rs
Outcome::OpenBinds; impl Launcher { pub fn open_binds(&mut self, binds: Binds); pub fn at_binds(&self) -> bool; }
// tools/app.rs
pub(crate) struct RealFiles;  impl Files for RealFiles  // File::open + take(max), read_dir sorted
pub(crate) fn binds_dialect(c: Option<&Compositor>) -> Option<Dialect>  // Mango, Sway; else None
ToolsConfig { binds_files: Vec<String> /* at most 8, none blank */, .. }
Context { binds: bool /* dialect known */, .. }
```
At `Level::Binds`, `ToolsState::rows()` returns one `ToolRow { id: RowId::Bind(i), icon: None,
text: "<combo>  <description>", trail: Trail::Category(<group>) }` per bind of the open tab, and
`hit_row` gives `ToolHitRow { name: description, path_text: "<combo> <command>", key: "",
parent: None, category: None }`: the existing Tools ranker searches it, and the plain list draws
it (the permanent look with `[tools] panel = false`, the interim one until Task 3).

- [ ] Failing tests: `the_binds_builtin_shows_only_with_a_known_dialect` (Context.binds false →
  hidden); `binds_files_holds_at_most_8_paths_none_blank` (9 → invalid, `""` → invalid);
  `enter_on_the_binds_builtin_opens_the_level` (`Step::Binds`, model answers `Outcome::OpenBinds`,
  no `Activation`); `open_binds_lists_all_then_a_group_tab` (`rows(binds, 0)` sections in group
  order, empty groups absent; tab 2 → only group 1's binds, no section);
  `tab_and_shift_tab_switch_bind_tabs_and_wrap`; `typing_filters_the_binds_and_groups_results`
  (query `term` → description hits first, grouped by group in best-rank order);
  `letters_are_text_in_the_binds_level` (`n` with a tool lettered `n` at home → query `n`, no
  `Activation`); `enter_and_right_on_a_bind_do_nothing` (`Outcome::Nothing`);
  `the_binds_level_is_dropped_on_up_and_on_close` (`binds()` None after Esc, after ← on an empty
  query, after `reset()`, and the
  `ToolsState` drop; the panel's home restored with its tab); `a_glue_load_reads_through_real_files_only_once`
  (`tools/app/tests.rs`: a temp dir fixture, `RealFiles` read count via a wrapper `Files` = one
  `load` per open); `binds_logs_are_fixed_words_and_counts` (guardrails: the glue logs `tools: binds
  shown (N binds, M groups)`, `tools: binds N lines not understood`, `tools: binds cut (files F,
  bytes B, binds N, groups G, lines L)`, `tools: binds no source`; no `{}` of a path, combo,
  command or group name).
- [ ] Run `nix develop -c cargo test -p alpheus-daemon binds_ tools` → the new tests fail (no
  variant, no method).
- [ ] Implement; the glue detects the dialect at load (`alpheus_common::compositor::detect(|k|
  std::env::var_os(k))`), reads synchronously, then `open.model.open_binds(binds)` and redraws.
- [ ] Mutate: skip the drop in `up()` for `Level::Binds` → the drop test fails; let letters live
  below the root → the letters test fails.
- [ ] MEASUREMENT: add `binds_pss_open_and_closed` to `tests/memory.rs` (release, the harness
  daemon with `binds_files` pointing at a 100-bind invented fixture in its temp dir; open Tools,
  Enter on Keybindings, Esc, close; 20 cycles; PSS and anonymous memory open and closed, threads
  before/after equal); run it, `tools_pss_open_and_closed` and `at_rest_no_wakeups_after_a_launcher_cycle`
  3 times back to back against `main` (the latter two only on `main`); record.
- [ ] Docs: `docs/configuration.md` (`[tools] binds_files`), `docs/features/tools-menu.md`
  (built-in list now 46, a "Keybindings" subsection: sources, groups, descriptions, keys, bounds,
  the not-understood line).
- [ ] Commit `feat(tools): a Keybindings level listing the compositor's bindings`.

### Task 3: Draw the Keybindings level in the panel

**Files:** Create `launcher/view/binds.rs`; modify `launcher/view/panel.rs` (`strip` and
`tab_strip` take names and the active index instead of `ToolsState` and `Panel`), `launcher/view.rs`
(dispatch to `binds::render` at `Level::Binds` with the panel on, `hit` for its tabs and rows),
`launcher/view/tests.rs` + snapshots.

**Interfaces — Consumes:** `ToolsState::binds`, `bind_tab`, `binds::rows`, `panel::window`,
`Label::Group`, `tools_chip`, `key_hints`, `ring_shape`, `list::boxed`, `view::tools_layout`.
**Produces:**
```rust
fn strip<'a>(names: &[Cow<'a, str>], active: usize, b: &BoxLayout) -> Strip<'a>;   // view/panel.rs, no behaviour change
fn tab_strip(names: &[Cow<'_, str>], active: usize, b: &BoxLayout, g: &mut Grid, shapes: &mut Vec<Shape>, y: usize);
pub(super) fn render(l: &Launcher, b: &BoxLayout) -> Boxed;                        // view/binds.rs
pub(super) fn hit(l: &Launcher, b: &BoxLayout, column: u16, row: u16) -> Click;    // view/binds.rs: a tab or a row
```
Layout: row 1 the search field (`tools_chip`), row 2 the tab strip (`All`, the groups), separator,
the list (each bind two lines: the combo in a `Chip(Pill)` capsule then the description; the
command dim under it) beside the detail card (`Card`: combo capsule, group dim, description
wrapped, a thin line, `Command` then the command wrapped, `From <file name>` dim), footer hints
`Tab Group · type to search · Esc Back`; under the list `N lines not understood` (`Style::MUTED`)
when `not_understood > 0`; `No keybindings found` when `!found`; `no match` for an empty search.

- [ ] Failing tests: `the_tab_strip_draws_the_same_after_taking_names` (the existing
  `panel_home`/`panel_category` snapshots unchanged — the refactor test);
  `binds_all_snapshot` (106×30, fixture of 3 groups); `binds_group_tab_snapshot`;
  `binds_search_snapshot` (query `desk`); `binds_not_understood_and_empty_lines`;
  `the_binds_detail_card_shows_the_whole_command` (a 200-char command wrapped, never cut in the
  card); `a_click_on_a_bind_tab_opens_it`; `nothing_of_the_binds_level_is_drawn_outside_the_box`.
- [ ] Run `nix develop -c cargo test -p alpheus-daemon view` → the new snapshots are missing / fail.
- [ ] Implement; accept the new snapshots with the repo's snapshot tool only after reading them.
- [ ] Render one PNG with the real painter (throwaway, scratch dir, not committed) and look at it
  once; fix obvious defects.
- [ ] Mutate: draw the command in the card through `truncate` → the whole-command test fails.
- [ ] MEASUREMENT: `binds_pss_open_and_closed` and `tools_pss_open_and_closed` back to back
  against Task 2's commit and `main`, 3 runs; record (the drawing's shm pool is the panel's box,
  expected within noise of Tools open).
- [ ] Docs: `docs/features/tools-menu.md` ("Keybindings": what the user sees, the detail card,
  footer); "How it is built" names `view/binds.rs`.
- [ ] Commit `feat(tools): draw the Keybindings level in the command panel`.

### Task 4: The Sway dialect

**Files:** Create `launcher/tools/binds/sway.rs`, fixtures `binds/fixtures/sway/` (invented
`config` with `set $mod Mod4`, `include ./config.d/*`, a `mode "resize" { … }`); modify
`binds/mod.rs` (dispatch, `roots` for Sway), `binds/describe.rs` (Sway table), `binds/tests.rs`,
`tests/headless_session.rs`.

**Interfaces — Produces:** `sway::parse(text, here, st) -> Vec<PathBuf>` (includes, a `*` in
the last component resolved through `Files::list`, sorted); `roots(Sway, …)` →
`FirstOf([$XDG_CONFIG_HOME/sway/config | $HOME/.config/sway/config, /etc/sway/config])` or
`Each(binds_files)`.

- [ ] Failing tests: `binds_sway_expands_set_variables_and_modes` (`$mod+Shift+q` → `Super+Shift+Q`,
  `Mod1` → `Alt`, `Control` → `Ctrl`; binds inside `mode "resize" {}` in group `resize`; outside,
  `# ── Name ──` groups else `Default`); `sway_flags_are_skipped` (`bindsym --release --locked
  $mod+x exec foo` → combo `Super+X`, command `exec foo`, description `Run foo`);
  `bindcode_keeps_its_code`; `sway_include_glob_and_cycle_are_bounded` (`include config.d/*` reads
  the folder sorted; a self-include read once; 8-file cap); `the_etc_fallback_is_used_only_without_a_user_config`;
  `sway_humanised_table` (`workspace number 3` → `Go to workspace 3`, `move container to
  workspace number 3` → `Move window to workspace 3`, `kill` → `Close window`, `focus left` →
  `Focus left`, `fullscreen toggle`, `floating toggle`, `reload`, `exit`, `mode "resize"` →
  `Enter mode resize`).
- [ ] Headless (`#[ignore]`d, harness Sway, recorder `PATH`): `binds_on_sway_lists_the_fixture`
  (launcher.toml `binds_files` = a fixture written in the test's temp dir; `--tools`, select
  Keybindings, Enter: the daemon's log has `tools: binds shown (N binds, M groups)` with the
  fixture's counts; Esc back; Esc closes; `pgrep` clean).
- [ ] Run the unit tests → fail (no Sway parse); run the headless one at `nice -n 19` → fails.
- [ ] Implement.
- [ ] Mutate: forget to pop the mode on `}` → the modes test fails.
- [ ] MEASUREMENT: `binds_pss_open_and_closed` with a Sway fixture (parametrise by dialect)
  against Task 3's commit, 3 runs; record.
- [ ] Docs: `docs/features/tools-menu.md` (Sway sources and the `/etc` fallback),
  `docs/compositors.md` (the cheatsheet reads files, asks no compositor; hidden on Hyprland/niri).
- [ ] Commit `feat(binds): read Sway's bindings, modes and includes`.

### Task 5: `alpheus-launch --binds`

**Files:** `crates/alpheus-common/src/launcher_ipc.rs` (`Request::Binds`, header `binds`),
`crates/alpheus-launch/src/lib.rs` (`Invocation::Binds`, `--binds`, the old-daemon mapping to exit
3), `crates/alpheus-launch/tests/client.rs`, `crates/alpheus-daemon/src/launcher/socket_glue.rs`
(route), `launcher/tools/app.rs` (`launcher_open_binds`), `notify/app.rs` (the exhaustive match),
`tests/headless_session.rs`, `docs/cli.md`.

**Interfaces — Produces:** `Request::Binds`; `pub(crate) fn launcher_open_binds(&mut self, qh:
&QueueHandle<App>, requested_at: Instant)` (the Tools model with the panel sized, then
`open_binds` with an empty stack); the verb toggles like `--tools`; no dialect →
`Reply::Error("no key bindings source for this compositor")`, exit 2.

- [ ] Failing tests: `binds_request_round_trips` (ipc); `the_client_maps_binds_and_an_old_daemon_to_3`
  (client.rs); `esc_on_a_binds_open_closes_the_box` (model: `up()` None → `Outcome::Close`);
  `the_binds_verb_is_refused_without_a_dialect`; headless `binds_verb_opens_and_toggles` (Sway
  harness, fixture via `binds_files`; second `--binds` closes; `pgrep` clean).
- [ ] Run → fail (unknown request).
- [ ] Implement.
- [ ] Mutate: route `Request::Binds` to `launcher_open_tools` → the Esc test fails (stack not empty).
- [ ] MEASUREMENT: `binds_pss_open_and_closed` driven by the verb, against Task 4's commit, 3 runs;
  record.
- [ ] Docs: `docs/cli.md` (`--binds`, exit codes), `docs/features/tools-menu.md` ("From a key"),
  a Mango/Sway bind example.
- [ ] Commit `feat(launch): --binds opens the Keybindings level`.

### Task 6: `[power]` and user rows in the ⏻ menu

**Files:** `crates/alpheus-daemon/src/config.rs` (+ `config/tests.rs`), `power/mod.rs`,
`power/app.rs`, `segments.rs` (`power` takes `&BarConfig`), `launcher/tools/app.rs`
(`Context.session` from `power::any_rows`), `tests/guardrails.rs`.

**Interfaces — Produces:**
```rust
// config.rs
pub struct PowerConfig { pub countdown: u8 /* 0..=30, 0 off */, pub builtins: bool /* true */, pub entry: Vec<PowerEntryConfig> /* ≤ 8 */ }
pub struct PowerEntryConfig { pub name: String /* 1..=24 chars */, pub icon: Option<String> /* one char */, pub run: Vec<String> /* not empty */, pub confirm: bool, pub countdown: Option<u8> }
pub const POWER_ENTRIES_MAX: usize = 8; pub const POWER_COUNTDOWN_MAX: u8 = 30; pub const POWER_NAME_MAX: usize = 24;
fn check_power(p: &PowerConfig) -> Result<(), String>;   BarConfig { pub power: PowerConfig, .. }
// power/mod.rs
pub enum PowerEntry { Session(Session), User(usize /* index into PowerConfig::entry */) }
impl PowerEntry { pub fn command(self, c: &BarConfig) -> &[String]; pub fn word(self) -> Cow<'static, str> /* "power off", "entry 2" */; }
pub struct PowerRow { pub entry: PowerEntry, pub glyph: char, pub label: String, pub asks: bool, pub countdown: u8 }
pub fn rows(c: &BarConfig) -> Vec<PowerRow>;  pub fn any_rows(c: &BarConfig) -> bool;
Power { pub rows: Vec<PowerRow>, pub selected: Option<usize>, pub question: Option<(PowerEntry, Instant)> }
Power::new(c: &BarConfig) -> Option<Power>;  PowerAction::RunAndClose(PowerEntry);
// power/app.rs
pub(crate) fn spawn_session(exec: &dyn Executor, argv: &[String], entry: PowerEntry) -> Option<String>;
```
A user row without `icon` shows `•`. `session.rs` is unchanged; the quick panel's session row keeps `Session`. `PowerRow.countdown` is
parsed and stored here, used in Task 8.

- [ ] Failing tests: `power_defaults_change_nothing` (no `[power]` → the same four rows as today);
  `power_entries_parse_and_bounds` (9 entries, a 25-char name, `countdown = 31`, an empty `run`, a
  blank program, a NUL, a two-character `icon` → each invalid with its reason);
  `a_power_entry_naming_sleep_makes_bar_toml_invalid` (`["systemctl", "Suspend"]`,
  `["sh","-c","… hibernate"]`, `hybrid-sleep`, `dpms` → invalid); `builtins_false_hides_the_four_rows`;
  `user_rows_follow_the_fixed_ones_in_file_order`; `the_button_shows_with_user_rows_only`
  (`builtins = false` + one entry → segment present, `--power` not refused);
  `session_user_row_runs_only_its_argv_through_the_fake_executor` (`spawn_session` with
  `PowerEntry::User(1)` → the fake records exactly that argv; log `power: entry 2`); a confirm user
  row asks then confirms 0.4–3 s later; guardrail `every_power_menu_activation_is_a_configured_session`
  extended: masks over the four commands × {no entry, a plain entry, a confirm entry}, every
  `RunAndClose(e)` has a non-empty `e.command(&config)`, rows that ask never run without their own
  question and a late second activation, every row is reached, no command holds a sleep word;
  `the_power_menu_lets_go_of_its_timer_and_runs_only_its_entry` adapted: `power_spawn` contains
  `entry.command(&self.bar.config)` and `spawn_session(`, still one `.spawn(` in `spawn_session`,
  still defined once and called once.
- [ ] Run `nix develop -c cargo test -p alpheus-daemon power` and `--test guardrails power` → fail.
- [ ] Implement; the "shown" log line names fixed rows by `[quick]` key and user rows as `entry N`.
- [ ] Mutate: let a user row with `confirm = true` run on the first activation → the guardrail fails;
  drop the sleep check → the invalid-file test fails.
- [ ] MEASUREMENT: `power_menu_pss_idle_open_and_closed` with two user rows and
  `at_rest_no_wakeups_after_the_power_menu`, back to back against `main`, 3 runs; record.
- [ ] Docs: `docs/configuration.md` (`[power]`), `docs/features/power.md` (rows, configuration,
  how it is built, rules), `docs/decisions.md`: amend "The power menu runs the `[quick]` commands
  and has no setting of its own" (reversed in part: it still runs them, and `[power]` adds rows and
  can hide them; why: the owner's custom logout and lock rows), add "A user row of the power menu
  never sleeps".
- [ ] Commit `feat(power): user rows in the power menu from [power]`.

### Task 7: Digits in the ⏻ menu

**Files:** `power/mod.rs` (+ tests), `power/app.rs` (the popup's width from the model),
`tests/guardrails.rs` (the activation guardrail's key list gains `'1'..='9'`), headless test.

**Interfaces — Produces:** `Power::key` handles `Key::Char('1'..='9')`: select row `n-1` and
`activate` it (a repeat does nothing); `Power::width(&self) -> u16` (longest of the rows' text,
a question or a countdown line, plus the digit capsule and pads; replaces `WIDTH`); `render`
draws a `Chip(Pill)` capsule with the digit at each of rows 1–9's right (`Boxed.shapes`, painted
by `Painter::paint_boxed`).

- [ ] Failing tests: `a_digit_runs_a_row_that_does_not_ask` (`1` on Lock → `RunAndClose(Session(Lock))`);
  `a_digit_on_a_row_that_asks_needs_the_same_digit_again` (`3` → question; `3` at +100 ms → nothing;
  at +400 ms → runs; `4` at +500 ms → asks its own); `a_digit_past_the_rows_does_nothing`;
  `a_held_digit_never_acts` (`repeat = true` → `Nothing`); `rows_past_nine_have_no_capsule`
  (12 rows → 9 capsule shapes); `the_width_fits_the_longest_row` (a 24-char user name);
  `power_menu_with_digits_snapshot`; headless `power_a_digit_runs_a_user_row` (Sway harness, recorder
  `PATH`, `bar.toml` with one entry running the recorder; `--power`, `5` → the recorder logged that
  argv once; `pgrep` clean).
- [ ] Run → fail.
- [ ] Implement.
- [ ] Mutate: let a repeated digit activate → the held-digit test fails.
- [ ] MEASUREMENT: `power_menu_pss_idle_open_and_closed` against Task 6's commit, 3 runs; record.
- [ ] Docs: `docs/features/power.md` ("Using it": digits), decisions entry "A digit picks a power
  row, and the same digit confirms".
- [ ] Commit `feat(power): digits pick a row, the same digit confirms`.

### Task 8: The countdown

**Files:** `power/mod.rs` (+ tests), `power/app.rs` (`power_tick`, `power_timer_sync` unchanged in
shape), `tests/guardrails.rs`, `tests/headless_session.rs`, `tests/memory.rs`.

**Interfaces — Produces:**
```rust
pub struct Countdown { pub entry: PowerEntry, pub ends: Instant, pub left: u8 }
Power { pub counting: Option<Countdown>, .. }
Power::deadline(&self) -> Option<Instant>  // the earlier of the question's end and the countdown's next whole second (or its end)
Power::due(&mut self, now: Instant) -> PowerAction  // RunAndClose at the end; Redraw when `left` changes
```
The row's run is a countdown when `row.countdown > 0` (a row that asks: at the confirming
activation; a row that does not ask with its own `countdown`: at its activation). Its line:
`<glyph> <label> in <left> s · Esc`, `Style::WARN`.

- [ ] Failing tests: `the_confirming_activation_starts_the_countdown` (`countdown = 5`, Power off,
  confirm → `Redraw`, `counting.left == 5`, no `RunAndClose`); `the_countdown_steps_each_second_then_runs`
  (`deadline` = +1 s, +2 s, …; `due` at the end → `RunAndClose(Session(Poweroff))`);
  `esc_cancels_the_countdown_and_runs_nothing` (`Escape` → `Redraw`, `counting == None`, menu open;
  a second `Escape` → `Close`); `enter_and_space_during_the_countdown_do_nothing`;
  `any_other_key_or_a_hover_elsewhere_cancels_it`; `a_rows_own_countdown_overrides_the_global_one`;
  `countdown_zero_runs_at_once`; guardrail `power_countdown_closed_popup_disarms_the_timer`
  (`power_closed` syncs the timer to `None` and the model drops with the popup: no field of `App`
  holds a `Countdown`); the activation guardrail: with `countdown > 0`, `RunAndClose` comes only
  from `due` at or after `ends`; headless `power_countdown_escape_runs_nothing` (Sway harness, 3 s
  countdown, confirm Power off with a recorder command, Esc at 1 s, wait 4 s: recorder empty) and
  `power_countdown_runs_at_its_end` (2 s, recorder logs once).
- [ ] Run → fail.
- [ ] Implement.
- [ ] Mutate: keep `counting` on Escape → the Esc test fails; arm the timer every 100 ms → the
  wake-up count of the memory test below rises.
- [ ] MEASUREMENT: extend `at_rest_no_wakeups_after_the_power_menu` with a countdown cancelled by
  Esc (0 wake-ups after close) and add `power_countdown_wakes_once_a_second` (5 s countdown: at most
  6 loop wake-ups over it); `power_menu_pss_idle_open_and_closed` against Task 7's commit and
  `main`, 3 runs; record.
- [ ] Docs: `docs/configuration.md` (`countdown`, per-row `countdown`), `docs/features/power.md`
  (the countdown, its timer, what cancels it), decisions entry "The power menu's countdown lives
  and dies with the menu".
- [ ] Commit `feat(power): an optional countdown before a row runs`.

### Task 9: Close-out

**Files:** `CHANGELOG.md`, `docs/measurements.md` (summary rows), `docs/features/README.md`
(the tools-menu and power lines), the spec's "Left to the builder" answered in the feature pages.

- [ ] Whole-branch check: fmt, clippy `-D warnings`, `cargo test --workspace`, the headless suite
  and the memory suite at `nice -n 19`; any total-PSS failure rerun alone three times with `--exact`.
- [ ] MEASUREMENT: the branch against `main`, 3 runs: `pss_under_budget_and_stable`,
  `at_rest_no_wakeups`, `tools_pss_open_and_closed`, `binds_pss_open_and_closed`,
  `power_menu_pss_idle_open_and_closed`; a summary row per feature in `docs/measurements.md`.
- [ ] A note for the owner (in the final report, never an edit of his files): comments above his
  Mango binds become their descriptions; `[power]` rows can replace a lock or logout script.
- [ ] Commit `docs(changelog): the power menu's rows and the keybinding cheatsheet`.
