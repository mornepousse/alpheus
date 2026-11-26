# Super+X overview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Tools menu's root (seven category rows) with an overview of category cards
where every tool is visible, has an optional one-key letter and shows its switch, in the P2a look.

**Architecture:** The box stays a `Boxed` of cell rows. `Boxed` gains a list of pixel shapes
(cards, chips, switches, the selection band) drawn by `Fonts::paint_boxed` under the text with the
bar's corner masks. A pure `launcher/tools/overview.rs` places cards and allocates letters; the
launcher model dispatches letters and 2-D moves at the overview; `view.rs` draws it.

**Tech Stack:** Rust, smithay-client-toolkit layer-shell, cosmic-text, the daemon's own `Grid`,
`Boxed` and painter (`painter/blocks.rs` corner masks). No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-26-superx-overview-design.md`

## Global Constraints

- Every cargo command: `nix develop -c …`; heavy suites at `nice -n 19`.
- No new dependency, thread, timer or polling. Nothing of the overview survives a close.
- Closed again after an open: daemon's anonymous memory ≤ idle + 256 KB; 20 more cycles ≤ closed + 128 KB; 0 wake-ups in 5 s at rest after a cycle.
- Headless tests only through `crates/alpheus-daemon/tests/common`. A test that opens the kill list puts `proc_dir` at the top level of `launcher.toml` (before every table) and asserts the listing holds only fixture processes before sending any key.
- Logs are fixed words: never a tool name, a letter, a query, a command (`the_tools_logs_are_fixed_words`).
- Colours only through `Tint` roles, never a value. English everywhere. One logical change per commit, `type(scope):` subject, body says why, trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Tests first; each new test is mutated once (break the code, watch it fail) before the commit.
- Never loosen a guardrail test.

## Review Focus

1. **A typed path from Mod+Space (`/home`)**: entering Tools by the `/` prefix must never run a tool from a letter → owned by Task 5 (`a_prefix_entry_never_runs_a_letter`).
2. **A held letter key** (auto-repeat after the box closed and reopened, or a key held while the tool opens): must run once → Task 5 (`a_held_letter_runs_nothing`).
3. **An output too small for the overview** (1280×720 at scale 2, or a portrait screen): cards fold to `+N more`, nothing drawn outside the frame, the box never exceeds 90 % × 80 % → Task 3 (`a_small_box_folds_every_card`) and Task 7 (`the_overview_box_fits_a_small_output`).
4. **Toggle states arriving after the first frame** (a user `state` read taking 1 s, or hanging): the row never reflows, the slot stays empty on a hang → Task 6 (`a_late_state_fills_its_slot_without_moving_the_row`).
5. **Letters when tools hide or appear** (`[tools] hide`, recording starts so Record hides and Stop shows): a letter always names the tool whose capsule is on screen at that moment → Task 3 (`stop_takes_record_s_letter_while_recording`) and Task 5 (`a_letter_runs_the_tool_its_capsule_shows`).

---

### Task 1: Record the two decisions of 2026-11-26

**Files:**
- Modify: `docs/decisions.md` (append two entries at the end)
- Modify: `docs/SCOPE.md:38-48` (requirement 1 and 3), `docs/measurements.md:74` (target row)

- [ ] **Step 1:** Append to `docs/decisions.md`, in its entry shape:
  - `### Everything alpheus keeps resident fits in 15 MB at rest` — menus closed; what opens on demand may exceed it while open and gives it back on close; replaces the 120 MB of the scope. `Date: 2026-11-26 · Source: tests/memory.rs (startup budget STARTUP_MAX_KB; steady state tightened by the measurement task of the Super+X overview plan)`.
  - `### The control center becomes its own layer-shell program` — launched on demand, outside the resident daemon for isolation and the budget, reusing the daemon's grid and painter through a shared crate; commands that need a terminal still open one. `Date: 2026-11-26 · Source: none yet (its design comes after the Super+X overview)`.
- [ ] **Step 2:** In `docs/SCOPE.md`, requirement 1 says 15 MB at rest instead of 120 MB with a pointer to the decision; requirement 3 notes the control center moves to layer-shell (pointer). `docs/measurements.md` row 74 reads `≤ 15 MB budget at rest`.
- [ ] **Step 3:** Commit: `docs(decisions): a 15 MB resident budget and the control center in layer-shell`.

---

### Task 2: Configuration keys `overview`, `keys` and an entry's `key`

**Files:**
- Modify: `crates/alpheus-daemon/src/launcher/tools/config.rs` (`ToolsConfig`, `EntryConfig`, validation)
- Test: `crates/alpheus-daemon/src/launcher/tools/config/tests.rs` (or the file's existing tests module)
- Modify: `docs/configuration.md` (the `[tools]` table)

**Interfaces:**
- Produces: `ToolsConfig { overview: bool /* default true */, keys: BTreeMap<String, char> /* built-in id → letter */, .. }`, `EntryConfig { key: Option<char>, .. }`.

- [ ] **Step 1: Failing tests**
  - `overview_defaults_on_and_parses_off`: default `overview == true`; `overview = false` parses.
  - `keys_take_one_lowercase_ascii_letter`: `keys = { "quick.wifi" = "w" }` → `Some('w')`; `"W"`, `"ww"`, `"1"`, `"é"` each make the file invalid (same error path as other invalid structure).
  - `an_entry_key_is_one_lowercase_letter`: `[[tools.entry]] key = "g"` → `Some('g')`; same refusals.
  - `a_key_for_an_unknown_builtin_is_noted_and_ignored`: unknown id → one note through the existing `note` callback (fixed word, the id not in it if `hide`'s unknown-id path does not print it either: mirror `hide`), map entry dropped.
- [ ] **Step 2:** Run `nix develop -c cargo test -p alpheus-daemon --lib tools::config` → FAIL (fields missing).
- [ ] **Step 3:** Add the fields (`serde` default, `deny_unknown_fields` kept); deserialize a letter from a one-char string and reject anything not `'a'..='z'`.
- [ ] **Step 4:** Tests PASS; mutate the range check, see `keys_take_one_lowercase_ascii_letter` fail, restore.
- [ ] **Step 5:** Document the three keys in `docs/configuration.md` (defaults: `overview = true`, `keys = {}`, entry `key` unset).
- [ ] **Step 6:** Commit `feat(tools): the overview, keys and an entry's key in launcher.toml`.

---

### Task 3: Default letters, category hues and the overview's placement (pure)

**Files:**
- Modify: `crates/alpheus-daemon/src/launcher/tools/catalog.rs` (`Builtin.key`, `Entry.letter`, `Category.hue`, `Catalog::build`)
- Create: `crates/alpheus-daemon/src/launcher/tools/overview.rs`, tests in `overview/tests.rs`
- Modify: `crates/alpheus-daemon/src/launcher/tools/mod.rs` (declare `overview`)

**Interfaces:**
- Produces in `catalog.rs`: `Builtin { key: Option<char>, .. }`; `Entry { letter: Option<char>, .. }` (the claimed letter, `None` when lost or unset); `Category { hue: Option<Hue>, .. }` (`None` = the Pinned card's accent).
- Produces in `overview.rs`:
  ```rust
  pub const MIN_CARD_COLUMNS: u16 = 34;
  pub const MAX_CARDS_PER_BAND: usize = 3;
  pub struct CardPlace { pub card: CardId, pub col: u16, pub width: u16, pub row: u16, pub rows: u16, pub shown: Vec<usize> /* entry indices */, pub more: usize /* hidden count, 0 = no "+N more" row */ }
  pub enum CardId { Pinned, Category(usize) }
  pub struct Overview { pub cards: Vec<CardPlace>, pub columns: u16, pub rows: u16 }
  impl Overview {
      pub fn place(catalog: &Catalog, max_columns: u16, max_rows: u16) -> Overview;
      pub fn order(&self) -> Vec<Slot>;               // selectable slots, card by card, top to bottom
      pub fn step(&self, from: usize, dir: Dir) -> usize; // 2-D move over `order()` indices
  }
  pub enum Slot { Entry(usize), More(usize /* category */) }
  pub enum Dir { Up, Down, Left, Right }
  ```
- `Catalog::build` claims letters: the Pinned card's entries first (display order), then cards by category order, each in display order; a letter already taken leaves the later claimant at `None`; a user entry's `key`, then `[tools] keys`, then `Builtin.key` is the claimant's wanted letter.

- [ ] **Step 1: Failing tests (catalog)**
  - `default_letters_are_the_table_s`: with every built-in shown except Stop: `capture.area`→a, `capture.full`→f, `capture.annotate`→e, `capture.record`→r, `capture.pick`→p, `capture.ocr`→o, `quick.wifi`→w, `quick.bluetooth`→b, `quick.vpn`→v, `quick.shield`→h, `dnd`→d, `keep_awake`→z, `night_light`→i, `shutdown`→s, `process.kill`→k, `lock`→l, `power`→q, `notes`→n, `timer`→t, `clipboard`→c, `emoji`→m, `diag.usb_events`→u, `diag.usb_reset`→x, `diag.preset.errors`→j; every other built-in `None`.
  - `stop_takes_record_s_letter_while_recording`: with recording on (Record rows hidden, Stop shown) `capture.stop`→r.
  - `a_taken_letter_leaves_the_later_claimant_without_one`: a user entry `key = "w"` in a category ordered after Quick settings → `None`; the same entry at the root (Pinned) → `w`, Wi-Fi → `None`.
  - `hidden_tools_free_their_letters`: `hide = ["quick.wifi"]` and a user entry `key = "w"` → the entry gets `w`.
  - `builtin_categories_have_their_hues`: Quick settings Blue, Control center Purple, Utilities Cyan, Captures Orange, System Yellow, Session Red, Diagnostics Green; user categories cycle `[Blue, Cyan, Green, Yellow, Orange, Red, Purple]` by their index among user categories.
- [ ] **Step 2: Failing tests (overview)**
  - `cards_follow_category_order_three_per_band`: default catalog, 110 columns, 60 rows → bands of 3 cards, Pinned absent, widths equal and ≥ 34, `columns ≤ 110`.
  - `pinned_card_comes_first`: `root = ["notes"]` → `cards[0].card == CardId::Pinned` with `shown == [notes]`.
  - `a_band_shares_its_tallest_card_s_height` and `bands_are_separated_by_one_blank_row`.
  - `a_small_box_folds_every_card`: 70 columns, 14 rows → every card fits the rows, `more > 0` where cut, the last shown row of a folded card is `Slot::More` in `order()`, `rows ≤ 14`.
  - `steps_move_within_and_across_cards`: Down at a card's last slot stays; Right from the 3rd row of card 0 lands on the 3rd row of card 1, clamped to its last row; Left at the first card stays; Down past a band's bottom goes to the first slot of the card below.
  - `place_is_total`: random catalogs (fixed seed loop, 0–40 entries, 0–16 categories, sizes 1..200 × 1..80) never panic and never place a card outside `columns × rows`.
- [ ] **Step 3:** Run `nix develop -c cargo test -p alpheus-daemon --lib tools::` → FAIL.
- [ ] **Step 4:** Implement. A card's rows = 1 header + shown entries (+1 for `+N more`). Columns per band = `min(MAX_CARDS_PER_BAND, max_columns / MIN_CARD_COLUMNS)`, at least 1. When the full height exceeds `max_rows`, cut cards from the tallest down, one entry at a time, until it fits (a card keeps at least its header and one row). `overview.rs` is pure: no logs, no panicking path (it joins the `the_tools_pure_parts_have_no_panicking_path_and_log_nothing` file list).
- [ ] **Step 5:** PASS; mutate the letter-claim order, see `a_taken_letter…` fail; mutate the fold loop, see `a_small_box_folds_every_card` fail.
- [ ] **Step 6:** Commit `feat(tools): default letters, category hues and the overview's placement`.

---

### Task 4: Pixel shapes in `Boxed`, painted under the text

**Files:**
- Modify: `crates/alpheus-daemon/src/list.rs` (`Boxed.shapes`, `Shape`, `ShapeKind`)
- Modify: `crates/alpheus-daemon/src/painter.rs` (`paint_boxed`), reusing `painter/blocks.rs` `round_block` and the `MaskCache`
- Test: `crates/alpheus-daemon/src/painter/tests.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct Shape { pub row: usize, pub col: u16, pub width: u16, pub rows: u16, pub kind: ShapeKind } // in Boxed coordinates (after `boxed` drops the ring), like `Graph`
  pub enum ShapeKind { Card, Chip(Tint), Band, Switch(bool) }
  // Boxed gains: pub shapes: Vec<Shape>   (empty from `list::boxed`, set after it, as `graphs`)
  ```
- Painting: shapes after the surface fill, before cells. `Card` fill `roles.raised`, radius 6 × scale; `Chip(t)` fill `color(roles, base, t)`, radius 4 × scale, inset 2 × scale vertically; `Band` = the selection's fill (`Tint::Selection`), radius 5 × scale; `Switch(on)`: a track 2 cells wide, fill `Hue::Green` on / `Tint::Pill` off, knob `Tint::Fg` on the right when on, left when off. A cell whose background is `Tint::Bg` inside a shape keeps the shape's fill (paint its glyph on the shape's colour as `base`).

- [ ] **Step 1: Failing tests**
  - `a_card_fills_its_cells_with_raised_and_rounds_its_corners`: a 10×4 card in a 20×6 `Boxed`; centre pixel == `roles.raised`; the card's outermost corner pixel == `popup_bg`; a pixel one cell outside == `popup_bg`.
  - `text_on_a_card_keeps_the_card_behind_it`: a space with `Style::NORMAL` written over the card's centre leaves `roles.raised`.
  - `a_switch_draws_its_knob_on_the_side_of_its_state`: on → the right half holds the knob colour, the track green; off → left knob, `pill_bg` track.
  - `no_shape_paints_outside_the_frame`: a shape past the last row or column is clipped (buffer untouched outside the frame's interior).
  - `the_masks_are_one_per_radius_and_scale`: painting two cards and three chips at scale 2 leaves 2 masks more in `Fonts::corner_masks()`.
- [ ] **Step 2:** `nix develop -c cargo test -p alpheus-daemon --lib painter` → FAIL.
- [ ] **Step 3:** Implement in `paint_boxed`; keep `paint_boxed`'s signature.
- [ ] **Step 4:** PASS; mutate the "Bg inside a shape" rule, see `text_on_a_card…` fail.
- [ ] **Step 5:** Commit `feat(painter): rounded shapes under a box's cells`.

---

### Task 5: The overview in the launcher model: letters, search start, 2-D moves

**Files:**
- Modify: `crates/alpheus-daemon/src/launcher/tools/nav.rs` (`ToolsState`: holds the `Overview` at the root when enabled; `rows()` at the root in overview order)
- Modify: `crates/alpheus-daemon/src/launcher/model.rs` (`apply_tools`, `key_repeat`)
- Test: `crates/alpheus-daemon/src/launcher/model/tests.rs` (or the existing launcher tests file for Tools keys)

**Interfaces:**
- Consumes: `Overview::{place, order, step}`, `Slot`, `Dir` (Task 3); `Entry.letter` (Task 3); `ToolsConfig.overview` (Task 2).
- Produces: `ToolsState::overview(&self) -> Option<&Overview>` (Some only at `Level::Root` with `overview` on); `ToolsState::letter(&self, c: char) -> Option<RowId>`; `Launcher::searching: bool` (the user asked for search with `/` or Space at the overview); `Launcher::letters_live(&self) -> bool` = Tools mode ∧ overview shown ∧ query empty ∧ ¬searching ∧ ¬by_prefix.
- The overview's results list is `order()`: `Slot::Entry(e)` → `RowId::Entry(e)`, `Slot::More(c)` → `RowId::Category(c)` (Enter on it drills the category, as today).

- [ ] **Step 1: Failing tests** (model level, fixture catalog, no executor)
  - `a_letter_runs_the_tool_its_capsule_shows`: `Key::Char('w')` at the overview → `Outcome::Activate(Activation::Tool(Builtin{ index of quick.wifi, .. }))`.
  - `a_letter_without_a_capsule_starts_the_search_with_it`: `'y'` → query `"y"`, no activation.
  - `slash_or_space_starts_the_search_and_is_not_typed`: `'/'` then `'w'` → query `"w"`; `' '` then `'w'` → query `"w"`; Backspace on the empty query after `/` → letters live again.
  - `a_prefix_entry_never_runs_a_letter`: enter Tools through the `/` prefix of Mod+Space, then `'w'` → query `"w"`, no activation.
  - `a_held_letter_runs_nothing`: `key_repeat(Key::Char('w'), true)` → `Outcome::Nothing`.
  - `a_confirm_tool_s_letter_asks_first`: `'x'` (Reset USB) → `Outcome::Asked`; a second `'x'` within 0.4 s → `Nothing`; `Enter` 0.5 s later → runs.
  - `arrows_move_in_two_dimensions`: Right from Wi-Fi's row lands on the same row index of the Control center card.
  - `enter_on_more_opens_its_category`: on a folded card's `+N more` → level `Category(c)`.
  - `overview_off_keeps_today_s_root`: `overview = false` → root rows are categories, letters inert.
- [ ] **Step 2:** `nix develop -c cargo test -p alpheus-daemon --lib launcher` → FAIL.
- [ ] **Step 3:** Implement. Letter dispatch goes through `tools_activate(row)` (the same path as Enter, so `confirm`, choices and `ask` behave as today). Left/Right/Up/Down at the overview use `Overview::step` and set `selected`; elsewhere unchanged. Extend the held-key guard in `key_repeat` to any `Key::Char` while `letters_live()`.
- [ ] **Step 4:** PASS; mutate `letters_live` (drop `¬by_prefix`), see `a_prefix_entry…` fail.
- [ ] **Step 5:** Commit `feat(tools): run a tool by its letter from the overview`.

---

### Task 6: Draw the overview and restyle the Tools levels

**Files:**
- Modify: `crates/alpheus-daemon/src/launcher/view.rs` (`render` dispatches to a new `render_tools_overview`; Tools rows, search chip, footer)
- Modify: `crates/alpheus-daemon/src/launcher/surface.rs:667` (frame ink: `FrameInk::Urgent` while a Tools question is pending)
- Test: `crates/alpheus-daemon/src/launcher/view/tests.rs` and its `snapshots`

**Interfaces:**
- Consumes: `ToolsState::overview`, `Launcher::letters_live`, `Shape`, `ShapeKind` (Tasks 3–5).
- Produces: `fn render_tools_overview(l: &Launcher, b: &BoxLayout) -> Boxed`; `pub fn frame_ink(l: &Launcher) -> FrameInk`.

What the box shows (from the spec and the P2a render):
- Row 1: the search chip across the row (`Chip(Tint::Pill)`), a magnifier glyph (Nerd Font `nf-md-magnify`, U+F0349) then the query or the dim placeholder `Search tools`; right-aligned key capsules `Super` `X` (`Chip(Tint::Pill)`), dropped when the box is under 60 columns.
- Cards (`ShapeKind::Card`): header = a `●` in the card's hue (`Tint::Hue(h)`, Pinned `Tint::Accent`), the name in bold `Fg`, the count dim at the right; tool rows = glyph in the hue, name `Fg`, then right to left: letter capsule (`Chip(Tint::Pill)`, letter `FgDim`), `›` for a tool that opens a level, a `Switch` shape (2 cells) when its state is known, a reserved empty 2-cell slot when the entry `has_state` but the state is not in yet.
- Selected slot: a `Band` shape over the row's span inside its card.
- Footer, left-aligned: capsules and labels `a-z Run · ↑↓←→ Move · ⏎ Open · Esc Close` (letters part omitted when `!letters_live`).
- Other Tools levels and the search: today's rows, with `Band` for the selection instead of the flat `SELECTED` fill, the search chip on row 1 carrying the breadcrumb, footer as capsules, no preview pane in Tools mode, icons kept in search rows, a fixed choice row placed right after its entry.

- [ ] **Step 1: Failing tests**
  - `overview_snapshot_default_catalog` (text snapshot of rows + the shapes list, 110×40): header names, counts, letters, `›`, Pinned absent.
  - `a_late_state_fills_its_slot_without_moving_the_row`: render before and after `adopt_state(e, Some(true))` → identical text columns for the name and the letter; the shapes gain one `Switch(true)` at the slot.
  - `a_hung_state_leaves_the_slot_empty`: `adopt_state(e, None)` → no `Switch` shape, the slot's cells blank.
  - `search_rows_keep_icons_and_group_choices`: query `"sh"` → `Schedule shutdown` row, then its five choice rows, then the next entry; every entry row has its glyph.
  - `a_pending_question_turns_the_frame_urgent`: `frame_ink` is `Urgent` while `asking`, `Border` otherwise.
  - `the_footer_hides_letters_when_they_are_not_live`.
- [ ] **Step 2:** `nix develop -c cargo test -p alpheus-daemon --lib launcher::view` → FAIL.
- [ ] **Step 3:** Implement; reuse `list::row` where the row is unchanged; `wipe` any text derived from the query, as `render` does today.
- [ ] **Step 4:** PASS; review the new snapshots by eye; mutate the reserved slot (only reserve once the state is known), see `a_late_state…` fail.
- [ ] **Step 5:** Commit `feat(tools): draw the overview and restyle the Tools levels`.

---

### Task 7: The overview's box size, fixed for the open

**Files:**
- Modify: `crates/alpheus-daemon/src/launcher/surface.rs:133` (`open_layout`)
- Modify: `crates/alpheus-daemon/src/launcher/view.rs` (`layout` gains the Tools case or a `tools_layout` beside it)
- Test: `crates/alpheus-daemon/src/launcher/view/tests.rs`, `crates/alpheus-daemon/src/launcher/surface` tests

**Interfaces:**
- Produces: `pub fn tools_layout(output_columns: u16, output_rows: u16, wanted: (u16, u16)) -> BoxLayout` with `preview_columns = 0`; `wanted` comes from `Overview::place` at open with `max_columns = output_columns * 9 / 10`, `max_rows = output_rows * 8 / 10`.

- [ ] **Step 1: Failing tests**
  - `the_overview_box_fits_a_small_output`: output 80×30 cells → `columns ≤ 72`, `rows ≤ 24`, every shape inside.
  - `the_box_keeps_its_size_from_overview_to_search`: open, type `"w"`, drill into a category → `BoxLayout` unchanged (the layout is computed once per open, as `open_layout` already is).
  - `rows_still_caps_the_other_modes`: Mod+Space `All` mode keeps `config.rows`.
  - `overview_off_keeps_today_s_size`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** PASS, mutate the 80 % cap, see the small-output test fail.
- [ ] **Step 5:** Commit `feat(tools): size the box to the overview once per open`.

---

### Task 8: Headless and memory proof, docs

**Files:**
- Modify: `crates/alpheus-daemon/tests/headless_session.rs` (new tests beside `tools_opens_and_a_builtin_runs_after_the_box_closed`, line 331)
- Modify: `crates/alpheus-daemon/tests/memory.rs` (`tools_pss_open_and_closed` line 4307, `at_rest_no_wakeups_after_tools` line 4451: open the overview, cycle letters and moves)
- Modify: `crates/alpheus-daemon/tests/guardrails.rs` (add `launcher/tools/overview.rs` to the pure-files list; the shapes painter to the no-panic list if the painter has one)
- Modify: `docs/features/tools-menu.md`, `docs/measurements.md`, `docs/decisions.md` (an entry: "The Tools menu opens on an overview; a letter runs its tool only at the overview, never after `/` or Space or from the `/` prefix"), `CHANGELOG.md`

- [ ] **Step 1: Failing headless tests** (recorder script on a private `PATH`, as the existing Tools tests)
  - `tools_a_letter_runs_its_builtin_after_the_box_closed`: open `--tools`, press `n` → the recorder sees Notes' verb once, after the surface is gone.
  - `tools_overview_slash_then_letters_search`: `/`, `n`, `o` → nothing recorded, the box still open.
  - `tools_overview_at_rest_no_wakeups_after_a_cycle`: open, move, close; 0 loop wake-ups in 5 s.
- [ ] **Step 2:** Run `nice -n 19 nix develop -c cargo test -q -p alpheus-daemon --test headless_session -- --include-ignored tools_` → new ones FAIL before Task 5–7 are merged into the binary path (if they already pass, mutate the letter path and see them fail). After the run: `pgrep -u "$USER" -a 'sway|alpheus'` shows only the owner's own session processes.
- [ ] **Step 3:** Memory: `nice -n 19 nix develop -c cargo test -q --release -p alpheus-daemon --test memory -- --include-ignored --test-threads=1 tools_` → closed bounds hold; record the printed open PSS and shm figures, against a base build of `main` in its own target directory, 3 runs each, in `docs/measurements.md`.
- [ ] **Step 4:** Update the feature page (What it does, Using it: the overview and letters, Configuration: the three keys, How it is built: `overview.rs` and shapes, Rules it keeps: the new tests, What it costs: the measured figures), the decision entry, and `CHANGELOG.md`.
- [ ] **Step 5:** `nix develop -c cargo fmt --all --check`, `nix develop -c cargo clippy --workspace --all-targets -- -D warnings`, `nix develop -c cargo test --workspace` all clean.
- [ ] **Step 6:** Commit tests and docs as two commits: `test(tools): the overview's letters and rest, headless and in memory`, `docs(tools): the overview`.

---

### Task 9: Re-measure the resident daemon and tighten the steady-state budget

**Files:**
- Modify: `crates/alpheus-daemon/tests/memory.rs:488` (the 30 MB steady-state assertion)
- Modify: `docs/measurements.md` (the "alpheus today" row, re-measured)

- [ ] **Step 1:** Run the steady-state test that asserts `end < 30 * 1024` alone, 3 times, release, `--exact`, at `nice -n 19`; note the three figures.
- [ ] **Step 2:** If all three are under 13 MB, change the bound to `15 * 1024` with the message `"PSS {end} KB >= 15 MB"` and a comment pointing to the decision of Task 1; if not, leave the bound, write the figures in `docs/measurements.md` and stop: report to the owner (never loosen, never fake).
- [ ] **Step 3:** Mutate: set the bound to the lowest measured figure minus 1 MB, see it fail; restore.
- [ ] **Step 4:** Commit `test(memory): hold the steady state to the 15 MB resident budget`.
