# Clipboard history

A history of what you copied, kept in memory only and shown again in the launcher's box. Off unless
you turn it on. This page says what it does, how to use it, how to configure it, how it is built,
which rules the tests enforce on it, and what it costs. It describes the code as it is today.

## What it does

With `[clipboard] enabled = true` the daemon follows the Wayland clipboard through
`ext-data-control-v1`, or `zwlr_data_control_manager_v1` version 1 where that is all the compositor
has, and keeps the last copies as a list.

- **Text only.** Images are never read, and the primary selection (the mouse selection) is never
  read. Every copy is read once, and the table below says when one is skipped instead.
- **In memory only.** The last 50 copies are kept, newest first; copying a text that is already
  there moves it to the top. Nothing is written to disk, so the history goes when the daemon stops.
- **Pins.** At most 10 entries can be pinned. A pinned entry is kept first and is never evicted, and
  `Clear history` keeps the pins.
- **A row** shows the entry's first non-blank line, folded to one line, with `+N lines` dim when
  there are more, and a glyph in front of a pinned one. The whole text is never drawn.

A copy is skipped, and nothing of it is kept, when:

| The copy | What happens |
| --- | --- |
| carries a password-manager hint (`x-kde-passwordManagerHint`, or macOS's and Windows' concealed types) | the text is never even received |
| is the daemon's own re-copy (its source adds `application/x-alpheus-clipboard`) | not read back into the history |
| arrives while an alpheus `--dmenu --password` prompt is open, or in the 5 s after it closes | skipped unread |
| offers no text type, or its text is over 4,096 bytes, not UTF-8, or only whitespace | skipped |
| takes more than 1 second to read | the read is dropped |

**What is never stored, and what is.** KeePassXC and `wl-copy --sensitive` (so `pass`) set the hint,
so their copies never reach the history; the Bitwarden desktop app sets it too, reported but not
verified here. A copy
**without** a hint **is stored**, in memory, with its text: a password manager that sets none puts
its passwords in the history. Not verified: 1Password for Linux, the Bitwarden browser extension
(browsers refuse custom types), Firefox's and Chromium's own password-field copies, gopass. A tool
that prompts for a password and copies it without the hint more than 5 s after an alpheus password
prompt closed, such as a slow `gpg` or `pinentry` decrypt, is not covered either. `Delete` on the
entry, or `alpheus-launch --clipboard-clear`, forgets it; the log lines and the debug output never
carry any clipboard text.

## Using it

**Opening it.** `alpheus-launch --clipboard`, bound by you (Mango
`bind=SUPER,v,spawn,alpheus-launch --clipboard`, Sway `bindsym $mod+v exec alpheus-launch --clipboard`),
or `"` typed in the launcher's All mode. Any other open launcher is replaced, and a `--clipboard`
less than 250 ms after the one that opened it is ignored.

**The keys.**

| Key or click | What it does |
| --- | --- |
| Enter | copies the entry back and closes the box |
| Ctrl+Enter, or a right click | pins or unpins the entry |
| Delete | forgets the entry |
| Enter on the last row, `Clear history` | asks `Clear? ⏎`; a second Enter forgets everything but the pins |

The question is answered only from 0.4 s after it was asked, so a held Enter never confirms it, and
any other key or a click cancels it. `alpheus-launch --clipboard-clear` does the same from a script,
without touching the current clipboard.

**After Enter** the daemon itself owns the clipboard, so the clipboard empties if the daemon stops,
and no `wl-copy` is left running.

**Exit codes.** Both verbs exit 2 when the history is off and when the compositor offers no
data-control protocol, and 3 when the bar is not running or is older than the verb (the one code on
which a script falls back).

## Configuration

`[clipboard]` in `bar.toml`, read once at start, so a change takes effect when `alpheus-demon`
restarts.

| Key (under `[clipboard]`) | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | `true`: follow the clipboard and keep the history described above |

```toml
[clipboard]
enabled = true
```

With the history off, no protocol object is bound and nothing is kept. The launcher's `"` is then
plain text, not a prefix ([the launcher page](launcher.md) lists the prefixes).

## How it is built

The files in `crates/alpheus-daemon/src/clipboard/`, split the way alpheus splits most features: the
pure parts, then the Wayland side (`mod.rs` only declares them).

- `history.rs`: the history itself, pure. `Entry`, `push` (what it added or moved), the pins and
  `ClipRow`, the one-line preview a launcher row shows. `Debug` prints ids and flags only, and every
  text is zeroed before it is freed.
- `offer.rs`: what to do with an offer, pure. `TEXT_TYPES` (the read preference), `SENSITIVE` (the
  password-manager hints), `MARKER` (the daemon's own source), the verdict (`Read` or a `Skip`), the
  1 s read deadline, the 5 s password quiet window and the bounded read buffer.
- `control.rs`: the only file that names a data-control protocol, both versions behind one macro: the
  manager, the device, the offers and the one pending read on the event loop. It never logs, and
  nothing in it shows an offer's types or text.
- `app.rs`: the wiring on `App` (`ClipboardApp` and the `clipboard_*` methods): the selection that
  arrives, the read and its deadline, the verbs, the re-copy's own source, and the history's edits
  from the launcher.
- The tests sit with each file: `history/tests.rs` and `offer/tests.rs` beside the pure parts, a
  `tests` module at the end of `control.rs` and of `app.rs`.

**One timer.** The read deadline, armed only while a read is pending and disarmed with it, so nothing
wakes the loop while the clipboard is quiet. There is no thread.

**The path of one copy.** The compositor announces a new selection; `clipboard_selection_inner` puts
the offer in a guard that destroys it whatever happens, asks `offer::verdict` what the offered types
mean, and either drops it or starts one bounded read on the loop. `clipboard_read_wake` fills the
buffer, `clipboard_read_done` hands the text to `History::push` and syncs the deadline. A read that is
still pending when another selection arrives is dropped first.

**The path of a re-copy.** Enter on a row asks `clipboard_copy`, which takes the entry's text and
makes the daemon the clipboard's owner through `own_selection`, offering the text types and then the
marker, so the compositor's announcement of that very copy is skipped instead of being stored again.

**The device's lifetime.** When the compositor finishes the data device, the history attaches to a
new one once; if the seat is removed it attaches to another seat.

## Rules it keeps

These are enforced by tests, so a change that breaks one fails the build.

- `clipboard_no_panic_no_log_files_have_no_panicking_path_and_log_nothing` and
  `clipboard_app_never_panics_and_logs_fixed_words` (`tests/guardrails.rs`): the pure files and
  `control.rs` never panic, never slice a string by range and never log, because a boundary panic's
  message would embed the text; `app.rs` logs fixed words only, never a text, a length, a type or a
  count (`clipboard_logs_only_fixed_words`).
- `nothing_under_clipboard_prints`: no print or debug macro anywhere in the folder, so a clipboard
  text reaches no stream but the log's own words.
- `the_clipboard_sensitive_hint_type_is_never_received`: the hint types are named in `offer.rs`'s
  `SENSITIVE` and nowhere else, so no code path can ask such an offer for its data.
- `the_clipboard_selection_is_set_only_by_the_own_source`: the daemon sets a selection in one place
  only, for a re-copy; never `None` (it would clear your clipboard) and never the primary selection.
- `the_clipboard_offer_is_wrapped_in_destroy_on_drop`: an offer is in its guard before anything can
  panic, so a panic between taking it and reading it still destroys it.
- `data_control_only_in_control_rs`: every data-control token is named in `control.rs` only.
- `every_clipboard_read_start_and_end_syncs_the_timer`: every function that starts or ends a read
  arms or disarms the deadline with it.
- `the_clipboard_shutdown_wipes_pins_too`: shutdown wipes the whole history, pins included.
- `a_clipboard_row_spawns_nothing_from_the_launcher`: no key or click on a history row reaches an
  executor.
- The headless tests run the real daemon on an isolated compositor:
  `clipboard_start_line_names_the_protocol`, `clipboard_enter_copies_back_without_a_new_entry`,
  `clipboard_skips_sensitive_copies`, `clipboard_skips_copies_around_a_password_prompt`,
  `clipboard_skips_a_text_over_4_kib`, `clipboard_clear_keeps_pins`,
  `clipboard_quote_prefix_in_the_all_launcher`, `clipboard_verbs_when_off_exit_2` and
  `clipboard_replaces_another_launcher_mode`.

## What it costs

Measured in release, back to back with the version before it, 3 runs each (the
[memory tables](../measurements.md) have the figures and the method).

- **Linked in, off** (no `[clipboard] enabled`): −256…+80 KB at start; the binary grew by 140 KiB
  stripped. Off, no protocol object is bound and nothing is allocated.
- **On, nothing copied:** +160, +64, −24 KB over the same bar without it
  (`startup_pss_per_source`, bound 256 KB): the data-control manager and device, no timer, no thread.
- **A full history** (50 × 4,000 bytes): +164…+188 KB of anonymous memory over rest; 100 more copies
  +4 KB; cleared, back to about rest (`clipboard_full_history`). That is the history, its previews
  and the one read buffer, all wiped when removed.
- **The box open on 50 entries:** +1.0–1.14 MB PSS over closed; after 2 warm-up and 4 more cycles
  −112…+12 KB of anonymous memory (`clipboard_launcher_open_and_closed`).
- **At rest:** 0 loop wake-ups in 5 s after copies and after a `--clipboard` cycle
  (`at_rest_no_wakeups_after_copies`), since the 1 s read deadline is armed only while a read is
  pending.
