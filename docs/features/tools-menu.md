# The Tools menu

One box for "do something to my session", drawn as the launcher's own box. This page says what it
does, how to use it, how to configure it, how it is built, which rules the tests enforce on it, and what
it costs. It describes the code as it is today. This page covers the menu's glue (`launcher/tools/app.rs`
and `join.rs`); the other files of the folder are summarised in "How it is built".

## What it does

`alpheus-launch --tools` (Super+X) opens the launcher on the command panel: a search field, a strip of
tabs (All, then each category), the open tab's tools on two lines each (name, then what the tool
does) beside a detail card for the selected one, and a footer of keys. Each tool may carry a letter
that runs it in one key. The search looks at every tool of every category at once. Typing `/` as
the whole query in Mod+Space jumps into the Tools mode too (as a plain list, never the panel), so
typing a path that starts with `/` does as well, from its first character (`[prefixes] tools = ""`
gives the key back). Nothing is resident while it is closed: no thread, no timer, no catalog.

- **Built-in entries**, each shown only when it can work: Sound, Wi-Fi, Bluetooth, VPN, Shield, Do not
  disturb, Keep awake and Night light, the control center's tabs, Notes, Timer, Clipboard, Emoji, the
  captures (Screenshot, Area, Area (save), Annotate, Record screen, Record area, Stop recording, Colour
  picker, Text from area (OCR): a tool's row only when its `[captures]` program is set and found, the
  Record rows only while nothing records, Stop only while a recording runs), Schedule shutdown, Kill a
  process, Lock, the Power menu, an optional "Reload the bar", and the Diagnostics entries (network
  scan, USB events and reset, the journal presets). The table has 45.
- **Your own entries**, declared in `launcher.toml`: a command with no shell, optionally in the terminal,
  with a confirmation, an on/off state, choices, or a text typed in the box.
- **A way in from a key:** `alpheus-launch --power` opens the ⏻ menu directly.

## Using it

### The command panel

**The box.** It is sized once, at open, for the output it opens on: about 106 cells wide and 30 rows
tall on a wide output, never narrower than the launcher's usual box, at most 90 % of the output's
width and 80 % of its height. It keeps that size for the whole open: the search and the levels draw
inside it. A Kill a process picked from the panel reopens the box at the kill list's own size.
`launcher.toml`'s `rows` does not size it.

**Tabs.** All is the tab at open. It lists `PINNED` (the built-ins of `[tools] root` and your entries
with no `category`), `RECENT` (the last 4 distinct tools run, read from the launcher's existing
history, so a tool run from Mod+Space's search counts too; a pinned tool is not repeated there, and a
tool that no longer shows is skipped), then one section per category, in category order, under its
name in capitals and its colour dot. An empty section is not shown. A category tab lists that
category's tools alone. **Tab** and **Shift+Tab** move between tabs, wrapping; a click on a tab opens
it (and ends a search). ↑ ↓, PageUp, PageDown and the wheel move the selection; the list scrolls to
keep it in view.

**A tool's row** has its glyph on a tile in its category's colour, its name, and under it its
description (a built-in's is fixed; an entry of yours shows its `description`, else its category's
name). At the right: a drawn switch for a tool whose state is known (the slot is kept while the
state is read), `›` for one that opens a level, and its letter's capsule.

**The detail card**, at the right of the list (the list takes the box alone when it is narrower than
80 cells), shows the selected tool: its tile, its name in bold, `<Category> · built-in` or `· yours`,
its description, then its actions derived from what the tool is (`Run ⏎` or `Open ⏎`, `Choose →`,
`Type <label> ⏎`, `Turn on/off ⏎`, `Confirm ⏎ ⏎`, `Quick key <letter>`), and `Pinned` for a pinned
tool. The levels below a tool (its choices, its prompt) keep the card.

**Letters.** A letter acts on the tool whose capsule is drawn on screen as Enter on its row
would: it runs it and closes the box, opens its level, or asks its question first (`confirm`). A tool shown twice (pinned and in its category)
keeps one letter. Letters are given in display order: Pinned first, then each category in order; a
user entry wants its `key`, a built-in its `[tools] keys` letter, else its default. A letter wanted
twice goes to the first, and one log line counts the letters taken twice (`tools: N letters taken
twice`), never which. A tool the open tab lists but has
scrolled out of the pane (or a box drawn shorter cuts off) is not drawn: its letter starts the
search, as a letter without a capsule does, until the list scrolls to it. A letter is never live:

- after `/` or Space, which start the search (the letters then are text);
- for a letter with no capsule in the open tab: it starts the search with itself;
- in Mod+Space: its box never shows the panel, and its `/` jump has no live letters;
- held: the repeat of a key never acts twice (a letter that opened a level types nothing below).

**Searching.** Typing (after `/` or Space, or a character with no capsule) filters every tool of every
category. The results are grouped by category, the groups ordered by their best result, each under its
label; tools with no category sit under `PINNED`. The matched characters are drawn in the accent
colour. Erasing the query, then ⌫ once more after `/` or Space, gives the letters back; Esc closes
the box. Esc on a level opened from the search goes back to the tool's row in the open tab.

**Chips.** In the search, an entry with fixed choices shows them as chips on its second line. No chip is
ever picked by default: a chip is picked by ← and →, by a click on it (which never runs it), or by a
query whose every word is found, case aside, in the chip's own text (`quick ports` picks
`Quick scan (common ports)`). Enter runs the entry with the picked chip; with none picked, Enter opens
its choices as a level. A `choices_cmd` entry has no chips: Enter or → opens its level.

**Confirming** works as below; while a question waits for its second Enter, the box's frame turns
to the urgent colour.

**Turned off.** `[tools] panel = false` keeps the category list described next, with no letters.

### The category list

**Browsing.** With `panel = false` and the query empty the box lists categories by their order (`Quick settings` 10,
`Control center` 20, `Utilities` 30, `Captures` 40 (only with screencopy), `Diagnostics` 70, `System` 80,
`Session` 90, and your own, 50 by default; a category with nothing to show is not listed). Enter or → opens one; ← or Esc goes back up; Esc at the root
closes. Every action closes the box: a run, a built-in opening its feature, a toggle. Only opening a
level, going up, a confirmation question and a program not on the `$PATH` (said in the box) keep it open.

**Searching.** In the category list, typing filters the current level: at the root, every entry of every category, the
category dimmed after the entry and fixed choices as `Entry · Choice`; inside a category or a list of
choices, that level only. Entries also rank in Mod+Space's normal search.

**Confirming.** An entry with `confirm = true` turns into `Name? ⏎` on the first Enter and runs on a
second one between 0.4 s and 3 s later. A double Enter never runs it.

**Typing a value.** An entry with `ask = "Target"` has a prompt step: Enter on it (or, when it has
choices, on one of its choices) opens a level with one row, the label, and what you type goes in the
box's own input line, not into a filter. Enter runs with the text; Esc goes back (dropping the text, empty or not), and ← or ⌫ does
the same on an empty line: to the choices, or to the category (the root for an entry with none). A blank text runs nothing
and the prompt stays. Clicking another mode with the prompt open drops the text and the prompt. The text
reaches `run` as the one whole element `{text}`, the same rule as `{choice}`: spaces, quotes and `;`
stay inside that one argument, and alpheus never passes it through a shell. A leading `-` is still
an option to the program it reaches, so an entry of your own should put `--` before it
(`run = ["ping", "--", "{text}"]`). It is never logged and
never kept in the history (the history records the entry, and its fixed choice if it has one). A
launch that fails keeps the box open, but the typed text is gone: type it again. An
entry with `ask` and `confirm` is refused: the typed text is the confirmation.

**A held Enter** does nothing in the Tools mode, so the repeat of the key that ran a built-in does not
act in the popup it just opened.

### Kill a process

`alpheus-launch --kill`, or Tools › System › Kill a process, opens the box on a list of your own
processes: the name, the command line (dim), the pid, the CPU and the memory. At open the rows are
sorted by memory; 300 ms later one more read of `/proc` fills in the CPU and sorts by it, then by
memory, and the highlighted process stays highlighted (by its pid, not its place). Nothing more is
read while the list is open. Typing filters on the name and the whole command line (its first 512
bytes).

- **Enter** asks the process to quit (SIGTERM).
- **Ctrl+Enter** asks `Force kill <name>? Enter again`; a second Enter between 0.4 s and 3 s later
  sends SIGKILL, and any other key cancels the question.
- A held key never signals, and keys within 0.4 s of a signal do nothing: a double tap signals once.
- What the signal did is said in the box only (`asked <name> to quit`, `killed <name>`,
  `<name>: already gone`, `<name>: not allowed`, `<name>: pid reused, nothing sent`,
  `<name>: could not signal`). A signalled row stays, marked, so a process that ignored the request
  stays reachable and no neighbour slides under the highlight; a process that is gone leaves the
  list, and then no row is selected until a key moves.
- **Never offered:** another user's process, PID 1, alpheus, the session leader, the compositor (by
  the pid of its socket's peer, and by its name), every ancestor of alpheus and of the compositor,
  and a process named `systemd` (the user manager). At most 4,096 processes are read.
- **A reused pid is never hit.** Before a signal, a pidfd is opened on the pid and its start time
  (field 22 of `/proc/<pid>/stat`) is read again: a start time that differs from the listed one
  sends nothing, and the signal goes through that pidfd, which names the process it was opened on
  for ever. `kill(2)` by pid is used only on a kernel without `pidfd_open`.
- No log line and no notification carries a name, a pid or a command line.

A second `--kill` less than 250 ms after the one that opened it is ignored; an older daemon exits 3.

### Schedule shutdown

Tools › System › Schedule shutdown (shown when `timer` is in `modules` and `[quick] poweroff` is
set) has the choices `30 min`, `1 h`, `1 h 30`, `2 h` and `3 h`; while a shutdown is pending, a first
choice `Cancel shutdown (in 1h20)` cancels it. A duration replaces the shutdown pending. The rest (the
chip, the warning a minute before, the state file, `--timer shutdown`) is the timer's: see
[the focus timer](timer.md#the-scheduled-shutdown).

### The diagnostics tools

The scans, the USB events and the USB reset run in `terminal_hold`, so `sudo` can ask there and the
output stays readable; alpheus itself never runs as root.

- **Network scan** (only with `nmap` on the `$PATH`): ten fixed choices, in this order:
  `Host discovery (ping sweep)`, `Quick scan (common ports)`, `Full scan (all ports)`,
  `Services and versions`, `Default scripts and versions`, `Specific ports`,
  `UDP scan (top 20, sudo)`, `Vulnerabilities (--script vuln)`, `OS detection (sudo)`,
  `Aggressive scan -A (sudo)`. Every scan but the sweep asks `Target (IP or host)`; `Specific ports`
  asks `Target, then ports (host 22,80,443)`, the first word being the target and the rest the ports
  (digits, commas and dashes only). The target comes after `--`, so one that starts with `-` is a host,
  never an option. The sweep (`nmap -sn`) takes the subnet of the default route's interface from
  `/proc/net/route`; with no connected route, a link-local one, or a net narrower than /30 or wider
  than /16, the box says `no network` and nothing runs.
- **USB events:** `udevadm monitor --udev --subsystem-match=usb` and `journalctl -kf -n 30 --grep=usb`
  side by side in one held terminal (a fixed `sh -c` script whose trap stops both together).
- **Reset USB (xHCI):** asks `Reset USB (xHCI)? ⏎` and runs `[tools.builtin] usb_reset` on a second
  Enter (default `sudo modprobe -r xhci_pci && sudo modprobe xhci_pci`); `[]` hides it.
- **The journal presets** (`Errors this boot`, `USB`, `Suspend and resume`, `Out of memory`,
  `Thermal and throttling`, `NetworkManager`, `Live journal`, `Logs of a service…`), shown when the
  control center's Diag tab is: each runs `alpheus --tab diagnostics --preset <slug>` in
  launcher.toml's `terminal` (never through `[actions] open_tab`, which takes only a tab; see
  [the control center](control-center.md)).

### Captures

The Captures category (only when the compositor offers screencopy) replays
`alpheus-launch --capture <word>` after the box has closed: Screenshot (`full`), Area, Area (save),
Annotate, Record screen, Record area, Stop recording, Colour picker (`pick`) and Text from area
(OCR). Annotate, OCR and the two Record rows show only when their `[captures]` program is set and
on the `$PATH`; the Record rows hide while a recording runs, and Stop shows only then. What each does
is on [the captures page](captures.md).

### From a key

**`alpheus-launch --tools`** replaces any other open launcher mode, then toggles; it exits 3 when the
bar is not running or is older than the verb, the one code on which a script falls back to another menu.
`alpheus-launch --power` exits 2 when no `[quick]` session command is set.

## Configuration

`[tools]` and its tables in `launcher.toml` (the struct is `ToolsConfig` in `launcher/tools/config.rs`),
and `[prefixes] tools` (default `/`; `""` turns the prefix off).

| Key | Default | What it does |
| --- | --- | --- |
| `builtins` | `true` | `false` shows no built-in entry |
| `hide` | `[]` | Built-in ids never shown (`"tab.power"`, `"quick.shield"`, …); an unknown id is logged and ignored |
| `root` | `[]` | Built-in ids also listed at the root, first, in this order (the panel's Pinned) |
| `panel` | `true` | `false` keeps the category list, with no letters |
| `keys` | `{}` | A built-in id to its letter, `{ "quick.wifi" = "w" }`; an unknown id is logged and ignored |
| `reload` | `[]` | The argv of "Reload the bar"; empty hides the entry |
| `[tools.builtin] usb_reset` | `sh -c 'sudo modprobe -r xhci_pci && …'` | The argv of "Reset USB (xHCI)", run in `terminal_hold`; `[]` hides the entry |

`[[tools.category]]` (at most 16) has `name`, `icon` (one character) and `order`. `[[tools.entry]]` (at
most 128) has:

| Key | What it does |
| --- | --- |
| `name`, `category`, `icon`, `order` | How the entry is listed; no `category` puts it at the root (the panel's Pinned) |
| `key` | One letter, `a` to `z`, that runs the entry from the command panel |
| `description` | One line (1 to 80 characters) under its name in the panel; none shows its category's name |
| `run` | The argv, run as written (no shell); with choices, exactly one element is `{choice}`; with `ask`, exactly one element is `{text}` |
| `terminal`, `hold` | Run in launcher.toml's `terminal`, or its `terminal_hold` |
| `confirm` | Ask `Name? ⏎` first (see above); not with `ask` |
| `ask` | A label: the entry asks for a text first, after its choice if it has choices (see "Typing a value") |
| `state` | An argv run once when the Tools mode is entered; exit 0 is on, any other exit off. At most 16 entries have one |
| `choices` | Fixed choices, at most 32 |
| `choices_cmd` | An argv whose output lines are the choices, run at each drill-in |

A command's reads (`state`, `choices_cmd`) are cut at 1 s, 2,000 lines and 256 KiB; a `state` that hangs is
killed at 1 s and shows nothing (neither on nor off). Past a bound the first
ones are kept and one line of counts is logged. Invalid structure makes `launcher.toml` invalid, as for
every other key. A built-in that opens a tab runs `[actions] open_tab` with `{tab}`, else the launcher's
terminal with `alpheus --tab <name>`. An entry of a category cut by the 16-category bound is dropped,
not moved to the root.

**Binding it.** Bind `alpheus-launch --tools` yourself (Mango `bind=SUPER,x,spawn,alpheus-launch --tools`,
Sway `bindsym $mod+x exec alpheus-launch --tools`). Exit 3 means the daemon is too old or not running, and
a bind can fall back to a menu of your own on that code. Names, commands and lines are never logged (an
entry is named by its position in the file).

**The `/` caveat.** A typed path (`/etc/hosts`) in Mod+Space's All mode jumps into the Tools menu;
`[prefixes] tools = ""` gives the key back. A `tools` prefix the file never sets that clashes with your
`commands` or `windows` prefix is turned off with a note; one the file sets is refused.

**Moving from a menu script.** `[[sources]]` keeps working beside the Tools menu (an entry in both shows
twice in Mod+Space's search: drop the source once the menu is yours).

## How it is built

In `crates/alpheus-daemon/src/launcher/tools/`:

- `config.rs`, `catalog.rs`, `nav.rs`: pure. The configuration and its bounds; the table of the 45
  built-ins (`BUILTINS`), the `Context` of booleans that decides which show, and the merged categories and
  entries; the levels (including the prompt level of an `ask` entry), rows and confirmation question.
- `panel.rs`: pure. The command panel's model: its tabs (`Tab`), the rows of the open tab (All's
  Pinned, Recent and category sections, or one category's tools), `recent_entries` (the last 4
  distinct tools of the history that still show) and `actions`, the detail card's actions derived from
  what a tool is. `nav.rs`'s `ToolsState` holds the panel, its box size (fixed for the open), the
  picked chips and the tab moves; `catalog.rs` holds the descriptions, the default letters and
  `claim_letters`.
- `read.rs`: the reader. One short-lived thread named `alpheus-tools` per read of states or of a
  `choices_cmd`, through the executor's `capture`.
- `app.rs`: the wiring on the daemon's state. `context` builds the `Context` from the configuration and
  what the app knows; `tab_argv` and `builtin_argv` say what a built-in spawns; `Scheduler` is the
  reader's one-job-at-a-time queue; `ToolsApp` holds the question's `DeadlineTimer` and the scheduler;
  and the methods of `App` open the menu, adopt the reader's mail, run a built-in after the box closed and
  arm the timer.
- `join.rs`: the only file of the folder that calls the clipboard's, the shield's and the diagnostics
  tab's own functions and types; `app.rs` still has a `Join` struct carrying their three answers and
  reads `join::DIAGNOSTICS_TAB`.
- `diag.rs`: pure. The ten scans (`Scan`), the argv of each (`scan_argv`), the check of a typed port
  list, the USB events script, and the `Subnet` seam whose real form (`ProcSubnet`) reads
  `/proc/net/route` once, when the sweep is chosen.

The drawing is the launcher's view: `launcher/view/panel.rs` draws the panel in the box's cells
(search field, tab strip, the list beside the detail card, footer) and maps a click to a tab, a row
or a chip; `view::tools_layout` sizes the box once per open. Its rounded parts are the painter's
shapes (`list::ShapeKind`: `Card` for the detail card, `Chip` for a tile, a capsule, a tab or the
search field, `Band` for the selection over a tool's two lines, `Switch` for a known state), drawn
in pixels under the cells. One font, one size: the card's title is bold, not larger.
`launcher/matching.rs`'s `holds_every_word` decides which chip a query picks.

The kill list is a mode of the launcher (`Mode::Processes`), not a file of the Tools folder:

- `launcher/processes.rs`: pure. The snapshot of one user's processes from a `/proc`-shaped folder,
  the rows built from one or two snapshots, the exclusions, and `send`, the one way a signal leaves,
  through the `Signaller` seam (a test records the signal instead of sending it).
- `launcher/processes_app.rs`: the wiring. The first read at open, the second 300 ms later, the
  exclusions worked out once per open, the real `Signaller` (pidfd, start time, signal), and one
  `DeadlineTimer` that follows the earlier of the second sample and the end of a force-kill question,
  removed whenever no kill list is open. `launcher.toml`'s `proc_dir` only narrows which pids are
  listed (the headless test's seam); start times, ancestors and signals always use the real `/proc`.
- `sources/peer.rs`: the compositor's pid, from the kernel's peer credentials of a connection to its
  socket made and dropped at once (non-blocking: a wedged compositor answers "no" at once). It lives
  under `sources/` because only there may the compositor's sockets be opened.

The Schedule shutdown entry only maps a choice to `timer/shutdown.rs` (see the timer's page).

**The path of a built-in.** Enter on its row; `tools_prepare` checks, while the box is still open, that
the program it would spawn is on the `$PATH` (a missing one is said in the box, which stays open).
Otherwise the launcher closes; `run_builtin` then replays the daemon's own verb (the quick panel, Notes, Timer, the
power menu, Do not disturb), opens the clipboard history or the Shield detail through `join.rs`, reopens
Mod+Space with `:` typed (Emoji), or hands the argv to the launcher's start function through
`tools_spawn`, in its own `systemd-run --user --scope` (a bare spawn when there is no systemd). One log
line names the built-in by its id; a refusal (`refused`) or a spawn that could not start (`could not
start`) is another.

**The reader's schedule.** `tools_offer` hands a job to `Scheduler::offer`. A choices read that a
drill-in asks for goes first; a running states read stops after its current entry and its unread entries
wait behind the choices. A job whose open is gone is never started. Each delivery comes back as a `Mail`
and reaches `tools_state`, `tools_states_done` or `tools_choices`; a stale delivery still frees the slot.

**The timer.** `tools_timer_sync` arms the one `DeadlineTimer` to the pending question's end and removes
it when there is none; `tools_tick` expires the question. `launcher_close` and `launcher_fail` both sync it.

## Rules it keeps

- `the_tools_pure_parts_have_no_panicking_path_and_log_nothing`: `config.rs`, `catalog.rs` and `nav.rs`
  never panic and log nothing.
- `the_tools_logs_are_fixed_words`: a log line of the menu is a fixed word, a built-in's id, an entry's
  position or counts, never a name, a command, a choice or a line.
- `the_typed_text_is_one_whole_argv_element`, `text_is_never_recorded_in_history_nor_shown_by_debug`,
  `a_built_ins_choice_is_recorded_under_its_key_and_its_text_never`,
  `a_typed_scan_target_never_shows_in_debug_output`, `a_mode_click_from_the_prompt_drops_the_typed_text`:
  a typed text is one argv element, neither the history nor a `{:?}` of the action shows it, and it
  never becomes another mode's query. The log lines are held to fixed words by
  `the_tools_logs_are_fixed_words`.
- `the_kill_list_has_no_panicking_path_and_logs_fixed_words`: `processes.rs`, `processes_app.rs` and
  `sources/peer.rs` never panic (a panic's message would carry a process's name); the pure files and
  the peer lookup log nothing, the glue fixed words only. `pid_1_alpheus_and_the_session_leader_are_never_offered`,
  `every_ancestor_of_the_compositor_and_of_alpheus_is_excluded`,
  `an_excluded_pid_is_refused_even_when_asked_for`, `a_reused_pid_is_never_signalled`,
  `a_pid_that_could_reach_a_group_or_init_is_refused` and `only_a_kernel_without_pidfd_falls_back_to_kill`
  pin what may be signalled; `closing_the_list_drops_its_timer`, that nothing stays.
- `the_shutdown_is_wired_through_one_path`: see [the focus timer](timer.md#rules-it-keeps).
- `the_tools_menu_hides_the_captures_without_screencopy`: no Captures row without the protocol.
- `the_tools_glue_spawns_only_through_launch`: `app.rs` starts no process itself and calls the
  launcher's start function exactly once.
- `every_builtin_argv_stays_on_the_allowlist`: a built-in's argv is the configured command,
  `[actions] open_tab`, the terminal running `alpheus --tab` (with `--preset <slug>` for a journal
  preset), or one of the scans' exact `nmap` lines, never through a shell except two `sh -c` scripts in the held terminal: the USB events (two streams
  share one terminal, fixed) and the USB reset (`modprobe -r` then `modprobe`, chained; the default of
  `[tools.builtin] usb_reset`, which the owner may replace). Nothing typed ever goes into either.
- `the_parallel_branches_are_named_only_in_tools_join`, `the_join_hooks_are_filled_in`: the folder's other
  files never name `clipboard_availability`, `launcher_open_clipboard`, `shield_open_detail`,
  `ShieldConfig` or the `"diagnostics"` slug, so `join.rs` is the one file that calls the clipboard's and
  the shield's own functions; and its three answers are real.
- `the_tools_timer_is_disarmed_by_close_and_by_a_panic` and
  `the_tools_reads_belong_to_the_tools_mode_and_stop_with_the_box`: the timer and the reads end with the
  box, however it ends.
- The thread and timer inventory in `guardrails.rs` counts the reader's `thread::Builder` and the one
  `DeadlineTimer`.
- The command panel's pure tests: `all_lists_pinned_recent_then_categories`,
  `recent_is_deduplicated_and_skips_pinned`, `recent_holds_at_most_four` and
  `recent_skips_tools_that_do_not_show` (`tools/panel/tests.rs`) pin the tabs and sections;
  `a_letter_runs_the_tool_its_capsule_shows`, `a_letter_of_a_tool_not_in_the_tab_starts_the_search`,
  `a_letter_without_a_capsule_starts_the_search_with_it`, `a_held_letter_runs_nothing`,
  `a_tool_shown_twice_keeps_one_letter`, `a_mod_space_jump_into_tools_never_runs_a_letter` and
  `panel_off_keeps_today_s_root` (`model/tests.rs`) pin when a letter acts;
  `enter_without_a_picked_chip_opens_the_choices`, `a_query_picks_the_matching_chip_of_each_entry`,
  `a_click_on_a_chip_picks_it_and_runs_nothing` and `a_click_on_a_picked_chip_selects_its_row` pin
  that a choice runs only once the user picks it; `every_description_starts_with_a_verb` the
  descriptions' style. The view's snapshots (`panel_home`, `panel_category`, `panel_search`) and
  `a_panel_drawn_in_a_narrower_box_keeps_its_shapes_inside` keep the drawing inside the box.
- `launcher/tools/panel.rs` is in the pure files of
  `the_tools_pure_parts_have_no_panicking_path_and_log_nothing`.
- The headless tests `tools_a_letter_runs_its_builtin_after_the_box_closed`,
  `tools_panel_slash_then_letters_search` (after `/` or Space a letter is text; the letters taken
  twice are one count in the log), `tools_panel_tab_changes_the_tab` and
  `tools_panel_at_rest_no_wakeups_after_a_cycle` drive the panel through a disposable Sway.
- The headless tests (`tools_opens_and_a_builtin_runs_after_the_box_closed`,
  `tools_drills_runs_and_keeps_argv_whole`, `tools_confirm_needs_a_second_enter_after_0_4_s`,
  `tools_states_are_read_and_a_hung_one_killed`, `tools_choices_cmd_filters_and_runs`,
  `tools_a_held_enter_drills_once_and_runs_nothing`, `tools_at_rest_no_wakeups_after_a_cycle`,
  `kill_list_opens_and_signals_by_enter` (its `proc_dir` lists only the test's own children),
  `a_diag_preset_entry_opens_the_tab_on_it` and the built-in tests for Wi-Fi, clipboard, emoji, the
  tabs, the shield and diagnostics) use a recorder
  script on a private `PATH`; none runs a real command of the session.
- The reasons are in [decisions.md](../decisions.md): "The Tools menu reaches the clipboard, the shield
  and the diagnostics tab through one file", "A built-in replays the daemon's own verb after the box has
  closed", "A tab opens through `[actions] open_tab`, else in the terminal", "Everything a built-in
  spawns runs in its own scope", "The Tools menu reads states and choices one job at a time on a short-lived
  thread", "Only the user's own processes are offered, and a reused pid is never signalled", "A typed
  text is one argv element and is never logged", "The diagnostics tools run in the held terminal, and
  the scan's target is typed", "The Tools menu opens on a command panel; a letter runs only a tool
  whose capsule shows, never after `/` or Space nor from Mod+Space; a choice runs only once the user
  picks it".

## What it costs

The command panel, 3 release runs back to back against `main` (c2f53bdd, the category list) built in
its own target directory (the [memory tables](../measurements.md) have the figures):

- **Open, 20 cycles:** PSS +2,041…+2,225 KB open over closed (main +1,522…+1,654), of which the shm
  pools +1,792 KB (main +1,316: the larger box), back to 56 KB closed; the daemon's own anonymous
  memory +340…+524 KB open, +20…+44 KB closed again (main +12…+32), +80…+112 KB after 20 more cycles
  (main +32…+40; bound 128); 5 threads before and after (`tools_pss_open_and_closed`).
- **At rest, closed:** PSS +276…+340 KB cold over main before any open (the panel's code and
  descriptions in the binary); the steady state stays at 11.1 MB against the 15 MB budget
  (`pss_under_budget_and_stable`). No second font size is loaded.
- **A `choices_cmd` of 2,000 lines:** anonymous memory +1,444…+1,812 KB open, +164…+256 KB closed
  again (main +144…+256) (`tools_choices_2000_lines`).
- **At rest:** 0 loop wake-ups in 5 s after a question closed with the box, after one left to expire,
  and after moves and a letter that ran its tool (`at_rest_no_wakeups_after_tools`).
- **The kill list**, 3 release runs on the branch with no base (new code): PSS +990…+994 KB at the
  first open, +418…+422 KB over cold once closed (the first open's code and glyph pages); the daemon's
  own anonymous memory +4…+24 KB closed again and −8…+20 KB after 20 more cycles; 5 threads before
  and after (`kill_list_pss_open_and_closed`); 0 wake-ups in 5 s at rest after
  (`at_rest_no_wakeups_after_a_kill_list`).
- **The diagnostics entries** add rows to the table and nothing resident: their commands run in the
  terminal, outside the daemon.
