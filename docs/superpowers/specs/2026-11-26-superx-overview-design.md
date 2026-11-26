# Super+X overview: design

Date: 2026-11-26. Status: approved in conversation by the owner ("tout me va"), calls marked
*(builder's call)* are the writer's and open to correction.

## Intent

The Tools menu (`alpheus-launch --tools`, bound to Super+X) works, but its look does not satisfy
the owner. A baseline study (17 headless captures, fixture data) found: the root shows seven
categories and no tool, so a tool is 3 to 9 keys away and only findable if you know its
category; the right preview pane is 40 % of the box and nearly empty; choices crowd the search;
icons vanish in search; orientation is lost in deep levels; a long category is cut with no cue;
the confirmation looks like a selection.

Mockups rendered by alpheus's own painter were shown; the owner chose **P2a, the overview**: every
category in its own rounded card, every tool visible with a key capsule, toggles drawn as
switches, a rounded search field on top, a footer of key capsules. Reference renders (throwaway,
not in the repo): the P2a, P2b and P2c PNGs of the 2026-11-26 session.

## Goals

- At rest (empty query), the box shows every visible tool at once, grouped in category cards.
- A tool with a key runs from the overview in one key press.
- Toggle built-ins show their state as a drawn switch.
- The look is P2a's: rounded cards on a raised surface, a hue dot per category, key capsules,
  switches, rounded search field, footer of key capsules.
- Every behaviour of today's menu is kept: search across all entries, choices, `ask` prompts,
  `confirm`, `state`, `choices_cmd`, the kill list, the held-Enter and double-Enter rules, the
  `/` prefix from Mod+Space, `[tools] hide/root/builtins`, user categories and entries.

## Non-goals

- The P2b option sheets (Area and Area (save) merged into one tool with toggles) and P2c session
  sheet: a later change, once the overview is lived with.
- The control center (Super+A): it becomes its own layer-shell program in a later project (see
  "Related decisions").
- A second font size, a proportional font, animations.

## Anti-goals (a failure even if it works)

- Any cost while the menu is closed: no thread, timer, cache or buffer kept after close beyond
  today's measured figures.
- A layout that jumps under the cursor while typing, or a key that runs something the user did
  not see on screen.
- Leaving the cell grid: hit-testing, snapshots and `view.rs` stay cell-based.

## Constraints

- Lightness: open, the box may cost what its larger surface costs (estimate +1.2 to +1.5 MB of
  shm pool while open, back to today's 56 KB once closed); the daemon's own anonymous memory once
  closed stays within today's bounds (closed ≤ idle + 256 KB, 20 more cycles ≤ closed + 128 KB);
  0 wake-ups at rest after a cycle. Corner masks: one per radius and scale, as the bar's.
- Session safety: headless tests only through the isolated harness; any test that opens the
  kill list sets `proc_dir` at the top level of `launcher.toml` and checks the listing holds only
  fixture processes before sending a key.
- No new dependency. English everywhere in the repository.

## Approaches considered

1. **Pixel overlays on the cell grid (picked).** The box stays a `Boxed` of cell rows. New
   overlay lists in `Boxed`, like the existing `graphs`, describe rounded shapes over cell
   rectangles: cards, chips (key capsules, the search field, the active chip), switches. The
   painter draws them under the text with the bar's corner masks (`painter/blocks.rs`). Cheap,
   testable as text plus overlay lists, reuses the bar's proven primitives.
2. **Free pixel layout** (how the mockups were drawn). Most freedom, but rewrites `view.rs`,
   hit-testing and every snapshot test; days of work and a second layout model in the daemon.
3. **Pure cell grid** (round 1 mockups). No new painting, but the owner rejected the look.

## The design

### What the box shows

- **Search row**: a rounded chip spanning the row (raised surface), a magnifier glyph, the
  cursor and query, placeholder `Search tools` when empty; at its right a key capsule hint
  (`Super X`) *(builder's call: drop it if it crowds narrow boxes)*.
- **Overview (empty query, root level)**: category cards laid in columns, left to right then top
  to bottom, in category `order`. Each card: a header row (hue dot, bold name, dim count at
  right), then one row per tool: icon glyph tinted with the category hue, name, then at the right
  a switch for a toggle with a known state, `›` for a tool that opens a level, and the key capsule.
  Cards of one band share their height (the tallest); bands are separated by a blank row.
- **Box size** *(builder's call)*: the overview sizes the box once, at open, to fit the catalog:
  width for up to 3 cards (about 105 cells), height for the tallest band set, capped at 90 % of
  the output's width and 80 % of its height (`rows` in `launcher.toml` no longer caps the Tools
  mode; it still caps the other modes). The box keeps that size for the whole open: search, levels
  and the kill list draw inside it, so nothing resizes on the first key. With `overview = false`
  the box keeps today's size.
- **Root items** *(builder's call)*: built-ins listed in `[tools] root` and user entries with no
  `category` form a first card titled `Pinned` (accent dot), before every category card; it
  claims quick-key letters first. It is absent when empty.
- **Switches for user entries** *(builder's call)*: an entry with a `state` command gets a
  switch slot reserved from the first frame, so the row never reflows; the slot stays empty until
  the read answers (on or off), and stays empty on a timeout or a hang, never a guess.
- **Card columns**: as many as fit at a minimum card width of 34 cells *(builder's call)*,
  at most 3. A card longer than the space left shows its first tools and a last row `+N more ›`
  that opens the category as today's level.
- **The selection** is a rounded band over the selected tool's cell range inside its card.
  Arrow keys move within a card and across cards (left/right to the neighbour card's row of the
  same index, clamped).
- **Footer**: key capsules and labels: `a-z Run · ↑↓←→ Move · ⏎ Open · Esc Close` at the overview;
  each level keeps its own help text, drawn as capsules.
- **Levels below the root** (a category opened by `+N more`, choices, `ask` prompt, the kill
  list): today's list, restyled: rounded selection band, icons kept in search, the breadcrumb in
  the search chip, the right preview pane dropped *(builder's call: its content, the entry's
  name and "enter run", repeats the row)*.
- **Search (non-empty query)**: today's flat ranked results across every entry, restyled with
  icons and the dim category at right; choices of one entry stay grouped after their entry
  instead of interleaving *(builder's call)*.
- **Confirmation**: a `confirm` entry's question shows on its row in the `Urgent` tint and the
  box frame turns `FrameInk::Urgent` until the second Enter or a cancel.

### Keys

- **Quick keys**: each tool shown on the overview may have one letter. A built-in has a default
  letter in the catalog table; a user entry may set `key = "x"` in `[[tools.entry]]`; `[tools]
  keys = { "quick.wifi" = "w" }` overrides a built-in's. A letter is used once: the first claimant
  in display order keeps it, the others show no capsule (logged as counts only). Hidden tools free
  their letters.
- **Letters versus typing** (owner's choice, 2026-11-26): at the overview, with an empty query, a
  letter that has a capsule runs its tool at once. `/` or Space starts the search (the `/` or the
  Space is not part of the query); a character with no capsule also starts the search with that
  character. Once the query is non-empty, every letter is text. Entering the Tools mode from
  Mod+Space's `/` prefix starts in search, letters inert, so a typed path never runs a tool. A
  held letter does nothing (as a held Enter).
- Everything else as today: Enter runs or opens, ← and Esc go back, Esc at the root closes, held
  Enter does nothing, a `confirm` needs a second Enter 0.4 to 3 s later.

### Configuration

New keys in `launcher.toml`, documented in `docs/configuration.md` and the feature page:
`[tools] keys` (built-in id to letter), `[[tools.entry]] key`, `[tools] overview = true`
(`false` keeps today's category list at the root, for a narrow screen or taste).

### How it is built

- `list.rs`: `Boxed` gains overlay lists: `cards`, `chips` (each with a `Tint` fill), `switches`
  (on or off). Painted by `Fonts::paint_boxed` before the text, with the bar's `round_block`
  corner masks.
- `launcher/tools/catalog.rs`: the default letters and a toggle's state source.
- `launcher/tools/nav.rs`: an overview level and its 2-D selection; letter dispatch.
- `launcher/view.rs`: the overview layout, the restyled rows and footer.
- Toggle state: from what the daemon already knows (the quick settings state, Do not disturb,
  Keep awake, Night light, Shield); a tool whose state is unknown shows no switch, never a guess.

### Tests (the house rules: tests first, mutated to prove they bite)

- Pure: the overview layout (columns, bands, `+N more`), letter allocation and conflicts, 2-D
  navigation, the overlays produced for a known catalog, choices grouped in search.
- Painter: overlays drawn inside their cell rectangles, nothing outside the frame.
- Headless: a letter runs its tool after the box closed; the overview opens and closes with
  0 wake-ups at rest; memory: `tools_pss_open_and_closed` bounds unchanged closed, open figure
  printed and recorded in `docs/measurements.md`.
- Guardrails unchanged: logs fixed words, typed text never logged, argv allowlist.

## Related decisions taken the same day

- **Resident budget: 15 MB at rest** for everything alpheus keeps resident (menus closed);
  replaces the 120 MB of `docs/SCOPE.md`. What opens on demand (the colour picker's frozen frame,
  a capture) may exceed it while open and must give it back on close. First step: re-measure the
  daemon headless with a realistic module set; then tighten the steady-state budget of
  `tests/memory.rs` (30 MB today) accordingly. To `docs/decisions.md` and `docs/SCOPE.md`.
- **Everything in layer-shell**: the control center becomes a separate layer-shell program
  launched on demand, reusing the daemon's grid and painter through a shared crate; it stays out
  of the resident daemon (isolation, budget). Commands that need a terminal (`sudo`, live
  journal, a unit in the foreground) still open one. Its own design and plan come after this
  change.

## Left to the builder

Card minimum width, exact paddings, the overlay structs' shape, which built-ins get default
letters, the restyled levels' details, within the look of the P2a render.
