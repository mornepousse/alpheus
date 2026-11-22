# Shield mode

A security mode for public networks, with the work split in two. Your system configuration does the
root work (a firewall chain, stopping sshd and Avahi, USBGuard's policy) behind one privileged
command you already have; alpheus does everything visible: it shows the state, switches it, lists the
USB devices USBGuard blocks, allows one, tells you and fires a hook. This page says what it does, how
to use it, how to configure it, how it is built, which rules the tests enforce on it, and what it
costs. It describes the code as it is today.

## What it does

Nothing in alpheus is specific to one system: the state is "a file exists", the actions are argument
lists, and the device list is USBGuard's `list-devices` text, which alpheus reads itself. alpheus runs
only the commands you configure: no sudo, no polkit call, no firewall or USBGuard call of its own.

With no `[shield]` table in `bar.toml`, nothing exists: no watch, no glyph, no row, and every verb
answers `shield is not configured in bar.toml`.

- **The state.** The mode is on while the `flag` file exists. A change made anywhere (a terminal, the
  boot, another session) shows at once, because the flag's directory is watched. When the file cannot
  be tested (any error but "not found") the state is unknown, never off.
- **The bar.** The `shield` module draws `󰒘` in the green, only while the mode is on. A left click
  opens the quick panel on the Shield row.
- **The quick panel.** A Shield row under the VPN row, with a switch. Space runs `off` or `on` and the
  switch reads `…` until the flag follows; a refused or failed command reads `failed` for 4 s. Enter
  or → opens the blocked USB devices; Enter on one runs `allow` with its id.
- **The verbs.** `alpheus-launch --shield on|off|toggle|status|allow`.
- **Notices.** Through alpheus's own notification server, while it serves.
- **A hook.** `[hooks] shield` runs on each change with `ALPHEUS_SHIELD=on` or `off`.

## Using it

**Switching.** Space on the Shield row, or `alpheus-launch --shield on`, `off` or `toggle`. The verbs
answer once the command has started (exit 0); the outcome reaches you through the switch, the notice
and the log, never through the exit code, because a password can take a minute. A verb for the state
already held runs nothing. A command already running, or an unknown state for `toggle`, is an error
(exit 2).

**Status.** `alpheus-launch --shield status` prints `Shield: on` or `Shield: off` and, when the mode is
on and `blocked` is set, `Blocked USB devices:` with one line per device (`<vid:pid>  ·  <name>`), or
`Blocked USB devices: none`, or `Blocked USB devices: cannot list (<reason>)`. An unknown state prints
`Shield: unknown` and exits 2.

**Allowing a device.** In the panel, open the detail (Enter or →), choose a device and press Enter. A
row reads `…` while `allow` runs, then the list is read again; a failure reads `failed` on that row for
4 s. From a script, `alpheus-launch --shield allow` opens the list as a picker in the launcher's box;
the chosen line runs `allow` and the client hears exit 0, Escape exits 1, nothing blocked gives the
`No blocked USB device` notice and exits 0, the mode off exits 2.

**Notices.** `Shield on` and `Shield off` (with your `on_text` and `off_text`) at any change, whoever
made it; `Shield unchanged` for a failed switch; `USB device allowed` with the device's name;
`USB device not allowed`; `No blocked USB device`.

**Exit codes.** 0 for a started or finished request, 1 for a cancelled picker, 2 for a refusal
(unconfigured, unknown state, busy, mode off, no `allow`), 3 when the bar is not running or is older
than the verb (the one code a caller falls back on).

## Configuration

`[shield]` in `bar.toml` (the struct is `ShieldConfig` in `config.rs`); read at start, so a change
takes effect after `shell-ctl reload-bar`.

| Key | Default | What it does |
| --- | --- | --- |
| `flag` | none | An absolute path to a file; the mode is on while it exists. It may not be in alpheus's configuration directory or share a directory with it (see "The Shield flag may not share a directory with alpheus's configuration" in [decisions.md](../decisions.md)) |
| `on` | none | The command that switches the mode on, as an argument list run as written (no shell) |
| `off` | none | The command that switches it off |
| `allow` | `[]` | The command that allows a blocked device; `{id}` must be one whole element, once. Empty: devices are listed only |
| `blocked` | `["usbguard", "list-devices", "--blocked"]` | The command that prints the blocked devices; `[]`: no device list, no detail |
| `on_text` | `Protections for public networks are on.` | The `Shield on` notice's body, one line, at most 256 bytes |
| `off_text` | `Back to the usual settings.` | The `Shield off` notice's body, same limits |

Shield is configured when `flag`, `on` and `off` are all set. A table with only some of them is a
warning at start and counts as unconfigured. A relative flag, a blank program, `{id}` in the wrong
place and an over-long text are errors of the file. Unknown keys are refused.

```toml
[shield]
flag = "/var/lib/shield/on"
on = ["pkexec", "/run/current-system/sw/bin/shield-root", "on"]
off = ["pkexec", "/run/current-system/sw/bin/shield-root", "off"]
allow = ["pkexec", "/run/current-system/sw/bin/shield-root", "allow", "{id}"]
```

A `modules` list of your own needs `"shield"` added, after `"privacy"`. The panel row and the detail
need the `quick` module; a click on the glyph does nothing without it.

## How it is built

Files in `crates/alpheus-daemon/src/shield/`, and the parts of other files that carry it:

- `mod.rs`: the pure model. `Shield` (the state, `observe` and `flag` for the flag's readings and
  edges, the attempt of a switch with its `deadline` and `expired`, the action slot and the read
  slot), `Job`, `RunOutcome`, `ShieldMail`, `Finish` and `Reason` (what a job is and how it ends),
  `notice`, `status_text`, `plan_verb` (what each verb asks for) and `read_flag`. The tests are in
  `tests.rs`.
- `usb.rs`: USBGuard's `list-devices` text. `parse_blocked` reads the lines `<id>: block id <vid:pid>
  …` with a quote-aware tokenizer, so the name is the value of the `name` keyword and never text found
  inside another attribute's quoted value; the name is cleaned, cut to 64 characters, `?` when empty;
  at most 64 devices are kept. `allow_argv` puts the id into the one element equal to `{id}`.
- `run.rs`: one job on a short-lived thread named `alpheus-shield`. `run_job` runs a command through
  `Executor::run_silent` (waited for, its output `/dev/null`) and a listing through
  `Executor::capture` (2 s, 64 KiB, 1,024 lines); `spawn_job` mails the result back as
  `Mail::Shield`, and a worker that panics still reports.
- `app.rs`: the wiring on the daemon's state. `ShieldApp` (the model, the configuration, the timer, who
  waits for the read) and the methods of `App` that carry out what the model answers.
- Elsewhere: `watch.rs` arms the flag's directory in the theme watch's inotify instance;
  `quick/shield.rs` is the row and the detail; `startup::resolve_shield` turns a partial table or a
  flag in the configuration's directory into a warning and no Shield; `notify/app.rs`'s
  `shield_notice` hands a notice to the server only while it serves; `hooks/app.rs`'s `hooks_shield`
  feeds the hook.

**One timer.** `ShieldApp` has one `DeadlineTimer`, armed only while a switch waits for its flag: 5 s
after a command exits 0, else 60 s from its start. A command is never killed at 60 s; the deadline only
judges the switch.

**One slot each.** One command at a time across the row, the detail, the verbs and the picker, held
from the start until the worker reports, even past 60 s; one read at a time, which a second asker
joins.

**The path of a switch.** Space on the row (or a verb) reaches `shield_switch`. The model's
`begin_switch` takes the action slot and records the attempt; `shield_spawn` starts a worker that runs
the command and mails its outcome back. The loop hands the mail to `shield_mail`. Meanwhile the flag's
directory reports the change to `shield_readable`, which reads the flag once and asks the model's
`flag` for the edge and the attempt it ends: the switch has succeeded when the flag reaches its target,
before or after the exit. A non-zero exit fails at once (`exit <n>`, `killed`, `program not found`,
`could not start`), after one look at the flag; an exit 0 waits 5 s for the flag
(`no change after the command`); 60 s with the command still running is `no change in 60 s`. An edge is
logged (`shield: now on` or `off`), announced, handed to the hook and published to the bar and the
panel. The timer's tick reads the flag once more before judging the deadline, watched or not.

**The first reading** after start is a seed: no notice, no hook. A change is a known state turning into
the other known one; unknown in between changes nothing.

**Without a watch.** If the flag's directory cannot be watched (inotify unavailable, an unwatchable
directory, or a descriptor shared with the theme's), the flag is read at start, after each command
alpheus runs and at each panel open, never on a timer.

## Rules it keeps

- `the_shield_logs_name_nothing`: every log line of Shield mode is a fixed word from a closed list,
  `<reason>` being `Reason::text`; no device name, id, `vid:pid`, command, argument or path.
- `the_shield_files_have_no_panicking_path`: the model, the parser, the runner and the panel's detail
  never panic, because a USB name is hostile text.
- `the_shield_watch_is_the_theme_watchs_instance`: the Shield's files create no inotify instance;
  `watch.rs` has one and carries both targets.
- `only_the_runners_capture_and_never_through_a_shell`: only the launcher's source runner and
  `shield/run.rs` call `capture` or `run_silent`, and neither builds a shell command.
- `a_shield_action_is_never_a_capture_and_shutdown_stops_the_commands`: an action never goes through a
  pipe that could be cut; `shield_mail` and the timer's tick read the flag before judging a failed
  switch or the deadline; shutdown stops the commands it can still reach.
- `the_hooks_are_fed_where_state_changes`: `shield_readable` calls the hook.
- `the_quick_panels_timer_follows_the_view_from_the_open`: a `failed` shown at open clears at its 4 s.
- The thread and timer inventory in `guardrails.rs` counts the worker's `thread::Builder` and the one
  `DeadlineTimer`.
- The headless tests (all begin `shield_`: `shield_space_switches_on_and_the_glyph_shows`,
  `shield_a_change_made_elsewhere_shows_at_once`, `shield_a_refused_password_fails`,
  `shield_the_detail_lists_and_allows`, `shield_status_and_allow_verbs`,
  `shield_a_picker_chosen_after_the_mode_went_off_allows_nothing`, `shield_unconfigured_is_silent`,
  `shield_a_refused_verb_is_logged`, `shield_a_notice_is_given_only_while_serving`,
  `shield_at_rest_no_wakeups`, `shield_the_hook_fires_on_a_change_past_the_quiet_window`) run a stub
  script on a private `PATH`; no test runs a real `pkexec`, `usbguard` or `systemctl`.
- The reasons are in [decisions.md](../decisions.md): "An unknown Shield state is never read as off",
  "A Shield switch succeeds when the flag reaches its target", "A running Shield command is never
  killed at its deadline", "Shield commands and reads run on a short-lived thread", "A Shield verb for
  the state already held runs nothing", "The picker line starts with the checked vendor and product
  ids", "The blocked USB devices are read when asked, never watched", "The Shield flag may not share a
  directory with alpheus's configuration" and "The Shield flag is watched in the theme watch's inotify
  instance".

## What it costs

Measured in release, back to back, 3 runs (the README's memory table has the figures and the method):

- **Linked in, nothing configured:** within noise of the build before it at start (binary +48 KiB
  stripped); no watch descriptor, no segment, no row, nothing allocated.
- **Configured, at rest:** `Anonymous:` +0 to +8 KB with the mode off and +28 KB on
  (`shield_pss_at_rest_unconfigured_and_configured`); no thread and no timer.
- **The detail open on 10 blocked devices:** about +0.6 MB PSS over rest; once closed, `Anonymous:`
  +64 to +68 KB over rest, and ten more cycles +64 to +68 KB (`shield_pss_detail_open_and_closed`).
- **At rest:** 0 loop and 0 whole-daemon wake-ups with the mode off, on and after a detail cycle
  (`at_rest_no_wakeups_with_shield`), and 6 threads before and after a switch and a listing
  (`shield_threads_back_to_baseline`).
