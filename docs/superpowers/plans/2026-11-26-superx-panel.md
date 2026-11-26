# Super+X command panel Implementation Plan (revision of the overview plan)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the card overview with the P1 command panel: tabs, two-line tool rows with
descriptions, a detail card with derived actions, search grouped by category with chips.

**Architecture:** Same cell grid with pixel shapes as the overview branch. A pure
`launcher/tools/panel.rs` builds tabs, sections, rows and derived actions from the catalog and
the history; the model drives tabs and chips; `view.rs` draws two panes. The overview's card
placement is removed; letters, keys, shapes and box sizing are kept.

**Tech Stack:** Rust, the daemon's `Grid`, `Boxed`, shapes and painter. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-26-superx-panel-design.md` (revises
`2026-11-26-superx-overview-design.md`, which stays the authority for what it does not change).

## Global Constraints

The overview plan's Global Constraints, unchanged (nix develop, no new dependency/thread/timer,
closed bounds, session safety and the kill-list `proc_dir` rule, fixed-word logs, Tint roles only,
English, tests first and mutated, never loosen a guardrail), plus:
- No second font size.
- Built-in descriptions: at most 40 characters, no trailing period; user `description` at most 80.
- `RECENT` holds at most 4 tools.

## Review Focus

1. **A tool both pinned and in its category**: same letter in both places, the letter runs it once → Task 11 (`a_tool_shown_twice_keeps_one_letter`).
2. **History naming a tool that no longer shows** (hidden, program gone): never in Recent → Task 11 (`recent_skips_tools_that_do_not_show`).
3. **A tab strip wider than the box** (16 user categories): the active tab always visible, marks at the cut → Task 12 (`the_active_tab_stays_visible`).
4. **←/→ on a selected row without chips, or at the first/last chip**: nothing moves out of the row, Left at home does not go "up" → Task 13 (`arrows_on_chips_stay_on_the_row`).
5. **A query that matches a choice of two entries**: each entry's own matching chip is picked → Task 13 (`a_query_picks_the_matching_chip_of_each_entry`).

---

### Task 10: Descriptions, the `panel` key, and the overview's placement removed

**Files:** `launcher/tools/catalog.rs` (`Builtin.description: &'static str`, `Entry.description: Option<String>`), `launcher/tools/config.rs` (`ToolsConfig.panel` replacing `overview`; `EntryConfig.description`), remove `launcher/tools/overview.rs` placement types and their uses (`place_overview` stays as the open hook but builds nothing yet — or is renamed `open_panel`; implementer's call, said in the report), `docs/configuration.md`.

**Interfaces — Produces:** `Builtin { description: &'static str, .. }`, `Entry { description: Option<String>, .. }` (built-in: its table text; user: its `description`), `ToolsConfig { panel: bool /* default true */, .. }`.

- [ ] Failing tests: `every_builtin_has_a_short_description` (non-empty, ≤ 40 chars, no trailing `.`); `an_entry_description_is_at_most_80_characters` (81 → invalid file); `panel_defaults_on_and_parses_off`; the old `overview` key is now an unknown key (invalid file, `deny_unknown_fields`).
- [ ] Write the 45 descriptions (plain, user's side: "Select an area, copy it", "Turn Wi-Fi on or off", …).
- [ ] Remove the card placement and the overview drawing path; Tools mode at the root shows today's category list until Task 12 draws the panel; every existing test still passes or is deleted with its feature (list deleted tests in the report).
- [ ] Mutate: a 41-char description → the first test fails.
- [ ] Commit `refactor(tools): descriptions and the panel key; the card overview removed`.

### Task 11: The panel's model (pure) and its keys

**Files:** Create `launcher/tools/panel.rs` (+ `panel/tests.rs`); modify `launcher/tools/nav.rs`, `launcher/model.rs`, `tests/guardrails.rs` (pure-file list).

**Interfaces — Produces:**
```rust
pub enum Tab { All, Category(usize) }
pub enum Label { Pinned, Recent, Category(usize) }
pub enum PanelRow { Section(Label), Tool(usize /* entry */) }
pub struct Panel { pub tabs: Vec<Tab>, pub active: usize, pub rows: Vec<PanelRow> }
impl Panel {
    pub fn build(catalog: &Catalog, recent: &[usize], active: usize) -> Panel;
    pub fn tools(&self) -> Vec<usize>;            // selectable rows' entries, in row order
}
pub fn recent_entries(catalog: &Catalog, history_keys_newest_first: &[String], n: usize) -> Vec<usize>;
pub enum Action { Run, Open, Choose, Type(String), Confirm, Toggle, QuickKey(char) }
pub fn actions(entry: &Entry) -> Vec<Action>;
```
`ToolsState::panel() -> Option<&Panel>` (Some at the root with `panel` on, on a `--tools` open only); `ToolsState::next_tab(&mut self, step: i32)`. At home the results list is `panel.tools()`; `selected` indexes it.

- [ ] Failing tests (pure): `all_lists_pinned_recent_then_categories`, `empty_sections_are_not_shown`, `recent_skips_tools_that_do_not_show`, `recent_is_deduplicated_and_skips_pinned`, `recent_holds_at_most_four`, `a_category_tab_lists_its_tools_without_label`, `actions_are_derived_from_the_entry` (toggle → `[Toggle, QuickKey]`, confirm → `[Confirm, …]`, choices → `[Choose, …]`, ask → `[Type(label), …]`), `panel_rs_is_pure` via the guardrail list.
- [ ] Failing tests (model): `tab_and_shift_tab_switch_tabs_and_wrap`, `a_tool_shown_twice_keeps_one_letter` (pinned Notes and Utilities' Notes show `n`; `n` runs Notes once), `letters_run_from_a_category_tab_too`, `page_down_moves_a_page_of_tools`, `panel_off_keeps_today_s_root`, plus every Task 5 key test still passing on the panel (letter, `/`, Space, held key, Mod+Space).
- [ ] Implement; Recent reads the launcher's existing history (no new file, no new write).
- [ ] Mutate: drop the de-duplication against Pinned → `recent_is_deduplicated…` fails.
- [ ] Commit `feat(tools): the command panel's tabs, sections and actions`.

### Task 12: Draw the panel

**Files:** `launcher/view.rs` (`render_tools_panel`, detail card, tab strip, two-line rows), `launcher/surface.rs` (box height), `launcher/view/tests.rs` + snapshots.

**Interfaces — Consumes:** `Panel`, `Tab`, `Label`, `PanelRow`, `actions`, `Entry.description` (Tasks 10–11); `Shape`/`ShapeKind` (painter); `tools_box` (kept).

- [ ] Failing tests: `panel_home_snapshot` (default catalog, 106×30), `a_category_tab_snapshot`, `the_detail_card_of_a_toggle_a_confirm_and_a_choices_tool` (actions rows and capsules), `the_band_covers_both_lines_of_a_row`, `the_active_tab_stays_visible` (16 user categories, last tab active → its pill drawn, `‹` at the left), `a_late_state_fills_its_slot_without_moving_the_row` (re-pinned on two-line rows), `nothing_is_drawn_outside_the_box` (every shape within columns and rows), `the_box_holds_six_tool_rows_when_the_output_allows`.
- [ ] Implement per the spec's Layout; tiles `Chip(Pill)` 2×3 with the glyph in `Tint::Hue(category hue)`; section labels `FgDim` capitals; selection `Band` listed after the cells' card/tiles it covers.
- [ ] Render one PNG with the real painter (throwaway, not committed) to the scratchpad; compare with `p1a-home.png`; fix obvious defects once.
- [ ] Mutate: draw the band one row tall → `the_band_covers_both_lines…` fails.
- [ ] Commit `feat(tools): draw the command panel`.

### Task 13: Search grouped by category, matched characters, choices as chips

**Files:** `launcher/matching.rs` (Tools grouping; matched positions), `launcher/model.rs` (←/→ on chips, Enter with a picked chip), `launcher/tools/nav.rs` (picked chip per entry), `launcher/view.rs` (group headers, accent on matched characters, chips line), tests.

- [ ] Failing tests: `results_are_grouped_by_category_in_best_rank_order`, `matched_characters_are_accented` (query `sh` on `Schedule shutdown` → the `sh` cells styled Accent), `fixed_choices_show_as_chips_not_rows`, `arrows_on_chips_stay_on_the_row`, `enter_runs_the_picked_chip` (Schedule shutdown, → twice, Enter → `ToolAction::Builtin { choice: Some("1 h 30") }` or the table's exact text), `a_query_picks_the_matching_chip_of_each_entry`, `a_choices_cmd_entry_keeps_its_level`, `typed_text_still_never_reaches_history_or_debug` (existing guardrails pass).
- [ ] Implement; the picked chip is reset when the query changes unless the query matches a chip.
- [ ] Mutate: group by entry instead of category → the first test fails.
- [ ] Commit `feat(tools): search grouped by category, with choices as chips`.

### Task 14: Proof and docs

**Files:** `tests/headless_session.rs`, `tests/memory.rs`, `tests/guardrails.rs`, `docs/features/tools-menu.md`, `docs/measurements.md`, `docs/decisions.md`, `docs/configuration.md`, `CHANGELOG.md`, and the two specs' outdated lines (the overview spec's "the kill list draws inside it").

- [ ] Headless (recorder on a private PATH, session-safety rules, kill-list `proc_dir` rule if touched): `tools_panel_tab_changes_the_tab`, the letter and `/` tests already committed still pass on the panel, `tools_panel_at_rest_no_wakeups_after_a_cycle`. `pgrep` after: only the owner's processes.
- [ ] Memory, release, back to back against `main` (c2f53bdd) in its own target dir, 3 runs each: Tools open/closed PSS and shm, anonymous closed bounds unchanged, 0 wake-ups; record in `docs/measurements.md`.
- [ ] Docs: the feature page (what it does, using it: tabs, letters, search, chips, detail card; configuration: `panel`, `keys`, `key`, `description`; how it is built; rules; cost), a decisions entry ("The Tools menu opens on a command panel; a letter runs only a tool whose capsule shows, never after `/` or Space nor from Mod+Space"), CHANGELOG.
- [ ] fmt, clippy -D warnings, workspace tests clean.
- [ ] Commits: `test(tools): …`, `docs(tools): the command panel`.
