# Command lines

alpheus ships three programs. This page lists every option and exit code, as
read from their parsers.

## `alpheus` (the control center)

```
alpheus [--tab <name>]
alpheus [--tab=<name>]
alpheus --tab diagnostics --preset <slug>
```

Opens the TUI in the current terminal, on the Home tab unless told otherwise.
`<name>` is one of `home`, `sound`, `network`, `bluetooth`, `power`,
`displays`, `appearance`, `diagnostics` (`theme` is accepted for `appearance`
and `diag` for `diagnostics`; case is ignored). The tabs are also reached with
`1` to `8` once open. `--preset <slug>` (`errors`, `usb`, `suspend`, `oom`, `thermal`, `network`,
`live`, `service`; only with `--tab diagnostics`) opens the Diag tab on that journal view, as the
Tools menu's Diagnostics entries do.

There is no `--help`; the usage appears in the error text.

| Exit code | Meaning |
|---|---|
| 0 | ended with `q`, or stopped cleanly by SIGTERM/SIGHUP |
| 1 | a terminal I/O error |
| 128 + signal | a stop signal the loop did not honour within 0.3 s (e.g. a closed terminal) |
| 2 | an unknown option, a missing `--tab` or `--preset` value, an unknown tab or preset name, or a preset without `--tab diagnostics` (message on standard error) |

Configuration: `~/.config/alpheus/config.toml`, see the
[configuration reference](configuration.md).

## `alpheus-demon` (the bar and everything resident)

```
alpheus-demon
```

Takes **no arguments** (any argument is ignored). Start it once per session
from the compositor's autostart or a systemd user unit; it needs a Wayland
session with layer-shell (`WAYLAND_DISPLAY` set).

- **One instance.** It takes an exclusive `flock` on
  `$XDG_RUNTIME_DIR/alpheus-demon.lock`, held as long as it lives; a second
  daemon logs `alpheus-demon is already running (pid N)` and exits 1. The
  kernel drops the lock if the daemon dies, so nothing stale is left.
- **Socket.** The launcher's socket is
  `$XDG_RUNTIME_DIR/alpheus-launcher.sock` (mode 0600, and the daemon refuses any peer whose uid is not its own), which
  `alpheus-launch` talks to.
- **Signals.** SIGTERM, SIGHUP and SIGINT stop it: the loop ends normally
  (a waiting `--dmenu` gets "cancelled"), and if it does not finish within a
  grace period (1 second; 3 seconds while alpheus records, so the recorder gets SIGINT and up to
  2 s to finish its file) the process exits with 128 + the signal number.
- **Logs.** One line per event on standard error, prefixed `alpheus-demon:`
  (under systemd: `journalctl --user -u <your unit>`). Configuration
  warnings and the detected compositor are logged at start.
- **Configuration** is read once at start for `bar.toml`; restart the daemon
  to apply it. Theme files reload live and `launcher.toml` is read at each
  open (see the [configuration reference](configuration.md)).
- **Exit code.** 0 after a clean stop, and also when the compositor ends
  the session (broken pipe or connection reset on the Wayland socket); 1
  when it cannot start (already running, no Wayland session, unusable fonts
  or signals) or when its event loop fails for any other reason, so that a
  supervisor knows to relaunch it; 128 + signal number when a stop signal
  is not honoured within the grace period.

## `alpheus-launch` (scripting bridge to the daemon)

```
alpheus-launch [--toggle | --close
               | --dmenu [-p|--prompt TEXT] [--index] [--password] [--visible N]
               | --dnd on|off|toggle [--for DURATION | --until HH:MM]
               | --clear-notifications
               | --failed-refresh
               | --osd brightness
               | --quick [sound|wifi|bluetooth|vpn]
               | --calc | --windows | --notes
               | --timer start DURATION|pomodoro|stopwatch|pause|resume|toggle|stop|skip|popup
               | --shield on|off|toggle|status|allow
               | --capture full|area|area-save|pick|annotate|ocr|record|record-area|stop
               | --clipboard | --clipboard-clear
               | --tools
               | --kill
               | --power
               | -h|--help]
```

It sends one request to `$XDG_RUNTIME_DIR/alpheus-launcher.sock` and waits
for the answer: no Wayland, no fonts. Exactly one verb is allowed; with none,
`--toggle` is assumed. `--prompt` and `--visible` may be given once.

| Verb | Effect |
|---|---|
| `--toggle` (default) | open the launcher, or close it if it is open |
| `--close` | close the launcher |
| `--dmenu` | read lines on standard input, show them in the launcher, print the chosen one on standard output (a `fuzzel --dmenu` replacement) |
| `--dnd on\|off\|toggle` | set the notifications module's do-not-disturb, kept across restarts in `$XDG_STATE_HOME/alpheus/dnd`; `--dnd on --for 30m` (`2h`, `1h30m`; 1 minute to 24 hours) or `--dnd on --until 22:30` (local time, tomorrow if already past) ends it by itself: one timer, armed only while it runs, and a restarted daemon keeps the end or turns it off if it passed; `off` and `toggle` cancel it; an older daemon exits 3. An alpheus older than this feature reads any state file as plain on, so a rollback keeps do-not-disturb on (untimed) until you toggle it |
| `--clear-notifications` | empty the notification history |
| `--failed-refresh` | read the failed-unit counts again now (the system manager's through the D-Bus thread, the user manager's on the loop), whatever `[failed] subscribe` says; the control center sends it itself after a unit action and when the Diag tab shows |
| `--osd brightness` | show the laptop backlight's level in the OSD (read once, never changed) |
| `--quick [sound\|wifi\|bluetooth\|vpn]` | open or close the quick settings panel on the focused output; the word opens it on that detail (`sound`: at the volume; exit 0 also without the `quick` module); a `--quick` less than 250 ms after the one that opened the panel is ignored |
| `--calc` | open the launcher in calculator mode: closed, it opens; open, in whatever mode (apps box or script menu), it closes, as `--toggle` does; a `--calc` less than 250 ms after the one that opened it is ignored |
| `--windows` | open the window switcher with the previous window selected; a second `--windows` while it is open steps to the next window; it replaces any other open launcher and cancels a waiting `--dmenu` (a password prompt included); only a `--windows` within 250 ms of the switcher's own open is ignored |
| `--notes` | open or close the quick notepad ([notes](features/notes.md)); a `--notes` less than 250 ms after the one that opened it is ignored |
| `--timer start <duration>\|pomodoro\|stopwatch\|pause\|resume\|toggle\|stop\|skip\|popup` | drive the focus timer: a duration is `90s`, `25m`, `1h30m` or a bare number of minutes, 1 second to 24 hours; `pause`... `skip` act on whatever runs (nothing to act on is no error); `popup` opens or closes the popup under the chip, the keyboard taken without a grab (a second within 250 ms of its own open is ignored); exit 2 without the `timer` module |
| `--shield on\|off\|toggle\|status\|allow` | drive [Shield mode](features/shield.md); exit 2 when `[shield]` is not set, the state cannot be read, a command is already running, or `allow` is asked with the mode off or nothing to run |
| `--capture full\|area\|area-save` | take a screenshot: `full` copies the focused output, `area` lets you drag an area (release or Enter confirms; Esc, a right click or a click without a drag cancels, silently) and copies it, `area-save` saves it as `%Y%m%d-%H%M%S.png` in `[captures] dir`. The copy goes through `launcher.toml`'s `copy` (PNG on its standard input; the default `wl-copy` is given `--type image/png`). One notice per capture; a failure is critical and leaves the clipboard untouched. A copy counts only when the copy command exits 0 within 5 s. A verb sent while a capture runs is ignored. Exit 2 when the compositor has no screencopy |
| `--capture pick` | the colour picker: the focused output is frozen under a zoom lens; a left click or Enter copies the pixel's colour as text (`#RRGGBB`, or `[captures] pick_format`) through `launcher.toml`'s `copy`, run as written, and one notice gives the value; Esc or a right click cancels, silently. Exit 2 when the compositor has no screencopy |
| `--capture annotate\|ocr` | drag an area (as `area`), then hand its PNG to `[captures] annotate` or `ocr`: alpheus copies the image the annotation tool wrote back, or the text the recognizer printed, through `launcher.toml`'s `copy`, and says so. A tool closed with nothing is a silent cancel; a tool that fails is critical; neither touches the clipboard. One capture at a time: while the tool is open, other captures are ignored. Exit 2 when the tool is `[]` or the compositor has no screencopy |
| `--capture record\|record-area` | start `[captures] record` on the focused output, or on a dragged area, into `screenrecord-%Y%m%d-%H%M%S.mp4` in the videos folder; the privacy screen glyph shows while it records. One recording at a time (a second is ignored); screenshots may be taken meanwhile. Exit 2 when the tool is `[]` or the compositor has no screencopy |
| `--capture stop` | end the recording: SIGINT to the recorder alpheus started, and once it finished its file a notice `Recording saved: <file name>` (a failure is critical). Exit 2 when nothing records |
| `--capture select` | debug: run the area selector alone, nothing is captured; a second call cancels it |
| `--clipboard` | open or close the [clipboard history](features/clipboard.md) in the launcher's box; any other open launcher is replaced; a `--clipboard` less than 250 ms after the one that opened it is ignored |
| `--clipboard-clear` | forget every unpinned entry without touching the current clipboard; exit 2 when `[clipboard] enabled` is not set or the compositor offers no data-control protocol |
| `--tools` | open the Tools menu (categories, a flat search, built-in entries and `[[tools.entry]]` from `launcher.toml`), or close it; any other open launcher is replaced. A second `--tools` less than 250 ms after the one that opened it is ignored |
| `--kill` | open the kill list (your own processes, sorted by memory then by CPU), or close it; any other open launcher is replaced. Enter asks the process to quit, Ctrl+Enter then Enter kills it. A second `--kill` less than 250 ms after the one that opened it is ignored |
| `--power` | open the ⏻ menu (lock, log out, reboot, power off) under the bar's right end on the focused output, or close it; works without the `power` module, exit 2 when no `[quick]` session command is set |
| `-h`, `--help` | print the usage and exit 0 |

`--dmenu` options:

| Option | Effect |
|---|---|
| `-p`, `--prompt TEXT` | text shown before the input (also `--prompt=TEXT`) |
| `--index` | print the chosen line's position (from 0, counting empty lines as fuzzel does) instead of its text; `-1`, exit 0, for typed text matching no line, as fuzzel. Not with `--password` |
| `--password` | draw one `•` per typed character, show no list (standard input is read and ignored); the text is printed as typed, never logged nor kept in the history, and wiped from memory on close. Not with `--index` or `--visible` |
| `--visible N` | with an empty query, list only the first N non-empty lines; typing searches every line (a script's home menu: its categories first, every entry by typing). Not with `--password` |
| `--placeholder TEXT`, `--width N`, `--lines N` | accepted for fuzzel's sake (also as `--name=value`), checked, then ignored |

`--dmenu` waits for the person with no timeout of its own; every other verb
gives up after 2 seconds without an answer. Standard input is capped (lines
and bytes; over the cap is an error before any connection).

| Exit code | Meaning |
|---|---|
| 0 | done, or a line was chosen (`--osd`: also when there is nothing to show or the `osd` module is off; `--quick`: also without the `quick` module; `--failed-refresh`: also without the `failed` module, where nothing is read) |
| 1 | the dmenu was cancelled (Escape) |
| 2 | an error: another menu is open, bad usage, no answer, input too large, the bar restarting mid-request, the notifications module not in `bar.toml` (for `--dnd`, `--clear-notifications`) |
| 3 | `alpheus-demon` is not running (no socket), or is older than the verb or option used (`--index`, `--password`, `--visible`, `--failed-refresh`, `--quick` and its words, `--calc`, `--windows`, `--notes`, `--timer`, `--shield`, `--capture`, `--clipboard`, `--clipboard-clear`, `--tools`, `--kill`, `--power`): restart it after an upgrade (a script then falls back to another menu with the same options). The only code on which a caller should fall back to another menu |

Messages go to standard error, prefixed `alpheus-launch:`. The 250 ms rule on
`--quick`, `--calc`, `--windows`, `--notes`, `--timer popup`, `--clipboard` and
`--tools` exists so that a compositor that re-fires a press bind does not close
what it just opened.

Examples:

```sh
printf 'one\ntwo\nthree\n' | alpheus-launch --dmenu -p 'Pick' || echo "cancelled or not running"
alpheus-launch --dnd toggle
alpheus-launch --quick wifi
alpheus-launch --tools; [ $? -eq 3 ] && tools-menu   # fall back to a menu script only when the daemon is too old or not running
```
