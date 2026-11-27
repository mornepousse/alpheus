# Wallpaper picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `Wallpaper` tool in the Super+X panel lists one folder's images by name; Enter restarts
the user's setter with the file, optionally runs `then` (matugen); the daemon re-applies the last
choice at start; `alpheus-launch --wallpaper next` serves an external timer.

**Architecture:** A pure `wallpaper.rs` (config, listing, name guard, argv fill, next). A
`wallpaper/setter.rs` holding at most one watched child (`Executor::spawn_watched`), replaced
new-first, ended only by dropping its handle. The Tools menu gets one built-in with
`Choices::Command` whose lines come from a `Job::Folder` on the existing `alpheus-tools` reader
thread. A glue file `wallpaper/app.rs` puts the setter's pidfd on the loop, applies at start and
answers the verb.

**Tech Stack:** Rust, calloop `Generic` sources, `alpheus_common::exec` (`spawn_watched`,
`Watched`, `FakeExecutor`), `state_file.rs`. No new dependency, thread or timer.

**Spec:** `docs/superpowers/specs/2026-11-27-wallpaper-design.md`.

## Global Constraints

- Every cargo command in `nix develop -c …`; headless and memory suites at `nice -n 19`; memory
  suite release only.
- Tests first, each failing for the stated reason, then mutated to prove it bites.
- No new dependency, thread, timer or polling. Everything off costs nothing.
- Never call a session tool or a real setter (swaybg, swww, wbg, matugen): `FakeExecutor`, or a fake
  script on the harness's private `PATH`. Never touch the user's daemon or session; after a headless
  run, `pgrep -u "$USER" -f fake-setter` finds nothing; kill only a PID you started.
- No `pkill`, `killall`, pattern kill, PID file or `/proc` read anywhere in `wallpaper/`.
- Logs are fixed words: never a file name, the folder, an argv, the query.
- Never loosen a guardrail test. The one extension below needs the owner's go (Task 2).
- English everywhere; one logical change per commit, `type(scope): …`, a body saying why, the
  session's two trailers; stage by name, read `git diff --cached` before committing; branch
  `feat/wallpaper` only.
- `docs/configuration.md` and `docs/features/wallpaper.md` updated in the commit of the task that
  changes what they describe.
- **Every task ends with a RAM measurement of each part it touches, recorded in
  `docs/measurements.md`** (daemon: `tests/memory.rs`, release, back to back against `main`
  07227887 built in its own target directory, 3 runs each; transient parts by the anonymous delta
  after a cycle). A task with no measurement is not done.

## Review Focus

1. **Killing a stranger or the wrong setter.** The old setter is dropped only after the new one
   started, and nothing in `wallpaper/` signals by name or PID → Task 2
   (`a_restart_drops_only_the_old_handle_after_the_new_one_started`,
   `the_wallpaper_ends_only_the_setter_it_started`).
2. **A name escaping the folder.** A remembered or chosen name with `/`, `..`, NUL, or not in the
   folder never reaches `{file}` → Task 1 (`a_name_with_a_slash_or_dotdot_is_refused`) and Task 4
   (`a_remembered_name_outside_the_folder_is_not_applied`).
3. **File names leaking.** Into the history, a log or a `{:?}` → Task 3
   (`a_wallpaper_choice_is_recorded_without_its_name`) and Task 2 (`the_wallpaper_logs_fixed_words_only`).
4. **Off is not free.** A read, a stat or a byte with `dir` empty → Task 3
   (`the_wallpaper_entry_is_absent_without_a_dir`, `the_folder_is_read_only_when_its_level_opens`)
   and Task 5 (`small_features_cost_nothing_at_start` extended).
5. **A one-shot setter judged by its start.** `restart = false` must write the name and run `then`
   only on exit 0 → Task 2 (`a_one_shot_setter_succeeds_only_on_exit_zero`,
   `a_failed_set_runs_no_then_and_keeps_the_remembered_name`).

---

### Task 1: `[wallpaper]` configuration and the pure module

**Files:** Create `crates/alpheus-daemon/src/wallpaper.rs`, `crates/alpheus-daemon/src/wallpaper/tests.rs`; modify `crates/alpheus-daemon/src/lib.rs` (`pub mod wallpaper;`), `crates/alpheus-daemon/src/launcher/config.rs` (`LauncherConfig.wallpaper`, parse and validate `[wallpaper]`), `tests/guardrails.rs` (pure-file list: `wallpaper.rs`), `docs/configuration.md`.

**Interfaces — Produces:**
```rust
pub const MAX_FILES: usize = 2000;
pub const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];
pub const FILE_PLACEHOLDER: &str = "{file}";
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WallpaperConfig { pub dir: String, pub set: Vec<String>, pub restart: bool /* true */, pub then: Vec<String>, pub remember: bool /* true */ }
impl WallpaperConfig { pub fn enabled(&self) -> bool; }           // dir non-empty and set non-empty
pub fn validate(w: &WallpaperConfig) -> Result<(), String>;
pub fn resolve_dir(dir: &str, home: Option<&Path>) -> Option<PathBuf>; // `~/` expanded; None unless absolute
pub fn is_image_name(name: &str) -> bool;                         // extension in EXTENSIONS, any case
pub fn is_plain_name(name: &str) -> bool;                         // non-empty, no '/', no NUL, not "." or ".."
pub struct Listing { pub names: Vec<String>, pub cut: bool }
pub fn list(dir: &Path, cap: usize) -> std::io::Result<Listing>;  // one read_dir; files (or links to files) only; non-UTF-8 names skipped; sorted
pub fn fill(argv: &[String], file: &Path) -> Vec<String>;         // the element exactly "{file}" replaced whole
pub fn next_after<'a>(names: &'a [String], current: Option<&str>) -> Option<&'a str>; // wraps; first when current absent or unknown
```
`LauncherConfig { wallpaper: WallpaperConfig, .. }`; `[wallpaper]` is `deny_unknown_fields`.

- [ ] Failing tests (`wallpaper/tests.rs`): `wallpaper_is_off_by_default` (`LauncherConfig::default().wallpaper.enabled()` false, `restart` and `remember` true); `set_needs_file_once_as_a_whole_element` (`["swaybg","-i","{file}"]` ok; no `{file}`, twice, `"x{file}"`, `{file}` as program → `Err`); `a_dir_without_set_is_invalid`; `then_takes_file_at_most_once_whole`; `an_unknown_wallpaper_key_is_refused` (via `LauncherConfig::load` on a temp file: warning, defaults kept); `resolve_dir_expands_home_and_refuses_relative`; `the_folder_listing_keeps_images_sorted_and_bounded` (temp dir: `b.JPG`, `a.png`, `notes.txt`, a subdirectory `c.png/`, cap 2 → `["a.png","b.JPG"]` then with 3 files and cap 2 → `cut: true`, 2 names); `a_name_with_a_slash_or_dotdot_is_refused` (`"a/b.png"`, `".."`, `"."`, `""`, `"a\0.png"` → false; `"my wall.png"` → true); `fill_keeps_a_name_with_spaces_in_one_argument`; `next_after_wraps_and_starts_at_the_first`.
- [ ] Run `nix develop -c cargo test -p alpheus-daemon wallpaper` → fails to compile (module absent).
- [ ] Implement; `list` stops reading once `cap + 1` images are seen (it only needs to know it cut).
- [ ] Mutate: drop the `".."` check in `is_plain_name` → `a_name_with_a_slash_or_dotdot_is_refused` fails; sort removed → the listing test fails.
- [ ] Measurement: `small_features_cost_nothing_at_start` and `startup_pss_per_source`, release, back to back against `main`, 3 runs (the field adds a default struct to the config read at start; expect within noise); row in `docs/measurements.md`.
- [ ] Docs: `docs/configuration.md` gains `### [wallpaper]` (the spec's table, examples for swaybg, wbg, swww, matugen in `then`).
- [ ] Commit `feat(wallpaper): the [wallpaper] configuration and its pure helpers`.

### Task 2: The setter, replaced new-first and ended by its handle

**Precondition:** the owner's go on extending `the_recording_signal_reaches_only_the_recorder_alpheus_started` (spec, "Owner's go needed"). Without it, stop and report.

**Files:** Create `crates/alpheus-daemon/src/wallpaper/setter.rs`, `crates/alpheus-daemon/src/wallpaper/app.rs`, `crates/alpheus-daemon/src/wallpaper/setter/tests.rs`; modify `crates/alpheus-daemon/src/state_file.rs` (`WALLPAPER_FILE`, `read_wallpaper`, `save_wallpaper`), the `App` struct (one field `wallpaper: Setter`), `tests/guardrails.rs`.

**Interfaces — Produces:**
```rust
// setter.rs
pub enum Started { Running, Pending }            // restart=true: done at start; false: wait for the exit
pub enum Ended { Applied, Failed, Gone }         // Gone: a running setter ended on its own
pub struct Setter { /* child: Option<Box<dyn Watched>>, pending: Option<(Box<dyn Watched>, String)> */ }
impl Setter {
    pub fn start(&mut self, exec: &dyn Executor, w: &WallpaperConfig, dir: &Path, name: &str) -> Result<Started, ()>;
    pub fn exit_fd(&self) -> Option<OwnedFd>;     // of the child just started
    pub fn child_ended(&mut self) -> Option<(Ended, Option<String>)>; // the name applied, for remember/then
}
// state_file.rs
pub const WALLPAPER_FILE: &str = "wallpaper";
pub fn read_wallpaper(state_dir: &Path) -> Option<String>;          // read_private, cap 4 KiB, trimmed, is_plain_name
pub fn save_wallpaper(state_dir: &Path, name: &str) -> io::Result<()>;
// app.rs (on App)
pub(crate) fn wallpaper_apply(&mut self, w: &WallpaperConfig, name: &str, run_then: bool);
fn wallpaper_watch(&mut self, fd: Option<OwnedFd>) -> Option<RegistrationToken>; // Generic, Interest::READ, Level
fn wallpaper_child_ready(&mut self);
```
`start` refuses a name that fails `is_plain_name`/`is_image_name` or is not a file in `dir`; spawns `fill(&w.set, &dir.join(name))` with `spawn_watched(prog, args, None)`; only on `Ok` drops the previous child (or previous pending one). `wallpaper_apply` on success (`Running`, or `Applied` later): `save_wallpaper` when `remember`, then `then` filled and started through `run::launch` when non-empty and `run_then`. Logs: `wallpaper: applied`, `wallpaper: setter could not start`, `wallpaper: setter failed`, `wallpaper: setter ended`, `wallpaper: not saved`.

- [ ] Failing tests (`setter/tests.rs`, `FakeExecutor::watch_ok("swaybg", None)`): `the_set_command_is_run_as_written_with_the_file_as_one_argument` (name `my wall.png` → child line `swaybg -i <dir>/my wall.png -m fill` and the arg count shows one element); `a_restart_drops_only_the_old_handle_after_the_new_one_started` (two starts: first child `killed()`, second not; with `watch_err` on the second start, the first is not killed and `start` is `Err`); `a_one_shot_setter_succeeds_only_on_exit_zero` (`restart=false`: `Pending`; `end(Some(1))` → `Failed`; a new start, `end(Some(0))` → `Applied` with the name); `a_failed_set_runs_no_then_and_keeps_the_remembered_name` (glue-level helper `apply_outcome` or the App test harness: state file unchanged, no spawn of `then`); `then_runs_after_a_successful_set_with_the_file` (`FakeExecutor` spawns list holds `matugen image <path>` in its scope wrap); `a_name_not_in_the_folder_is_not_started`; `the_wallpaper_file_round_trips_and_refuses_a_path` (`save_wallpaper` then `read_wallpaper`; a file holding `../x.png` reads `None`).
- [ ] Guardrail tests (`tests/guardrails.rs`): `the_wallpaper_ends_only_the_setter_it_started` (no `.interrupt()`, `kill(`, `SIGINT`, `SIGTERM`, `SIGKILL`, `pkill`, `killall`, `pgrep`, `pidof`, `/proc/` in `wallpaper/`; `spawn_watched(` once, in `wallpaper/setter.rs`); extend the allowlist of `the_recording_signal_reaches_only_the_recorder_alpheus_started` with `wallpaper/setter.rs` (owner's go); `the_wallpaper_logs_fixed_words_only` (every `log(` in `wallpaper/` takes a string literal, no `format!`).
- [ ] Run `nix develop -c cargo test -p alpheus-daemon setter` and the guardrails → fail (absent).
- [ ] Implement. `spawn_watched` is called on the event-loop thread (its PDEATHSIG belongs to the launching thread); a missing loop or fd: the child still runs, `Gone` is then only seen at the next start (a zombie at most until then; said in a comment).
- [ ] Mutate: drop the old handle before spawning the new one → the restart test fails (first child killed although the second failed to start).
- [ ] Measurement: nothing is resident until a wallpaper is applied; record `small_features_cost_nothing_at_start` (unchanged expected) and, from Task 5's harness when it exists, defer the "setter running" row to Task 5 with a note in the row. Record the anonymous size of `Setter` (`size_of`) in the row.
- [ ] Docs: create `docs/features/wallpaper.md` (what, configuration pointer, how it is built: the setter part, rules: the two guardrails, cost so far).
- [ ] Commit `feat(wallpaper): the setter, replaced new-first and ended only by its handle`.

### Task 3: The Tools menu entry and the folder listing

**Files:** `crates/alpheus-daemon/src/launcher/tools/catalog.rs` (`BuiltinAction::Wallpaper`, the row, `Context.wallpaper`, `builtin_shape`), `catalog/tests.rs` (`the_table_is_the_specs`, 46), `launcher/tools/nav.rs` (`Job::Folder`, `ToolsState::with_folder`, `drill_choices`), `launcher/tools/read.rs` (`run_job` and `spawn_job`'s panic event), `launcher/tools/app.rs` (`context`, `run_builtin`, `run_builtin_choice`, `tools_catalog` passing the folder), `launcher/run.rs` (`perform_tool`), tests next to each, `tests/headless_session.rs`, `docs/features/tools-menu.md`.

**Interfaces — Consumes:** Task 1's `WallpaperConfig`, `resolve_dir`, `list`, `MAX_FILES`; Task 2's `App::wallpaper_apply`.
**Produces:**
```rust
BuiltinAction::Wallpaper
b("wallpaper.pick", "Wallpaper", UTILITIES, "Pick a wallpaper from your folder", '\u{F0E09}', BuiltinAction::Wallpaper) // no default letter
Context { wallpaper: bool, .. }      // enabled() && resolved dir is a directory (one metadata per open)
Job::Folder { entry: usize, dir: PathBuf, token: u64 }
impl ToolsState { pub fn with_folder(self, dir: Option<PathBuf>) -> ToolsState; }
```
`run_job(Job::Folder)` → `JobEvent::Choices { entry, token, lines: listing.names, failed: io error }`, note `tools: folder read failed` (fixed). `perform_tool`: `Some(c)` on `Wallpaper` → `remember("tool:builtin:wallpaper.pick")`, `Done::BuiltinChoice(index, c)`. `run_builtin_choice`: `Wallpaper` → `self.wallpaper_apply(&config.wallpaper, &c, true)` (config: the open launcher's, read at open). `run_builtin`'s `Wallpaper` arm: `None` (never chosen without a name).

- [ ] Failing tests: `the_wallpaper_entry_is_absent_without_a_dir` (catalog: `Context::default()` → no `wallpaper.pick`; `wallpaper: true` → present in Utilities, `Choices::Command`); `every_builtin_has_a_short_description` still passes with 46; `the_folder_is_read_only_when_its_level_opens` (nav: `ToolsState::new(..).with_folder(Some(d))` builds no job until `enter` on the entry; then `Step::Drill(Some(Job::Folder { dir: d, .. }))`); `a_folder_job_lists_names_like_a_choices_cmd` (`run_job` on a temp dir → `JobEvent::Choices` with sorted names; missing dir → `failed: true`, note fixed); `a_wallpaper_name_is_never_in_the_flat_search` (`Catalog::flat()` holds no choice row for it); `a_wallpaper_choice_is_recorded_without_its_name` (`perform_tool` with `choice: Some("beach.jpg")` → history holds `tool:builtin:wallpaper.pick` and no key containing `beach`); `the_wallpaper_builtin_spawns_nothing_through_the_tools_glue` (`builtin_argv` → `Ok(None)`; `every_builtin_argv_stays_on_the_allowlist` passes unchanged).
- [ ] Headless (`tests/headless_session.rs`, fake setter script `fake-setter` that writes its argv to a file in the harness dir then `exec sleep 600`, on the private `PATH`): `tools_wallpaper_picks_and_starts_the_setter` (open `--tools`, type `wall`, Enter, type `b`, Enter → the argv file shows `-i`, `<dir>/b.png`); `tools_wallpaper_replaces_the_setter` (pick twice → one `fake-setter` process left in the harness's process group); after the run, `pgrep` shows none of yours.
- [ ] Run unit tests and `nix develop -c nice -n 19 cargo test -q -p alpheus-daemon --test headless_session tools_wallpaper -- --include-ignored` → fail.
- [ ] Implement.
- [ ] Mutate: record `tool:builtin:wallpaper.pick/{c}` → the history test fails; build the job in `with_folder` → the read-only-on-open test fails.
- [ ] Measurement: new memory test `tools_wallpaper_2000_files` modelled on `tools_choices_2000_lines` (2,000 empty `NNNN.png` files in the harness dir): PSS open, anonymous closed back within the same bound as the choices test; plus `tools_pss_open_and_closed` with `[wallpaper]` set; release, back to back against `main`, 3 runs; row in `docs/measurements.md`.
- [ ] Docs: `docs/features/tools-menu.md` (the table has 46; Wallpaper in the built-in list), `docs/features/wallpaper.md` (using it).
- [ ] Commit `feat(tools): a Wallpaper tool listing the folder by name`.

### Task 4: Re-applied at start, and `--wallpaper next`

**Files:** `crates/alpheus-daemon/src/startup.rs` (`settings` keeps `launcher.wallpaper`), `wallpaper/app.rs` (`wallpaper_at_start`, `wallpaper_verb`), the socket dispatch, `crates/alpheus-common/src/launcher_ipc.rs` (`WallpaperVerb`, `Request::Wallpaper`, encode/decode, tests), `crates/alpheus-launch/src/lib.rs` (parse, usage, help), `docs/cli.md`.

**Interfaces — Produces:**
```rust
pub enum WallpaperVerb { Next }                 // wire word "wallpaper next"
Request::Wallpaper(WallpaperVerb)
impl App {
    pub(crate) fn wallpaper_at_start(&mut self, w: &WallpaperConfig);   // after the loop exists
    pub(crate) fn wallpaper_verb(&mut self, verb: WallpaperVerb) -> Reply;
}
```
At start: `enabled() && remember`, `read_wallpaper(state_dir)` is `Some(name)`, `name` a file in the resolved dir → `wallpaper_apply(w, name, false)` (no `then`). Verb: re-read `launcher.toml` (`LauncherConfig::load`), off → `Reply::Error("wallpaper is off")`; else `list(dir, MAX_FILES)` on the loop, `next_after(names, remembered)`, `wallpaper_apply(w, n, true)`, `Reply::Done`; an empty folder → `Reply::Error("no image in the folder")` (fixed words).

- [ ] Failing tests: `the_wallpaper_verb_round_trips` (ipc encode/decode); an older-daemon decode of `wallpaper next` is an unknown request (as the existing tests for new verbs); `alpheus_launch_parses_wallpaper_next` and refuses `--wallpaper` alone; `the_remembered_wallpaper_is_applied_at_start_without_then` (App test harness or a unit on a helper `start_choice(w, state_dir) -> Option<String>`); `a_remembered_name_outside_the_folder_is_not_applied` (state file holding a name absent from the folder → `None`); `nothing_is_read_at_start_when_off` (helper returns before any fs access: `dir` empty with a state dir that does not exist → no error, `None`); `next_applies_the_name_after_the_remembered_one`.
- [ ] Headless: `wallpaper_reapplied_at_start` (state file preset in the harness's `XDG_STATE_HOME/alpheus/wallpaper`, daemon starts → fake setter argv shows that file); `wallpaper_next_steps_through_the_folder` (`alpheus-launch --wallpaper next` twice against the harness's socket only → argv files show `a.png` then `b.png`); `wallpaper_setter_dies_with_the_daemon` (after the harness stops the daemon, no fake setter remains in its process group).
- [ ] Run → fail; implement; run → pass.
- [ ] Mutate: run `then` at start → the start test fails (a `then` spawn recorded).
- [ ] Measurement: `startup_pss_per_source` with `[wallpaper]` on and a remembered file (fake setter), daemon PSS and anonymous against `main`, 3 runs; the fake setter's own PSS is not the daemon's and is not counted; row in `docs/measurements.md`.
- [ ] Docs: `docs/cli.md` (`--wallpaper next`, an example cron line and a systemd user timer), `docs/features/wallpaper.md` (login and rotation), a note in the feature page for owners who start swaybg from the compositor's autostart: remove it once alpheus manages the wallpaper (two setters otherwise). The owner's own config is not edited.
- [ ] Commit `feat(wallpaper): re-applied at start, and the next verb for an external timer`.

### Task 5: Proof and docs

**Files:** `tests/memory.rs`, `tests/headless_session.rs`, `docs/features/wallpaper.md`, `docs/features/README.md`, `docs/measurements.md`, `docs/decisions.md`, `docs/comparison.md` (the matugen row's neighbour: a wallpaper picker row), `CHANGELOG.md`.

- [ ] Memory: extend `small_features_cost_nothing_at_start` with a daemon whose `launcher.toml` sets `[wallpaper] dir` pointing at an empty harness folder but nothing remembered (bound unchanged, 128 KB anonymous); new `at_rest_no_wakeups_with_a_wallpaper_setter` (fake setter running, 5 s window, 0 loop wake-ups); `wallpaper_setter_replaced_50_times_no_growth` (50 `--wallpaper next`, anonymous growth within the launcher cycle test's bound). Release, back to back against `main` 07227887 in its own target dir, 3 runs each.
- [ ] Headless: the full `tools_wallpaper_*` and `wallpaper_*` tests pass; `tools_panel_at_rest_no_wakeups_after_a_cycle` passes with `[wallpaper]` set.
- [ ] Docs: the feature page complete (what, using it, configuration, how it is built, the rules its tests enforce with test names, cost with the measured figures); `docs/features/README.md` link; a decisions entry "The wallpaper setter is alpheus's child, replaced new-first and ended only by its handle; it dies with the daemon, which re-applies the last choice at start"; CHANGELOG; the `measurements.md` swaybg row says it may now be started by alpheus.
- [ ] `nix develop -c cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` clean; `nix build .#alpheus-demon .#alpheus-launch`.
- [ ] Commits: `test(wallpaper): memory and wake-ups of the setter`, `docs(wallpaper): the wallpaper picker`.

## Self-review

- Spec coverage: listing, guard, fill, next (T1); setter, remember, then, guardrails (T2); entry,
  reader job, history privacy (T3); start re-apply, verb (T4); cost proof and docs (T5). Non-goals
  (per-monitor, daemon timer, thumbnails) appear in no task.
- Types: `WallpaperConfig`, `Listing`, `Setter`, `Job::Folder`, `WallpaperVerb` are named the same
  in every task that consumes them; `wallpaper_apply(w, name, run_then)` is the one apply path used
  by T3 (`true`), T4 at start (`false`) and the verb (`true`).
- The owner's go on the guardrail is the only open point; T2 stops without it.
