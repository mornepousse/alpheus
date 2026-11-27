# Session menu rows and the keybinding cheatsheet: design

Date: 2026-11-27. Status: the owner settled every open question of the draft by its own
recommendation (2026-11-27, "ok go"). Calls marked *(builder's call)* are the writer's.

## Intent and goals

Two things the owner's old scripts did and alpheus does not yet:

- **A configurable ⏻ menu**: rows of the user's own beside the four fixed ones (which stay the
  default; nothing changes without `[power]`), a digit per row, an optional countdown before a
  destructive row acts, cancelled by Esc and by any close.
- **A keybinding cheatsheet**: Tools › System › Keybindings (and `alpheus-launch --binds`) lists
  every binding of the running compositor (Mango first, Sway second), grouped, described,
  searchable, with the panel's tabs, search and detail card.

## Non-goals

No rebinding or editing (the compositor's config is never written); no live capture of pressed
keys; no idle or sleep handling; no watcher of the config files; no countdown on the quick
panel's session buttons nor on the scheduled shutdown (decision Q2: only the ⏻ menu); no
Hyprland or niri parser.

## Anti-goals

- A countdown that survives its popup (a resident timer) or cannot be cancelled.
- Asking the compositor for its bindings: `swaymsg` and `mmsg` stay forbidden words in the
  daemon (`FORBIDDEN_WORDS`), and neither answers with bindings anyway.
- A bind's command, a key, a path or a row's name in a log line.
- A sleep row: alpheus never handles sleep, a user row cannot either.

## Constraints

- **Lightness.** Closed: code pages only, no read, thread, timer or watcher.
- **Session safety.** `FakeExecutor` and a file-read seam; no test reads the user's `~/.config`;
  the headless test uses the harness's disposable Sway and a fixture file.
- **Privacy.** Logs carry fixed words, positions and counts; commands appear on screen only.
- **Compositors.** Mango is the default session, Sway the fallback. The dialect follows
  `alpheus_common::compositor::detect`; on another compositor the Keybindings entry is hidden.

## Approaches considered

1. **The cheatsheet as a level of the Tools panel, read from files at open (picked).** The panel
   already has tabs, search, detail card and a box sized per open; one synchronous read of a few
   local files is under a millisecond, so no thread. Decision Q1 (a).
2. **Its own popup.** A second drawing of tabs and search for the same rows.
3. **Ask the compositor.** Breaks the forbidden-word guardrail, and neither IPC returns bindings.

Session menu: a `PowerEntry` wrapping `Session` or a user row (picked) rather than reworking the
shared `Session` enum, which would drag in the quick panel's row that Q2 keeps fixed
*(builder's call)*.

## The design

### Session menu

The fixed rows (each when its `[quick]` command is set and `builtins` is true), then the user
rows in file order; a digit capsule at the right of rows 1 to 9 (of at most 12). The width
follows the longest text instead of the fixed 16 cells.

| Key (`bar.toml`) | Default | Meaning |
| --- | --- | --- |
| `[power] countdown` | `0` | seconds counted after a confirming activation, 0 to 30; 0 is off |
| `[power] builtins` | `true` | `false` hides the four fixed rows |
| `[[power.entry]]` | none, at most 8 | `name` (1 to 24 characters), `icon` (one character, default `•`), `run` (argv, no shell, not empty), `confirm` (default `false`), `countdown` (0 to 30, overrides the global one for this row) |

- The global `countdown` applies to rows that ask (Reboot, Power off, a user row with
  `confirm = true`); a row's own `countdown` applies to that row whether it asks or not
  *(builder's call)*.
- A user row whose `run` names `suspend`, `hibernate`, `hybrid-sleep` or `dpms` (any argument,
  case aside) makes `bar.toml` invalid with the reason, as every other refused key does.
- A digit selects its row and activates it (`session::activate`'s rule: a row that asks asks; a
  second press of the same digit 0.4 s to 3 s later confirms). A held digit never acts (repeat
  only moves, as today). Decision Q5 (a).
- **Countdown.** The confirming activation of a row with a countdown arms it instead of running:
  the row reads `⏻ Power off in 5 s · Esc` in the warning colour, redrawn each second; at 0 the
  row runs and the menu closes. Any key cancels it (Esc cancels only; a second Esc closes), but
  Enter and Space, which do nothing; a hover or press on another row cancels it; closing the
  menu by any route drops it. It lives in the menu's state and the menu's one `DeadlineTimer`.
- The ⏻ button and `--power` exist when any row exists (fixed or user). A user row is logged by
  position: `power: entry 2`.

Built: `config.rs` (`PowerConfig`, `PowerEntryConfig`, `check_power`), `power/mod.rs`
(`PowerEntry`, `Power` with the countdown, digits, computed width), `power/app.rs` (`power_spawn`
by `PowerEntry`, `power_refusal`, the timer following `Power::deadline`), `segments.rs` and
`launcher/tools/app.rs` (`power::any_rows`). `session.rs` keeps `Session`, `activate`, `ask`,
`expire` unchanged.

### Cheatsheet

What the user sees: the command panel's box, its search field, a tab strip `All` then one tab per
group, the list on the left (one bind per two-line row: the combo in a capsule then the
description; under it the command, dim), the detail card on the right (the combo, the group, the
description, the full command wrapped, the file it came from by name only), a footer
`Tab Group · type to search · Esc Back`. The All tab shows every group as a section. Typing
filters at once (no letters live here); the results are grouped by group, ordered by their best
match. Enter does nothing *(builder's call: running a bind's command is out of scope)*. Esc and
← on an empty query go back to the panel, or close the box when `--binds` opened it. A line
under the list says `N lines not understood` when the parser skipped some, and `No keybindings
found` when no file was readable. With `[tools] panel = false` the level is the plain
one-line list.

Sources (decision Q3 (a) + the /etc fallback): `launcher.toml` `[tools] binds_files = []` (paths,
`~` expanded), else per dialect:

- **Mango:** `$XDG_CONFIG_HOME/mango/config.conf` (else `~/.config/…`), following `source=`
  (relative to the including file). Lines `bind`, `bindl`, `bindsl`, `bindr` (`MODS,key,fn[,args]`;
  `code:N` keys kept as written); `mousebind`, `axisbind`, `gesturebind` skipped silently. Groups
  from `# ── Name ──` comment lines. `$NAME` expanded when an `env=NAME,value` of the chain
  defines it, else kept.
- **Sway:** `$XDG_CONFIG_HOME/sway/config`, else `/etc/sway/config`, following `include` (a
  `*` in the last component lists that folder, sorted). `set $var value` expanded; `bindsym` and
  `bindcode` with their flags (`--release`, `--locked`, …) skipped over; a `mode "x" { … }` block
  is the group `x`; outside one, `# ── Name ──` comments as for Mango, else `Default`.
- **Bounds:** 8 files, 512 KiB in all, 1,000 binds, 32 groups, a line cut at 1 KiB; past one, the
  first are kept and one log line of counts is written. An include cycle stops at the file seen.
- **Descriptions** (decision Q4 (a)): a trailing `# comment` on the bind's line, else the comment
  line right above it (not a group header, not separated by a blank line), else a humanised
  function from a fixed table (`spawn,firefox` → `Run firefox`, `view,2` → `Go to desktop 2`,
  `killclient` → `Close window`; Sway `exec`, `workspace number N`, `kill`, `focus left`, …);
  an unknown function shows raw.
- Combos print as `Super+Shift+A` (Mango `SUPER`, Sway `Mod4`/`$mod` → `Super`, `Mod1` → `Alt`,
  `Control` → `Ctrl`; a one-letter key upper-cased).

Built: a pure folder `launcher/tools/binds/` (types, `Files` seam, `load`, bounds; `mango.rs`,
`sway.rs`, `describe.rs`), the built-in `tools.binds` (`BuiltinAction::Binds`), `Level::Binds`
in `nav.rs`, `Outcome::OpenBinds` answered by the glue (`tools/app.rs`, the only file reading,
through `RealFiles`), the level's rows ranked by the existing Tools ranker, `view/binds.rs` drawing with the
panel's pieces (`tools_chip`, a name-agnostic tab strip, `key_hints`), and `Request::Binds` /
`--binds`.

## The owner's decisions applied

Q1 (a) panel level plus `--binds`; Q2 (a) countdown only in the ⏻ menu; Q3 (a) `binds_files`
plus `/etc/sway/config`; Q4 (a) comments then humanised; Q5 (a) digit asks, same digit confirms.
Cluster rules: every task ends with a RAM measurement recorded in `docs/measurements.md`; 15 MB
at rest for everything. This reverses the decision "The power menu runs the `[quick]` commands
and has no setting of its own": it keeps running them, and gains `[power]`; `docs/decisions.md`
says so and why.

## Cost

- Closed: code only (about 20 KB binary); the at-rest wake-up tests stay at 0.
- Cheatsheet open: about 15 KB read, about 100 rows, 30 to 40 KB heap, freed at close.
- Menu open: a few hundred bytes for user rows; the countdown wakes once a second, at most 30 s.

## Tests

Pure: parsers on anonymised fixtures (chains, groups, descriptions, `set`, modes, bounds,
cycles), combos, the level's search and grouping; the menu's user rows, digits, countdown. Guardrails: the power
activation guardrail extended to user rows, the power wiring one adapted as strict, the binds
folder pure, forbidden words untouched. Snapshots of the cheatsheet and the menu. Headless:
`--binds` on the harness Sway, a digit running a user row, Esc on a countdown. Memory against
`main`, 0 wake-ups at rest.

## Left to the builder

Capsule placement and widths, the cheatsheet's column split, footer and empty-line wording, the
humanised phrases beyond those above, within the panel's look as the polish branch leaves it.
