# Adding a module: the recipe, walked through the timer

A bar module is a feature that draws something in the bar, may open a popup, and may take a verb
from `alpheus-launch`. This page lists what a new one has to touch, in the order to do it, and
points at the focus timer for each step: the timer is a small, complete module with a chip, a
popup, a verb, a state file and a configuration section, and it follows the
[house style](STYLE.md). Its page for users is [`features/timer.md`](features/timer.md). Read
[`ARCHITECTURE.md`](ARCHITECTURE.md) first for the vocabulary (the [glossary](GLOSSARY.md) explains
each term at its first use here) and [`tour.md`](tour.md) for how a message reaches the screen.

All source paths are under `crates/alpheus-daemon/` unless said otherwise. Names were checked by
`grep` at commit `2b2c730`.

The shape to aim for is always the same: a **pure model** (`mod.rs`: state, changes, what is
shown; it reads no clock, file or process), the [glue](GLOSSARY.md#glue) (`app.rs`: the clock, the
keys, the files, the loop), and a few lines in the shared tables that make the daemon aware of it.
Start from the test, as [`CONTRIBUTING.md`](../CONTRIBUTING.md) requires: each step below says
where its test goes.

## 1. Name it: `ModuleId`, its row, its place

- **The variant.** `src/config.rs`, `enum ModuleId`: add `Timer` with a one-line doc. The
  variants are in the default bar's order, left to right; the timer sits just before `Clock`.
  Add a new variant where the module should appear by default, never reorder the others (a user's
  `modules` list names them, and `ModuleId::ALL` is the default list).
- **The row.** `src/modules.rs`, the `MODULES` table: one `ModuleSpec` per variant, in the
  variant's order (`COUNT` is the table's length; the test `one_row_per_module_in_variant_order`
  in `src/modules/tests.rs` pins each row's `id` to its index). The timer's row sets its hue
  (`Hue::Yellow`), its `segment` (a closure calling `segments::timer`), its `popup`
  (`ModulePopup::Timer`) and its `group` (`GroupKind::Timer`, the block it forms under the block
  styles). The name used in `bar.toml` is the string in `row(ModuleId::Timer, "timer", ...)`
  (`names_are_the_bar_toml_spelling`).
- **`ModuleId::ALL`** is built from `MODULES` (`config.rs`), so there is no second list to edit.
  A test like `the_timer_module_sits_just_before_the_clock` (`src/config/tests.rs`) pins the
  place.
- **What it needs.** `Needs` (`src/modules.rs`) are the sources a module asks for
  (`Needs::BATTERY`, `Needs::TRAY`...): a module that is not in `modules` costs nothing because
  `startup::wanted` reads only these flags. The timer needs none (it runs on the loop with one
  [deadline timer](GLOSSARY.md#deadline-timer)), so its row sets no `needs`. A module with its
  own source thread or connection adds a flag, and `start_sources` or `start_modules` in
  `src/startup.rs` starts it when the flag is set.
- **A narrow bar.** `REMOVAL` in `src/layout.rs` lists the order in which modules disappear when
  space runs short; the timer is there (after `Launcher`, before `Failed`), pinned by a layout test
  (`src/layout/tests.rs`, the one that reads `[Launcher, Timer, Failed, Shield, Privacy, Power]`).
  A module missing from `REMOVAL` is never dropped, like the clock.

## 2. The pure model: `timer/mod.rs`

`src/timer/mod.rs` starts with the `//!` header STYLE asks for (what it is, `Never:` lines,
`See:` line). It holds:

- the state, `Timer`, a small value never edited in place: `Timer::apply(self, Change, &Lengths,
  now) -> Timer`;
- the changes, `Change`, and the ends, `End`;
- what the bar shows, `TimerChip` and `Timer::chip`, and when it next changes,
  `Timer::next_change(now, each_second)`;
- the state file's line (`Timer::saved`, `Timer::parse_saved`) and the notification an end
  becomes (`incoming`).

Every time is passed in (`now` in Unix milliseconds): the model never reads a clock, which is why
`src/timer/tests.rs` can test any moment (`a_late_end_is_applied_once_and_the_next_phase_starts_now`,
`the_chip_reads_minutes`, `the_saved_line_round_trips`). Write these tests first and watch them
fail for the right reason.

A module whose state comes from a source also adds a `Msg` variant (`state.rs`, `enum Msg`) and an
arm in `Bar::apply` returning whether the display changed. The timer's variant is
`Msg::Timer(Option<TimerChip>)` with `Msg::Timer(chip) => set_if_changed(&mut self.timer, chip)`;
the test `a_timer_message_redraws_only_on_change` is in `state.rs`. The state also keeps the
latest value in a `Bar` field (`pub timer: Option<TimerChip>`).

## 3. The segment

The segment is what the module draws in the bar: `segments::timer(chip, module_colors)` in
`src/segments.rs` returns a `Segment` of styled pieces (the glyph, the text), using
`module_segment(ModuleId::Timer, pieces)` so that it has one click zone aimed at
`Target::Module(ModuleId::Timer)`. Colours come from the `Style` helpers and the hue (`icon_style`,
`text_style`): states win over hues (dim when paused, warning when `done`). The tests are in
`src/segments/tests.rs` (a snapshot of the cells) and the layout tests above.

## 4. The glue: `timer/app.rs`

`src/timer/app.rs` ties the model to the daemon. Its header says what it never does (start a
thread, wait for the sound, log a command). The pieces:

- `TimerApp` (a field of `App`, built in `wayland.rs`: `crate::timer::app::TimerApp::new`) holds
  the model, the lengths, the one `DeadlineTimer` and the popup's countdown row.
- `App::timer_do(Change)`: the one place a change is made. It applies an end already due, then
  the change, saves the file (`timer_save`), logs one fixed line, sends the chip (`timer_chip`,
  which does `self.bar.apply(Msg::Timer(chip))` and raises the `dirty` flag) and re-arms the deadline
  (`timer_sync`).
- `timer_tick` (the deadline's callback), `timer_check_clock` (called by the clock's minute tick in
  `startup.rs`, because a monotonic timer stops during a suspend), `timer_load` (read at start from
  `start_modules`), `timer_end` (notification and sound).
- the click path: the popup table row `OPS` (below) and `timer_middle_click`, routed by
  `actions::timer_middle` (`src/actions.rs`).

Tests of the glue that need no Wayland live in `src/timer/app/tests.rs`
(`effects_follow_dnd_serving_and_the_sound`, `change_lines_are_fixed_words`,
`a_due_end_is_applied_before_the_change`): the functions that matter are free functions over plain
values (`step`, `change_step`, `effects`) precisely so that they can be tested alone.

**One deadline, armed only while there is something to wait for.** That is the rule that keeps the
daemon at zero [wake-ups](GLOSSARY.md#wake-up) at rest: see
[`decisions.md`](decisions.md), "One deadline, armed only while a timer runs". Any timer you add is
a `DeadlineTimer` that is armed from one function (`timer_sync`) and released when its reason ends.

## 5. The popup, if it has one

A module popup is three small additions:

1. `src/popup.rs`: a `ModulePopup` variant (`Timer`), a `PopupBody` variant (`Timer(TimerPopup)`)
   and a row of the static `POPUPS` table, in variant order (the test
   `one_popup_row_per_variant_in_variant_order` checks it). The row is the module's `OPS`.
2. `src/timer/popup.rs`: the popup's pure content (`TimerPopup`, `items`, `render`, `key`, `hover`,
   `press`, `scroll`); it answers a `TimerAction` and changes nothing itself (tests in
   `src/timer/popup/tests.rs`). It draws through the shared `list` rows, so the keys and the mouse
   behave as in the other menus.
3. `src/timer/app.rs`: `pub(crate) const OPS: PopupOps` with `toggle`, `key`, `pointer`, `flush` and
   `closed`. `closed` runs whatever closed the popup and lets go of what only it needed (the
   one-second tick): the guardrail `the_timer_popup_closing_drops_its_tick_and_sees_the_end` keeps
   that. `popup_needs_quick` in the module's row says whether the popup needs the `quick` module.

## 6. Configuration keys

- `src/config.rs`: a struct for the section (`TimerConfig`, with `#[serde(default,
  deny_unknown_fields)]` and a `Default`), a field on `RawBar` and on `BarConfig`, and a check
  function called while loading (`check_timer`: every length 1 to 180, `rounds` 1 to 12, a
  non-blank sound program). An invalid value is refused with a message, never silently clamped.
  Tests: `timer_defaults_and_bounds` in `src/config/tests.rs`.
- The header comment of `config.rs` lists every key with its default; add yours there.
- `docs/configuration.md`: a table for the section (`### [timer]`) with key, type, default, bounds
  and meaning, and a link to the feature page. Every default in that table comes from the code,
  never the other way round.
- An unused option costs nothing: with `timer` absent from `modules`, `TimerApp::enabled` is false
  and `timer_do` returns at its first line.

## 7. A verb on the socket, if it has one

`alpheus-launch --timer ...` reaches the daemon through the launcher's socket:

- `crates/alpheus-common/src/launcher_ipc.rs`: `Request::Timer(TimerVerb)`, its wire line
  (`alpheus-launcher/1 timer start <secs>`), and the parser. An older daemon answers an unknown
  request with an error, which the client turns into exit 3.
- `crates/alpheus-launch/src/lib.rs`: the flag (`"--timer" => ...`, `timer_arg`) and the usage text;
  `parse_timer_duration` reads `90s`, `25m`, `1h30m`.
- `src/launcher/socket_glue.rs`: `Route::Timer` sends the request to `App::timer_verb`
  (`src/timer/app.rs`), answered before the launcher's own guard. The reply is an error when the
  module is not in `modules` (a headless test, `timer_verbs_need_the_module`).
- The exhaustive match in the guardrail `adding_a_request_variant_must_update_this_exhaustive_match`
  (`tests/guardrails.rs`) fails until the new variant is listed (the timer is `"timer"`), so a
  verb cannot be added without a decision about how it is answered.
- A built-in entry in the Tools menu replays the verb (`BuiltinAction::Timer` in
  `src/launcher/tools/app.rs`); add one if the verb is worth a menu entry.

## 8. The guardrails to join

[Guardrail](GLOSSARY.md#guardrail) tests read the source files and fail when a rule is broken. In
`tests/guardrails.rs` the timer appears in these:

- `nothing_resident_while_the_launcher_is_closed`: every place that builds a timer, thread, watch
  or keyboard is in a table with the reason it does not run at rest. The timer's line is
  `("timer/app.rs", "Timer::", 1)`. A second timer in your module makes this test fail until you
  add it, with a reason.
- `the_tray_spawns_no_thread`: the list of files that must start no thread, process or command
  includes `"timer/mod.rs"` and `"timer/popup.rs"` (a new pure file is added to it; an entry that
  names no file fails, `unmatched_entries`).
- `the_timer_is_wired_through_one_path` and `the_timer_has_no_panicking_path`: a module's own
  rules (the model is assigned in three functions only; no `unwrap`, `expect` or `panic!` in
  production code). Write the same kind of test for a rule of yours: "this state changes in one
  place" is the most useful one.
- `adding_a_request_variant_must_update_this_exhaustive_match` (the verb, step 7).

Do not write the forbidden tokens in a comment of a file these tests cover: they read comments too
(STYLE, rule 1). Some guardrails read your file's text for words that must not appear (`dpms`,
`hyprctl`...) as well.

## 9. A headless test

`tests/headless_session.rs` runs the real daemon on a headless Sway, a private D-Bus and fake
peers, in the isolated harness; those tests are `#[ignore]`d and run with `--ignored`. The timer's
are named `timer_*`, with a shared helper `timer_daemon`:

- `timer_a_click_opens_the_popup_and_enter_starts_a_countdown`: click the chip, press Enter, read the
  log line `timer: countdown started (10 min)`;
- `timer_verbs_need_the_module`, `timer_popup_verb_opens_and_closes_it`,
  `timer_survives_a_daemon_restart`, `timer_under_dnd_ends_without_bubble_or_sound`.

They read the daemon's log, never a real tool: the sound is `touch` on a private path. Never run
the daemon against your own session.

## 10. The memory test and the rest wake-up test

A module that costs memory or wakes the loop must prove it in `tests/memory.rs`, release builds
only (`require_release`):

- `timer_pss_idle_running_and_popup`: the idle chip's cost against the same bar without it, a running
  timer, the popup open, closed and cycled;
- `at_rest_no_wakeups_with_the_timer_idle_or_paused`: the loop sleeps (0 wake-ups in a 5 s window);
- `timer_running_wakes_about_once_a_minute` and `timer_popup_ticks_each_second_only_while_open`: the
  planned wake-ups, and no more.

Bound leaks on anonymous memory (`Anonymous:`), not on total PSS, which moves by hundreds of KB
between runs; print the PSS. The method, the command and the figures are in
[`measurements.md`](measurements.md). Record the measured cost there when the feature costs
anything.

## 11. The documents

- `docs/features/timer.md`: the feature's page, in the shape of its neighbours (what it does, how to
  use it, how to configure it, how it is built, the rules it keeps, what it costs). Add it to the
  list in [`features/README.md`](features/README.md).
- `docs/decisions.md`: one entry per choice that someone might later reverse (title, a few
  sentences, `Date`, `Source`). The timer's run from "The timer is drawn at rest" to the logging
  rules. Code comments point to these entries by title instead of citing a person or a plan.
- `docs/comparison.md`: the row of the feature table, with its status.
- `docs/configuration.md`: the keys (step 6).
- `docs/cli.md`: the exit codes, if the verb adds one; the verb's own words go in the feature page
  (`features/timer.md`, "From a script or a key bind") and in `alpheus-launch --help`.

## 12. Before you open a pull request

- A test first, seen to fail; then the code; then a mutation that turns it red again (break the
  code, see red, restore): [`CONTRIBUTING.md`](../CONTRIBUTING.md) calls it proving the test
  bites, and the glossary [prove-it-bites](GLOSSARY.md#prove-it-bites).
- Tests never call a real `wpctl`, `nmcli`, `systemctl`...: use the injectable [executor](GLOSSARY.md#executor)
  and its fake (the timer's sound goes through it: `the_sound_spawns_its_argv_and_logs_no_command`).
- `nix develop -c cargo test --workspace`, clippy with `-D warnings`, `cargo fmt --all --check`.
- The release memory suite for anything resident, run against a base commit back to back.
- English everywhere, anonymised fixtures, one logical change per commit.

## The checklist

| Step | The timer's file | Test or guardrail |
|---|---|---|
| `ModuleId` variant | `src/config.rs` | `the_timer_module_sits_just_before_the_clock` |
| row in `MODULES`, `Needs` | `src/modules.rs` | `one_row_per_module_in_variant_order` |
| `REMOVAL` | `src/layout.rs` | the layout test of the removal order |
| model | `src/timer/mod.rs` | `src/timer/tests.rs` |
| `Msg` and state | `src/state.rs` | `a_timer_message_redraws_only_on_change` |
| segment | `src/segments.rs` (`timer`) | `src/segments/tests.rs` |
| glue | `src/timer/app.rs` | `src/timer/app/tests.rs` |
| popup | `src/timer/popup.rs`, `src/popup.rs` | `one_popup_row_per_variant_in_variant_order` |
| config keys | `src/config.rs` (`TimerConfig`) | `timer_defaults_and_bounds` |
| verb | `launcher_ipc.rs`, `alpheus-launch/src/lib.rs`, `socket_glue.rs` | `adding_a_request_variant_must_update_this_exhaustive_match` |
| guardrails | `tests/guardrails.rs` | the four named in step 8 |
| headless | `tests/headless_session.rs` | `timer_a_click_opens_the_popup_and_enter_starts_a_countdown` |
| memory, rest | `tests/memory.rs` | `timer_pss_idle_running_and_popup`, `at_rest_no_wakeups_with_the_timer_idle_or_paused` |
| documents | `docs/features/timer.md`, `decisions.md`, `comparison.md`, `configuration.md` | `lychee` |
