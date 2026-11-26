# Super+X command panel: design (revision of the overview)

Date: 2026-11-26. Status: the owner chose this layout ("P1") after seeing the overview ("P2a")
rendered by the daemon; it replaces the overview's drawing. Calls marked *(builder's call)* are
the writer's.

This revises `2026-11-26-superx-overview-design.md`. Everything there stays unless this page
says otherwise: intent, goals except the one replaced below, non-goals, anti-goals, constraints,
the keys, the related decisions (15 MB at rest, the control center in layer-shell).

## What changes and why

The owner preferred P1 to P2a: a command panel in the style of Raycast, where each tool says what
it does, the selected tool's actions and keys are shown, and search results read by category.
The goal "the box shows every visible tool at once" becomes: **nothing is hidden** — the home tab
lists pinned and recent tools, then every category as a section, scrolled.

Reference render (throwaway): `p1a-home.png` and `p1b-search.png` of the 2026-11-26 session.

## Kept from the branch

The painter's shapes (`Card`, `Chip`, `Band`, `Switch`), the letters (defaults, claim, `[tools]
keys`, an entry's `key`), the keys' behaviour (a letter with a visible capsule runs its tool, `/`
or Space starts the search, a held key never acts twice, Mod+Space never has live letters nor the
panel), the box sized once per open (about three cards wide, at least today's width, capped at
90 % × 80 % of the output), the urgent frame while a question is pending, choices kept after
their entry in search, the 15 MB budget.

## Removed

The card placement (`launcher/tools/overview.rs`'s `Overview`, `CardPlace`, 2-D `step`) and the
overview's drawing. `[tools] overview` is renamed `[tools] panel` (default `true`; `false` keeps
today's menu): the branch is unreleased, so no migration.

## The design

### Layout

- **Row 1, search**: as built (rounded `Chip(Pill)`, magnifier, `Search tools`, `Super` `X`
  capsules); while searching it shows the result count at its right *(builder's call)*.
- **Row 2, tabs**: pills `All`, then each category that holds a tool, in category order; the
  active tab filled `AccentContainer`, the others `Pill`. When they do not fit, the strip scrolls
  so the active tab stays visible, with `‹` `›` marks at the cut ends.
- **Separator**, then two panes:
  - **List (left, about 60 %)**: section labels in small dim capitals; tool rows **two cells
    tall**: a tile (`Chip(Pill)`, 2 rows × 3 columns) holding the glyph in the category's hue;
    line 1 the name (`Fg`), line 2 its description (`FgDim`, cut with `…`); at the right a drawn
    `Switch` when the state is known (a reserved slot while it is read), `›` when it opens a
    level, and the letter capsule. The selection is a `Band` over both lines.
  - **Detail card (right, about 40 %)**: a `Card` with the selected tool's tile, its name in
    bold, `<Category> · built-in` or `<Category> · yours` dim, its description wrapped, a thin
    line, then its actions, one per row with key capsules at the right, then a dim hint line.
- **Footer**: key capsules and labels for what works here (`⏎ Run · → Options · Tab Category ·
  a-z Quick key · Esc Close` at home; each level its own).

### Tabs and sections

- **All** (the tab at open): `PINNED` (the built-ins of `[tools] root` and the user entries with
  no category), `RECENT` (the last 4 distinct tools run from this menu that still show, not
  already pinned, from the launcher's existing history), then one section per category in
  category order, its header the hue dot and the name in capitals. A tool may appear in Pinned or
  Recent and again in its category; it keeps the same letter in both places. An empty section is
  not shown.
- **A category tab**: that category's tools only, no section label.
- **Tab** / **Shift+Tab** move between tabs (wrapping); a click on a tab opens it. The list
  scrolls to keep the selection visible; PageUp/PageDown move by a page.

### Descriptions and actions

- Every built-in has a one-line description (English, at most 40 characters, no trailing period),
  in the catalog table. A user entry may set `description = "…"` in `[[tools.entry]]` (at most
  80 characters; none shows the dim category name instead).
- The detail card's actions are derived from what the tool is, never written per tool:
  `Run ⏎` (or `Open ⏎` for one that opens a level or a panel), `Choose →` for one with choices,
  `Type <label> ⏎` for one with `ask`, `Confirm ⏎ ⏎` for one with `confirm`, `Turn on/off ⏎` for a
  toggle, and `Quick key <letter>` when it has one. A pinned tool's hint line says `Pinned`.

### Search

- Typing (after `/` or Space at home, or any character without a capsule) filters every tool of
  every category. Results are grouped by category, groups ordered by their best result's rank,
  results inside a group by rank; each group has its section header.
- The characters that matched are drawn in `Accent` in the name.
- An entry with fixed choices shows them as chips on its second line instead of its
  description; **←/→** pick a chip on the selected row, **Enter** runs the entry with the picked
  chip; a search that matches a choice's text picks that chip. A dynamic `choices_cmd` entry keeps
  today's level (Enter or → opens it).
- The detail card follows the selection in search as at home.

### Box

As built (fixed per open, about 106 columns on a wide output, at least today's width, 90 % ×
80 % cap), with a height for at least 6 two-line rows plus the chrome when the output allows
*(builder's call: the exact rows)*. Levels below (choices of a `choices_cmd` entry, an `ask`
prompt, a category opened from a folded place no longer exist — there is no fold) use the list
pane with one-line rows and keep the detail card.

### Cost

No second font size (the detail title is bold at the normal size): no +464 KB. The two-line rows
and the detail card add cells, not memory beyond the larger surface already budgeted. Descriptions
are `&'static str` in the binary (45 × ≤ 40 bytes).

## Tests (the house rules)

Pure: tab strip and its scroll, All's sections (pinned, recent de-duplicated, empty sections
hidden), letters shared by a tool shown twice, description bounds, derived actions per tool kind,
search grouping and order, matched character positions, chip picking by ←/→ and by a matching
query. Drawing: snapshots of home, a category tab, a search with chips, the detail card for a
toggle, a confirm tool and a tool with choices; the band over two lines; nothing outside the box.
Headless: a letter still runs its tool after the box closed; Tab changes the tab; 0 wake-ups at
rest after a cycle. Memory: open and closed figures recorded against `main`.

## Left to the builder

Exact column split of the panes, paddings, the hint lines' wording, the result count's format,
within the P1 render's look. The visual polish pass the owner announced comes after.
