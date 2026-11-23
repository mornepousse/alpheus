# The quick notepad

One pad of plain text, opened by a key bind of your own. This page says what it does, how to use it,
how to configure it, how it is built, which rules the tests enforce on it, and what it costs. It
describes the code as it is today.

## What it does

`alpheus-launch --notes` toggles one pad, drawn as the launcher's box (60 columns and 14 text rows,
down to 24 by 3 on a small output) on the focused output. It holds one note, in
`$XDG_STATE_HOME/alpheus/notes.txt`: at most 64 KiB, owner only, written atomically.

- The file is read when the pad opens and written after a pause of two seconds in the typing, at
  every close, and when the daemon stops.
- Nothing of the pad is resident while it is closed: no surface, no keyboard, no timer, no text. The
  one exception is the text of a save that failed, kept for the next open (see the paragraph on a
  save that fails, below).
- The title row says `saved` or `not saved`, and the help row reads
  `esc close · ctrl+c copy · ctrl+v paste`, or the last message.

**A file the pad cannot safely take over opens read only** and is never overwritten: moves, the
selection and copy work, edits and paste are refused, and the reason is on the help row.

| The file | What the pad shows |
| --- | --- |
| over 64 KiB, or not UTF-8 | its first 64 KiB, the invalid bytes as `�` |
| holding tabs, carriage returns or control characters (edited elsewhere) | the text, cleaned |
| not ours (a symlink, another owner, a directory open to others), or unreadable | nothing |

**A save that fails** at close keeps the text in memory for the next open, which then shows
`not saved`, as long as the file is still as the pad last read or saved it (device, inode,
modification time and size). If the file changed meanwhile — written elsewhere, replaced, removed,
now refused — the file wins: it opens as usual, the kept text is dropped unwritten, and the help
row says `earlier unsaved text not written: the file changed` (the log says
`notes: not saved (<kind>)`). That text, up to 64 KiB, is the one thing of the pad resident while
it is closed, and only after such a failure; a save that fails while the bar stops loses it.

## Using it

**Opening and closing.** Bind the verb yourself, for example Mango
`bind=SUPER,n,spawn,alpheus-launch --notes` or Sway `bindsym $mod+n exec alpheus-launch --notes`. The
pad closes on Esc, on the verb again, on a click outside, and when the keyboard focus leaves; every
close saves. A `--notes` less than 250 ms after the one that opened the pad is ignored
(`notes: toggle debounced`), because some compositors fire a key bind twice. The pad and the launcher
share the box: opening one closes the other, and a waiting `--dmenu` is cancelled.

**The keys.**

| Keys | What they do |
| --- | --- |
| characters, Enter, Tab | type; Tab is four spaces |
| Backspace, Delete, Ctrl+Backspace, Ctrl+Delete | erase a character, or a word |
| arrows, Ctrl+arrows, Home, End, Ctrl+Home, Ctrl+End, Page Up, Page Down | move the caret, by character, word, line or page |
| Shift with a move, Ctrl+A | select |
| Ctrl+C, Ctrl+X | copy or cut the selection, through `copy` of `launcher.toml` |
| Ctrl+V | paste the Wayland clipboard |
| Esc | close |

A held key repeats moves and edits, never a copy, a cut, a paste or the close: each repeat would
start another copy program or reopen the clipboard. A click places the caret and the wheel scrolls;
there is no selection with the mouse.

**Known limits.**

- One pad: no several notes, no colours, no pinning.
- No undo.
- Copy and cut are capped at 4 KiB (one page, which a pipe holds whatever its size), and a cut whose
  copy program fails after it started loses the text, since the program is only known to have
  started.
- Paste works with every `wl_data_device` version, but a compositor offering only version 1 cannot
  release the device, so one protocol object per open stays on the server until the bar exits
  (wlroots compositors, Sway and Mango among them, offer version 3; verified headless on Sway only).
- There is no `fsync`, the rename being atomic.
- Something else writing the file while the pad is open is overwritten by the pad's next save.

## Configuration

The pad has no configuration key of its own. It uses `copy` from `launcher.toml` (default
`["wl-copy"]`) for Ctrl+C and Ctrl+X, and the launcher's box, so the theme and the font are the
bar's.

## How it is built

The files in `crates/alpheus-daemon/src/notes/`, the pure parts first, then the glue, then the
Wayland side (`mod.rs` only declares them):

- `text.rs`: the buffer. A `String` of at most `CAP` (65,536) bytes, a caret and an optional anchor,
  both byte offsets on scalar boundaries and never inside a glued mark. It slices only through
  `str::get`, so it never panics.
- `lines.rs`: the soft wrap. The visual rows over the text, and the maps between a byte offset and a
  (row, column) cell.
- `keys.rs`: the keys the pad understands, from what xkbcommon already resolved (a keysym with the
  layout applied, and its text). The held-key repeat is the caller's business.
- `model.rs`: the buffer, the scroll, the goal column of vertical moves, the dirty and save-failure
  flags and the one-line message. The glue does the I/O and tells the model what happened.
- `view.rs`: the pad drawn as rows of cells — title and status, the wrapped text with its caret and
  selection, the help row — in the painter's frame, as the launcher's box.
- `store.rs`: the file. Read once when the pad opens, written after the pause in typing, and only
  through `state_file` (atomic, private, never a followed link). Pure and silent: it never logs,
  because the note is private.
- `paste.rs`: Ctrl+V, through the data device the pad creates when it opens. The pipe is a source of
  the event loop only while a paste is pending, bounded by `CAP`, so a clipboard owner that streams
  for ever is cut at the first byte past it. The pad never owns a selection and accepts no drop.
- `app.rs`: the pad as one feature of the `App`: the toggle decided purely (`decide`), the guard that
  saves what was typed and closes the pad on an error of its own, the outcomes of its keys, the
  autosave timer and the copy command.
- `surface.rs`: the Wayland side, shaped like the launcher's. One centred `Overlay` layer surface
  with exclusive keyboard focus, one transparent scrim per output on the same layer to catch a click
  outside, the `App`'s keyboard, and buffers from a pool of its own. Everything lives in `OpenNotes`,
  dropped whole on close.

**One timer**, the autosave's, armed only while the pad is open and edited. There is no thread, and
the paste pipe is on the loop only while a paste is pending.

**The path of a key.** The keyboard handler gives the key to `notes_guarded`, which runs it inside the
pad's guard; `keys.rs` turns it into a `NoteKey`, the model applies it to the buffer, and the view is
repainted. An edit arms the autosave timer; its callback writes the file through `store.rs` and sets
`saved` or `not saved` on the title row.

**The path of a paste.** Ctrl+V asks the data device for the offer's text (`text/plain;charset=utf-8`,
`text/plain` or `UTF8_STRING`); the pipe is added to the loop, read in chunks up to `CAP`, and the
text is spliced at the caret.

**A panic in the pad** saves its text and closes the pad, never the bar. The log names the place,
never the text: the daemon's panic hook never prints a panic's message, and neither the pad's guard
nor the launcher's logs it (`notes error in <what>`, `launcher error in <what>`).

## Rules it keeps

These are enforced by tests, so a change that breaks one fails the build.

- `the_notes_pure_parts_have_no_panicking_path_and_log_nothing` (`tests/guardrails.rs`): the six pure
  files never panic, never slice a string by range (a boundary panic's message would embed the text)
  and never log, the note being private.
- `the_notes_glue_has_no_panicking_path` and `the_notes_glue_never_logs_the_text`: the glue does not
  panic on its own either, and no log line of `app.rs`, `surface.rs` or `paste.rs` names the pad's
  text, its selection or its buffer.
- `notes_files_are_written_only_through_state_file`: the note reaches the disk only through
  `state_file`, and only `store.rs` calls its writers.
- `the_notes_spawn_only_the_copy_command`: the pad runs one command, the copy program, with the text
  on its standard input; nothing else under `notes/` is started.
- `the_notes_timer_and_pipe_live_only_while_open` and `the_paste_pipe_lives_only_while_the_pad_is_open`:
  `notes_close` takes the pad out before it syncs the timer and releases the keyboard, and the paste
  pipe is a source of the loop only inside `paste.rs`.
- `shutting_down_closes_and_saves_the_pad`: a daemon stopping saves an edit the autosave has not
  written yet.
- `seat_and_keyboard_handlers_guard_their_launcher_calls` and
  `a_bar_press_swallows_only_a_launcher_it_closed`: the keyboard and seat handlers go through the
  guard, and a press on the bar while the pad is open closes it without acting twice.
- `the_panic_hook_is_installed_before_anything_else`: the hook that names the thread and the place,
  never the message.
- The headless tests run the real daemon on an isolated compositor:
  `notes_type_close_and_find_the_file`, `notes_autosaves_while_open`,
  `notes_toggle_is_debounced_and_a_click_outside_closes`, `notes_replaces_the_launcher_and_back`,
  `notes_a_failed_close_save_is_kept_for_the_next_open`,
  `notes_a_kept_text_never_overwrites_a_newer_file`,
  `notes_a_kept_text_never_overwrites_a_newer_refused_file`,
  `notes_a_kept_text_after_an_own_save_is_still_kept`,
  `notes_sigterm_saves_an_edit_younger_than_the_autosave`, `notes_paste_from_the_selection`,
  `notes_paste_ignores_a_selection_without_text` and `a_refused_notes_file_opens_read_only`.

## What it costs

Measured in release, back to back with the version before it, 3 runs each (the
[memory tables](../measurements.md) have the figures and the method).

- **Linked in:** −361…−44 KB at start, which is the harness's spread: one base run read about
  0.6–0.8 MB high on every row, so every case of that pairing came out negative and the linked-in
  code is not visible. The binary grew from 6.71 to 6.84 MiB stripped (+128 KiB). The
  `wl_data_device_manager` is bound once at start (one protocol object); the data device itself
  exists only while the pad is open.
- **Open with 2,000 characters typed:** +794…+985 KB PSS over closed
  (`notes_pss_open_and_closed`): the pool, the text and the data device exist only while it shows.
  The first open also warms code and font pages once (+0.3–1.5 MB of file-backed PSS, as any box
  does), so closed is read after one warm-up cycle.
- **Closed again:** +0…+92 KB of anonymous memory over the warmed closed figure; anonymous memory
  1.40–1.41 MB cold, 1.48–1.50 MB warmed.
- **At rest:** 0 loop wake-ups in 5 s after a close, and 0 in 5 s with the pad open and saved
  (`at_rest_no_wakeups_after_notes`); a save timer left re-arming every second makes the second
  window read 5.
