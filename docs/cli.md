# Command lines

alpheus ships three programs. This page lists every option and exit code, as
read from their parsers.

## `alpheus` (the control center)

```
alpheus [--tab <name>]
alpheus [--tab=<name>]
```

Opens the TUI in the current terminal, on the Home tab unless told otherwise.
`<name>` is one of `home`, `sound`, `network`, `bluetooth`, `power`,
`displays`, `appearance` (`theme` is accepted for `appearance`; case is
ignored). The tabs are also reached with `1` to `7` once open.

There is no `--help`; the usage appears in the error text.

| Exit code | Meaning |
|---|---|
| 0 | the TUI ran and ended (`q`, or SIGTERM/SIGHUP, which stop it cleanly) |
| 2 | an unknown option, a missing `--tab` value or an unknown tab name (message on standard error) |

Configuration: `~/.config/alpheus/config.toml`, see the README's
[Configure](../README.md#configure) section.

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
  `$XDG_RUNTIME_DIR/alpheus-launcher.sock` (owner-only), which
  `alpheus-launch` talks to.
- **Signals.** SIGTERM, SIGHUP and SIGINT stop it: the loop ends normally
  (a waiting `--dmenu` gets "cancelled"), and if it does not finish within a
  grace period (1 second) the process exits with 128 + the signal number.
- **Logs.** One line per event on standard error, prefixed `alpheus-demon:`
  (under systemd: `journalctl --user -u <your unit>`). Configuration
  warnings and the detected compositor are logged at start.
- **Configuration** is read once at start for `bar.toml`; restart the daemon
  to apply it. Theme files reload live and `launcher.toml` is read at each
  open (see the README's [Configure](../README.md#configure) section).
- **Exit code.** 0 after a clean stop; 1 when it cannot start (already
  running, no Wayland session, unusable fonts or signals) or when its event
  loop fails.

## `alpheus-launch` (scripting bridge to the daemon)

```
alpheus-launch [--toggle | --close
               | --dmenu [-p|--prompt TEXT] [--index] [--password] [--visible N]
               | --dnd on|off|toggle
               | --clear-notifications
               | --osd brightness
               | --quick [sound|wifi|bluetooth]
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
| `--dnd on\|off\|toggle` | set the notifications module's do-not-disturb |
| `--clear-notifications` | empty the notification history |
| `--osd brightness` | show the laptop backlight's level in the OSD (read once, never changed) |
| `--quick [sound\|wifi\|bluetooth]` | open or close the quick settings panel on the focused output; the word opens it on that detail (`sound`: at the volume) |
| `-h`, `--help` | print the usage and exit 0 |

`--dmenu` options:

| Option | Effect |
|---|---|
| `-p`, `--prompt TEXT` | text shown before the input (also `--prompt=TEXT`) |
| `--index` | print the chosen line's position (from 0, counting empty lines) instead of its text; `-1` for typed text matching no line. Not with `--password` |
| `--password` | show the typed text as dots, no list (standard input is ignored); the text is printed as typed and never logged. Not with `--index` or `--visible` |
| `--visible N` | list only the first N non-empty lines until something is typed; typing searches every line |
| `--placeholder TEXT`, `--width N`, `--lines N` | accepted for fuzzel's sake (also as `--name=value`), checked, then ignored |

`--dmenu` waits for the person with no timeout of its own; every other verb
gives up after 2 seconds without an answer. Standard input is capped (lines
and bytes; over the cap is an error before any connection).

| Exit code | Meaning |
|---|---|
| 0 | done, or a line was chosen (`--osd`: also when there is nothing to show; `--quick`: also without the `quick` module) |
| 1 | the dmenu was cancelled (Escape) |
| 2 | an error: another menu is open, bad usage, no answer, input too large, the bar restarting mid-request, the notifications module not in `bar.toml` (for `--dnd`, `--clear-notifications`) |
| 3 | `alpheus-demon` is not running (no socket), or is older than `--index`, `--password`, `--visible` or `--quick` (and its `wifi`/`bluetooth` words): restart it after an upgrade. The only code on which a caller should fall back to another menu |

Messages go to standard error, prefixed `alpheus-launch:`. A `--quick`
arriving less than 250 ms after the one that opened the panel is ignored, so a
compositor that re-fires a press bind does not close it at once.

Examples:

```sh
printf 'one\ntwo\nthree\n' | alpheus-launch --dmenu -p 'Pick' || echo "cancelled or not running"
alpheus-launch --dnd toggle
alpheus-launch --quick wifi
```
