# The launcher

A centred box for apps, open windows, `$PATH` commands, a calculator, emoji, a web search and the
lines of your own menu scripts, opened and closed by the daemon itself. This page says what it does,
how to use it, how to configure it, how it is built, which rules the tests enforce on it, and what it
costs. It describes the code as it is today.

## What it does

The box has four modes: **All**, **Apps**, **Windows** and **Commands** (the executables on `$PATH`).
Typing ranks the results with [nucleo](https://github.com/helix-editor/nucleo) (Helix's fuzzy
matcher) and then by frecency, which also orders the empty query, most-used first. Nothing of the
launcher stays resident while it is closed, not even a thread.

![The alpheus launcher, All mode, sample data (fictional apps and windows), default theme (catppuccin, dark), zoomed ×2](../images/launcher.png)

### The calculator

A query that looks like arithmetic shows one row above the other results, or alone with
`alpheus-launch --calc`. It takes `+ - * / % ^`, parentheses, `x` as a multiplication, `0x`/`0b`/`0o`
and `_` in numbers, `pi` and `e`, and `sqrt`, `cbrt`, `abs`, `ln`, `log`, `log2`, `exp`, `sin`, `cos`,
`tan`, `asin`, `acos`, `atan`, `round`, `floor`, `ceil`, `min`, `max`.

Integers are exact (128 bits) and fall back to floating point on overflow; results keep 12
significant digits. `%` is the truncated remainder, as in C and Rust: `-7 % 3` is `-1`. A result
computed from hexadecimal, binary or octal input also shows `0x…` and `0b…`.

Units convert with `<value> <unit> to <unit>` (or `in`): length (`mm cm m km inch ft yd mi`), mass
(`mg g kg t oz lb`), temperature (`c f k`), data (`b kb mb gb tb pb`, `kib … pib`) and time
(`ms s min h d`). Inches are typed `inch` (or `inches`): `in` is the keyword, so `12 in to cm` gives
no row. A word such as `e`, `pi`, `2048`, `7zip` or `1password` never shows the row.

`Enter` copies the answer (`3.10685596119 mi`), `Ctrl+Enter` copies `5 km to mi = 3.10685596119 mi`,
through `copy` in `launcher.toml` (`[]` hides the action); the copy is not recorded in the history.
Nothing is resident, and nothing runs per keystroke beyond the parse.

### The prefixes

A query that starts with `?`, `:`, `$` or `"` in the All mode lists only that prefix's rows. They are
never prefixes under `--password`, in the Apps, Windows and Commands modes, or in script and dmenu
menus, where they are plain text. A prefix alone shows a hint line, not a row.

- `? rust ratatui` shows `Search the web: rust ratatui`; Enter opens `search` (default
  `https://duckduckgo.com/?q={query}`, the text percent-encoded as one parameter) through `open`
  (default `["xdg-open"]`, the URL as the last argument), in a systemd scope like an app, so the
  browser never dies with the bar. `search = ""` or `open = []` turns the prefix off, and a `?` query
  is then searched as text. It is never recorded in the history.
- `:heart` lists the emoji whose name matches (Unicode 16.0, the 1,906 fully-qualified ones without
  skin tones, a table in the binary's read-only data, paged in only when used; the launcher's fuzzy
  matcher over the name, at most `rows`). Enter copies the emoji through `copy`, `Ctrl+Enter` copies
  its name. Sequences (a family, a flag, a keycap) show as their pieces but are copied whole. Colour
  needs an emoji font in `[font] fallback` of `bar.toml` (for example
  `["DejaVu Sans Mono", "Noto Color Emoji"]`, the key replacing the default): the default monospace
  font has few. `:!x` is the matcher's negation, and an accented needle matches literally.
- `"` lists the clipboard history (see [Clipboard history](clipboard.md)); only while
  `[clipboard] enabled = true`, otherwise plain text.
- `$ sleep 5 && notify-send hi` shows `Run in a shell: …`; Enter runs `sh -c -- <text>` detached in a
  scope, `Ctrl+Enter` in `terminal_hold`. The text is never recorded or logged.

`:` and `$` are always claimed as prefixes: a query that starts with one never ranks apps (set
`[prefixes]` to something else if you search names that start with them; a configured mode prefix is
taken first). The attribution of the emoji names is in `NOTICE`.

### The window switcher

`alpheus-launch --windows` opens the windows with the previous one selected, so Enter goes back to
the last window. A second `--windows` while it is open, and `Tab`, step down (`Shift+Tab` up),
wrapping, so holding the modifier and pressing the key again walks the list; only a `--windows`
within 250 ms of the switcher's own open is ignored, because some compositors fire a key bind twice
(Hyprland re-fires a press bind on release). Esc closes. There is no modes row, no thumbnail, no scan
of apps or `$PATH`, and nothing resident.

### Command sources

A menu script's lines can be searched next to the apps. A `[[sources]]` table in `launcher.toml`
names a `list` command and a `run` command. `list` runs once per open, on the thread that already
scans the applications, after them: 1 s at most, 2,000 lines, 256 KiB. `run` receives the chosen line
as one argument, never through a shell; it is the cleaned line (controls and invisible formatting
characters removed, U+2028 and U+2029 turned into a space), not the script's raw bytes.

A line `Captures · Screenshot zone` shows as **Screenshot zone**, then `Captures` dimmed. It is found
by typing either part. It is never listed on an empty query, in another mode or behind a prefix. Its
use is remembered for the ranking like an app's.

A source that fails, hangs or floods is cut off and logged once, and the launcher opens as usual.
Whatever the list command started is killed with it when the read ends. If the daemon itself is
killed mid-read, the list command dies too, but processes it left in the background do not; a systemd
unit with `KillMode=control-group` (the default) cleans them up.

## Using it

**The keys.** `Tab` and `Shift+Tab` switch modes; typing filters. `Enter` runs the selection — a
command or typed shell text in a held terminal, which stays open so you can read the output — and
`Ctrl+Enter` runs it detached. An app starts in its own systemd scope when a user manager exists, so
it never dies with the bar.

**The verbs.** `alpheus-launch` bridges to the daemon over an owner-only socket: `--toggle` (the
default) and `--close`, `--calc`, `--windows`, `--clipboard`, `--notes` and `--dmenu`. Every option
and exit code is in [`docs/cli.md`](../cli.md); exit code 3 (the daemon is not running, or is older
than the verb) is the one case where a script falls back to another menu.

**Binding it.** alpheus sets no key bind. For the switcher, for example,
`bind=ALT,Tab,spawn,alpheus-launch --windows` in Mango, or
`bindsym Mod1+Tab exec alpheus-launch --windows` in Sway.

**The dmenu mode.** `alpheus-launch --dmenu` reads lines on standard input, shows them in the box and
prints the chosen one, with fuzzel's dmenu options that scripts use:

| Option | What it does |
|---|---|
| `-p`, `--prompt TEXT` | text shown before the input |
| `--index` | print the chosen line's 0-based position on standard output instead of its text, empty lines counted as fuzzel does (`-1`, exit 0, for typed text that matches no line, as fuzzel) |
| `--password` | draw one `•` per typed character, show no list (standard input is read and ignored), print the text as typed; never logged nor kept in the history, wiped from memory on close; not with `--index` |
| `--visible N` | with an empty query, list only the first N non-empty lines; typing searches every line (a script's home menu: its categories first, every entry by typing); not with `--password` |
| `--placeholder TEXT`, `--width N`, `--lines N` | accepted and ignored (the box keeps its own size) |

A running `alpheus-demon` older than `--index`, `--password` or `--visible` (upgraded but not
restarted) refuses them: `alpheus-launch` then exits 3, like an unreachable daemon, so a script falls
back to another menu with the same options.

## Configuration

`launcher.toml` in alpheus's configuration directory, read at each open of the launcher, so a change
needs no restart and nothing of it is kept while the box is closed. Absent: the defaults, with no
message. Invalid: the defaults and one line in the daemon's log, never an abort.

Every key is in [`docs/configuration.md`](../configuration.md#launchertoml): the terminals
(`terminal`, `terminal_hold`), `rows`, `history`, `focus`, `copy`, `search` and `open` at the top
level, the mode prefixes in [`[prefixes]`](../configuration.md#prefixes), and the command sources in
[`[[sources]]`](../configuration.md#sources) (at most 8). The bar's own `[font]` and the theme decide
how the box looks.

The `launcher` module (the icon that opens the box, the first module of the default `modules`) is
configured in `bar.toml`; the box itself works without it, through the verbs.

## How it is built

The files are in `crates/alpheus-daemon/src/launcher/`, the pure parts first, the Wayland side last.
The box is one more pair of layer-shell surfaces of the daemon, built at open and freed at close.

- `config.rs`: `launcher.toml`, read at each open.
- `history.rs`: the frecency history — which entries were launched, how often, how recently. It holds
  entry ids (`app:<desktop id>`, `cmd:<name>`, `source:<name>:<line>`) and numbers only, never typed
  text, in `$XDG_STATE_HOME/alpheus/launcher-history`.
- `desktop.rs`: the parts of the XDG Desktop Entry specification the launcher uses (the
  `[Desktop Entry]` group and its actions, the value escapes, the localized keys, `Hidden` and
  `NoDisplay`).
- `scan.rs`: the catalog, rebuilt at each open on a short-lived thread: the `.desktop` files of the
  XDG data directories (no cache, since a NixOS rebuild changes every path) and the executables on
  `$PATH`, bounded everywhere (files, depth, counts). Nothing of it outlives the delivery.
- `sources.rs`: the command sources, read on that same thread through `Executor::capture`.
- `matching.rs`: the matching and the ranking. `nucleo-matcher` (fzf scoring, smart case, Unicode
  folding), then frecency from the history; the `Ranker` owns the matcher's scratch memory, built
  when the launcher opens.
- `calc.rs`: the calculator — a hand-written lexer, a recursive-descent parser with a depth counter,
  and evaluation over exact `i128` values that fall back to `f64`. No state, no I/O.
- `prefix.rs` and `emoji.rs`: the prefixes of the All mode, and the emoji table (one `&'static str`
  of `<emoji>\t<name>\n` lines generated once from Unicode's `emoji-test.txt`, so it costs one
  read-only section and no allocation).
- `model.rs`: the state and every key rule — the query, the mode and the prefixes that jump to one,
  the selection, the visible window of results, and what Enter and Ctrl+Enter activate. The Wayland
  side feeds it keys and clicks and applies the `Outcome`.
- `view.rs`: the box drawn as rows of cells (the modes row, the input line, the results, the help
  line, and a preview panel on the right when there is something to show).
- `run.rs`: the only place the launcher builds a command line. Apps start in their own systemd scope
  when a user manager exists, fully detached otherwise, and `Exec` is expanded per the Desktop Entry
  specification.
- `socket.rs` and `socket_glue.rs`: the daemon's side of the launcher socket, owner-only (mode 0600
  and the peer's uid checked), read without ever blocking, every request bounded, with the listener
  and each connection on the event loop and a deadline per request. The socket's path is only ever
  built by `launcher_ipc::socket_path`, so it cannot drift between the daemon and `alpheus-launch`.
- `host.rs`: the launcher as one feature of the `App` — the socket requests decided purely
  (`decide`), the bar button and the toggle, the scan's catalog delivered by generation, the
  compositor's windows pushed into an open box, and a dmenu client's hang-up. Every entry point sits
  behind `guarded`, so an error of the launcher's closes the box, never the bar.
- `surface.rs`: the Wayland side, kept thin. One centred `Overlay` layer surface with exclusive
  keyboard focus, one transparent 1×1 scrim per output scaled by `wp_viewporter` to catch a click
  outside, the `App`'s keyboard (shared with the popups), and buffers from a pool of its own.

**The path of one open.** A verb arrives on the socket; `socket_glue.rs` hands the request to
`host.rs`, which decides what it means and asks `surface.rs` for the box, while the scan thread
builds the catalog (the apps, then the sources) and delivers it by generation, so a late answer for
an earlier open is dropped. Keys reach `model.rs`, whose `Outcome` the Wayland side applies; an
activation goes to `run.rs` and then to the executor. The close drops the surfaces, the keyboard, the
ranker and the catalog.

## Rules it keeps

These are enforced by tests, so a change that breaks one fails the build.

- `nothing_resident_while_the_launcher_is_closed` (`tests/guardrails.rs`): every place in the daemon
  that can start a thread, arm a timer, watch a descriptor or bind a keyboard is in one table with
  the reason it does not run at rest; a new one fails the test until it is added and justified.
- `every_launcher_activation_stays_on_the_allowlist`: every activation the launcher can produce — of
  every mode, with Enter and Ctrl+Enter, typed shell text included, under both spawners — is one of
  the allowed shapes: an app's argv alone or in its systemd scope, after the terminal for
  `Terminal=true`; a `$PATH` command alone (detached) or held in the terminal; `sh -c` from the
  Commands mode only; the focus template, never wrapped; a command-source entry as its source's `run`
  with the whole line as one argument, never through a shell, from the All mode only. Each shape is
  reached by the test.
- `the_launcher_socket_is_bound_only_in_socket_rs`: the socket is bound in exactly one place, and its
  file name is only ever built by `launcher_ipc::socket_path`.
- `the_launcher_prefixes_have_no_panicking_path_and_log_nothing`: the prefix parser runs on each
  keystroke, so it never panics, and the query text it sees is never logged.
- `the_launcher_guard_never_logs_a_panic_payload`: the guard logs the place only, since a panic's
  message can quote the query, a menu line or a password.
- `launcher_fail_takes_the_launcher_then_releases_the_keyboard_then_returns_early`,
  `seat_and_keyboard_handlers_guard_their_launcher_calls`,
  `keyboard_entry_points_go_through_key_event_and_bind_no_launcher_call` and
  `a_bar_press_swallows_only_a_launcher_it_closed`: the guard's order, and the one path by which a
  key or a press reaches the box.
- `a_clipboard_row_spawns_nothing_from_the_launcher`: no key or click on a clipboard row reaches an
  executor.
- The headless tests run the real daemon on an isolated compositor:
  `toggle_opens_and_closes_the_launcher`, `calc_opens_the_calculator_and_enter_copies`,
  `dmenu_round_trip_and_cancel`, `dmenu_password_shows_dots_and_keeps_nothing`,
  `dmenu_index_prints_the_position`, `dmenu_visible_lists_the_first_lines_and_searches_them_all`,
  `dmenu_client_killed_closes_the_launcher`, `output_unplugged_while_the_launcher_is_open`,
  `the_launcher_and_a_menu_are_never_open_together`,
  `thirty_launcher_closes_by_key_then_an_outside_click_keep_the_bar_alive`,
  `under_sway_the_launcher_lists_and_focuses_a_window`,
  `windows_focuses_the_previous_window_then_steps`,
  `a_source_entry_is_found_by_typing_and_runs_its_whole_line`,
  `the_empty_query_shows_no_source_entry` and `a_hung_source_is_killed_and_nothing_stays_awake`.

## What it costs

Measured in release on the headless harness, 300 synthetic apps (the full tables are in
[measurements](../measurements.md); the tests that measure are named here).

- **Linked in, never opened** (no `launcher` module even): +0.3–0.7 MB over the delivery before it,
  the one-time cost of a bigger binary (`nucleo-matcher`, `xkeysym`, `xkbcommon`'s Rust bindings) plus
  `libxkbcommon.so` itself (about 60 KB of it). **Closed:** +0 MB on top, since no thread, timer or
  keyboard exists while it is closed.
- **Open** (a typed query, the keyboard bound): ≈ +2.0–2.1 MB over closed, at a median open latency
  of ≈ 0.8–1.3 ms (max 1.7–2.9 ms over 20 opens, from the request accepted to the first frame
  committed).
- **50 open and close cycles:** ≈ +0.1–0.2 MB over the post-warm-up baseline (measured 148–220 KB
  across runs, well under the 1 MB budget), and 0 wake-ups in 5 s at rest after use (2 in one of
  three runs).
- **The prefixes and the emoji table, linked in:** −91…+106 KB at start, which is the harness's
  spread; the binary grew by 64 KiB stripped, of which the emoji table's text is 43 KiB of read-only
  data, paged in only when `:` is used. Open with `:face` typed, +1.2–1.3 MB over closed, and
  +32…+49 KB once closed again (`launcher_prefixes_pss_open_and_closed`).
- **The calculator:** open with `6*7` typed, +778–790 KB over closed, and 0…+16 KB once closed again
  (`launcher_calc_pss_open_and_closed`); 0 loop wake-ups in 5 s after a `--calc` cycle
  (`at_rest_no_wakeups_after_a_calc_cycle`).
- **Command sources:** linked in with none configured, within noise (−4…+92 KB closed, +0.1–0.2 MB
  open). While the box is open each kept line costs about three times its text (the line, its entry
  and its path): ≈ 6–8 MB at the bounds (8 sources × 256 KiB) plus the ranker, released on close. A
  300-line source costs ≈ 0.5 MB: the box open on one reads ≈ +2.4–2.5 MB over closed (≈ +1.9–2.4 MB
  with no source), and its entries appear ≈ 15–17 ms after the request (median of 21 opens), the box
  itself showing first at ≈ 1.7–2.2 ms against ≈ 1.6–1.9 ms with no source, with 0 wake-ups at rest
  after (`launcher_sources_pss_latency_and_rest`).
- **A 100,000-line dmenu:** ≈ +6.6–6.8 MB open, back to +0.5 MB one second after it is cancelled.
