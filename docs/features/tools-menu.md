# The Tools menu

One box for "do something to my session", drawn as the launcher's own box. This page says what it
does, how to use it, how to configure it, how it is built, which rules the tests enforce on it, and what
it costs. It describes the code as it is today. This page covers the menu's glue (`launcher/tools/app.rs`
and `join.rs`); the other files of the folder are summarised in "How it is built".

## What it does

`alpheus-launch --tools` opens the launcher in a Tools mode: categories to browse, and a search that
looks at every entry of every category at once. Typing `/` as the whole query in Mod+Space jumps into it
too, so typing a path that starts with `/` does as well, from its first character (`[prefixes] tools = ""`
gives the key back). Nothing is resident while it is closed: no thread, no timer, no catalog.

- **Built-in entries**, each shown only when it can work: Sound, Wi-Fi, Bluetooth, VPN, Shield, Do not
  disturb, Keep awake and Night light, the control center's tabs, Notes, Timer, Clipboard, Emoji, Lock,
  the Power menu and an optional "Reload the bar". There are 23.
- **Your own entries**, declared in `launcher.toml`: a command with no shell, optionally in the terminal,
  with a confirmation, an on/off state, or choices.
- **A way in from a key:** `alpheus-launch --power` opens the ⏻ menu directly.

## Using it

**Browsing.** With the query empty the box lists categories (`Quick settings`, `Control center`,
`Utilities`, `Captures` (only with screencopy), `Session`, and your own). Enter or → opens one; ← or Esc goes back up; Esc at the root
closes. Every action closes the box: a run, a built-in opening its feature, a toggle. Only opening a
level, going up, a confirmation question and a program not on the `$PATH` (said in the box) keep it open.

**Searching.** Typing filters the current level: at the root, every entry of every category, the
category dimmed after the entry and fixed choices as `Entry · Choice`; inside a category or a list of
choices, that level only. Entries also rank in Mod+Space's normal search.

**Confirming.** An entry with `confirm = true` turns into `Name? ⏎` on the first Enter and runs on a
second one between 0.4 s and 3 s later. A double Enter never runs it.

**A held Enter** does nothing in the Tools mode, so the repeat of the key that ran a built-in does not
act in the popup it just opened.

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
| `root` | `[]` | Built-in ids also listed at the root, first, in this order |
| `reload` | `[]` | The argv of "Reload the bar"; empty hides the entry |

`[[tools.category]]` (at most 16) has `name`, `icon` (one character) and `order`. `[[tools.entry]]` (at
most 128) has:

| Key | What it does |
| --- | --- |
| `name`, `category`, `icon`, `order` | How the entry is listed; no `category` puts it at the root |
| `run` | The argv, run as written (no shell); with choices, exactly one element is `{choice}` |
| `terminal`, `hold` | Run in launcher.toml's `terminal`, or its `terminal_hold` |
| `confirm` | Ask `Name? ⏎` first (see above) |
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

- `config.rs`, `catalog.rs`, `nav.rs`: pure. The configuration and its bounds; the table of the 23
  built-ins (`BUILTINS`), the `Context` of booleans that decides which show, and the merged categories and
  entries; the levels, rows and confirmation question.
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
- `the_tools_glue_spawns_only_through_launch`: `app.rs` starts no process itself and calls the
  launcher's start function exactly once.
- `every_builtin_argv_stays_on_the_allowlist`: a built-in's argv is the configured command,
  `[actions] open_tab`, or the terminal running `alpheus --tab`, never through a shell.
- `the_parallel_branches_are_named_only_in_tools_join`, `the_join_hooks_are_filled_in`: the folder's other
  files never name `clipboard_availability`, `launcher_open_clipboard`, `shield_open_detail`,
  `ShieldConfig` or the `"diagnostics"` slug, so `join.rs` is the one file that calls the clipboard's and
  the shield's own functions; and its three answers are real.
- `the_tools_timer_is_disarmed_by_close_and_by_a_panic` and
  `the_tools_reads_belong_to_the_tools_mode_and_stop_with_the_box`: the timer and the reads end with the
  box, however it ends.
- The thread and timer inventory in `guardrails.rs` counts the reader's `thread::Builder` and the one
  `DeadlineTimer`.
- The headless tests (`tools_opens_and_a_builtin_runs_after_the_box_closed`,
  `tools_drills_runs_and_keeps_argv_whole`, `tools_confirm_needs_a_second_enter_after_0_4_s`,
  `tools_states_are_read_and_a_hung_one_killed`, `tools_choices_cmd_filters_and_runs`,
  `tools_a_held_enter_drills_once_and_runs_nothing`, `tools_at_rest_no_wakeups_after_a_cycle` and the
  built-in tests for Wi-Fi, clipboard, emoji, the tabs, the shield and diagnostics) use a recorder
  script on a private `PATH`; none runs a real command of the session.
- The reasons are in [decisions.md](../decisions.md): "The Tools menu reaches the clipboard, the shield
  and the diagnostics tab through one file", "A built-in replays the daemon's own verb after the box has
  closed", "A tab opens through `[actions] open_tab`, else in the terminal", "Everything a built-in
  spawns runs in its own scope", "The Tools menu reads states and choices one job at a time on a short-lived
  thread".

## What it costs

One release run on the branch, with no back-to-back base (the [memory tables](../measurements.md) have the figures):

- **Open, 20 cycles:** the daemon's own anonymous memory +336 KB open over closed, +12 KB closed again,
  +64 KB after 20 more cycles; the shm pools +1,316 KB open and back to 56 KB closed; 6 threads before
  and after (`tools_pss_open_and_closed`).
- **A `choices_cmd` of 2,000 lines:** anonymous memory +1,880 KB open, +144 KB closed again
  (`tools_choices_2000_lines`).
- **At rest:** 0 loop wake-ups in 5 s after a cycle, with a question pending and closed and after one
  left to expire (`at_rest_no_wakeups_after_tools`).
