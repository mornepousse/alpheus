# Compositors and external commands

alpheus never writes to a compositor. It only **reads** each compositor's own
socket for state, and every action that must reach a compositor (switching a
workspace, focusing a window, changing a monitor, logging out) goes through an
external command that you provide. This page lists what each compositor gives
the bar, and exactly which commands alpheus runs and what it expects of them.

## Detection

The daemon and the TUI look at four environment variables, in this order; the
first one that is set and non-empty decides (an unusable value gives no
compositor rather than falling through to the next):

1. `SWAYSOCK` (Sway, i3 IPC)
2. `HYPRLAND_INSTANCE_SIGNATURE` (Hyprland, with `XDG_RUNTIME_DIR`)
3. `MANGO_INSTANCE_SIGNATURE` (Mango; the variable holds the socket's absolute path)
4. `NIRI_SOCKET` (niri)

The daemon logs the result once at start (`compositor: Sway ($SWAYSOCK)`). With
none of them, it logs `no compositor detected` and the bar shows "–" where the
workspaces would be.

## Support matrix

Legend: yes = implemented and covered by tests; partial = see the note;
no = not available; "unverified" = reasoned from the code, never run.

| Feature | Hyprland | niri | Sway | Mango | Other layer-shell |
|---|---|---|---|---|---|
| Workspaces in the bar | yes: socket events | yes: event stream | yes: i3 IPC (workspace and window events) | yes: one workspace per (output, tag); empty inactive tags hidden | no: the bar shows "–" |
| Switching workspace (click, wheel) | via `[workspaces] command` (`wm-set workspace` by default) | same | same | same | n/a |
| Window title | yes | yes | yes | yes (the active output's client) | no |
| Launcher's windows list and focus | yes: `address:0x…` selectors | yes: `id:N` | yes: `id:<con_id>` | yes: `id:<client id>` | no: the list is empty |
| Order of that list | previous window first, focused last | same | same (Sway's own focus order) | same (a focus history the daemon keeps) | n/a |
| Screen-capture privacy indicator | yes: PipeWire, plus the compositor's `screencast` events | yes: PipeWire, plus niri's cast events | partial: PipeWire only; a recorder that does not go through PipeWire (wf-recorder) is not seen | partial: PipeWire only, same limit | partial, unverified: PipeWire only |
| Displays tab (TUI) | yes, through `wm-set outputs` and `wm-set monitor` | no: the tab says "not available under niri" and calls nothing | no: same message for Sway | partial: calls `wm-set`, which must support Mango (the author's `wm-set` does not yet) | unverified: the tab calls `wm-set` as under Hyprland |
| Keyboard-grab caveat | On every Hyprland version the bar stays non-exclusive while a menu grabs (Hyprland hands the grab the keyboard itself). The `alpheus-launch --quick` panel has no grab and makes the bar exclusive; on Hyprland before 0.56.2 (hyprwm/Hyprland#15729) a click inside it may then close it | none known | a grabbing popup gets no keyboard unless the bar surface is exclusive: alpheus makes it exclusive during a grab | unverified | unverified |

Notes.

- Sway and Mango were added last. They are covered by tests against recorded
  Mango captures and a headless Sway, and the daemon's memory and wake-up
  tests run under both. The author's daily trial under them is still pending,
  so treat them as less proven than Hyprland and niri.
- On any compositor the bar also needs layer-shell, and its menus need
  xdg-shell popups. The notification bubbles, the OSD and the launcher are
  layer-shell surfaces on the Overlay layer (above a fullscreen window).
- "Other layer-shell compositor" (river, labwc, …) has not been run by the
  author. The clock, resources, sound, network, Bluetooth, battery, tray,
  media, notifications, OSD, launcher (apps and commands, not windows),
  privacy and quick panel do not depend on the compositor; what is missing
  is the workspace and window information.
- Mango and Sway tree depth is bounded: a reply nested past 90 JSON levels is
  treated as unreadable rather than parsed.

## External commands

Commands alpheus runs that **you must provide** (or replace). "Configurable"
means a key in a config file; the others are hard-wired today.

Every command is run without a shell, as a plain argument vector. The TUI
kills a tool call after 2 seconds; the daemon starts its commands detached
(clicks, the launcher, `[quick]`), so they are not timed, except the launcher's
`list` sources (1 s). A missing program is reported in the UI ("<tool> not found" in the tab, or
a log line) and nothing else breaks.

### `wm-set`

A single script is the one place where compositor-specific writes live. The
repository does not ship it (the author's lives in a private configuration), so
here is the contract. Write one for your compositor, or point the configurable
commands at your own tools.

| Call | Who runs it | Configurable | What it must do |
|---|---|---|---|
| `wm-set workspace <ref> <output>` | the bar, on a click or wheel on a workspace | yes: `[workspaces] command` in `bar.toml`, with `{workspace}` and `{output}` | switch `<output>` to the workspace `<ref>` |
| `wm-set focus-window <selector>` | the launcher, when a window is chosen | yes: `focus` in `launcher.toml`, with `{window}` (required) | focus that window |
| `wm-set exit` | the TUI's Power tab "Log out"; the bar's quick panel and ⏻ menu do **not** use it | the TUI: no; the bar: `[quick] logout` (empty by default) | end the compositor session |
| `wm-set outputs` | the TUI's Displays tab | no | print the outputs as JSON, see below |
| `wm-set monitor <output> <WxH@R> <X>x<Y> <scale> <rotation>` | the Displays tab, to apply a layout or roll one back | no | enable or reconfigure an output; exit 0 on success |
| `wm-set monitor-disable <output>` | the Displays tab | no | disable an output; exit 0 on success |

`<ref>` and `<selector>` are opaque strings that the daemon takes from the
compositor you run, so your script only has to hand them back to it:

| Compositor | workspace `<ref>` | window `<selector>` |
|---|---|---|
| Hyprland | the workspace id (`3`), or `name:<name>` for a named one (negative id) | `address:0x<hex address>` |
| niri | the workspace's index on its output | `id:<window id>` |
| Sway | the workspace number (`3`), or `name:<name>` when it has none | `id:<con id>` |
| Mango | the tag index | `id:<client id>` |

`<output>` is the compositor's own output name (`eDP-1`, `HDMI-A-1`).

The Displays tab parses `wm-set outputs` strictly. It expects a JSON array,
one object per output, enabled or not:

```json
[
  {
    "name": "eDP-1",
    "description": "Interne Dalle 0x0001",
    "enabled": true,
    "mode": "2560x1600@60",
    "modes": ["2560x1600@60"],
    "x": 0,
    "y": 1080,
    "scale": 1.00,
    "transform": 0,
    "displaylink": false
  }
]
```

- `mode` is the current mode as `<width>x<height>@<refresh>` (`@60`, no
  unit); for a disabled output it is ignored (`"0x0@0"` is fine). An
  unreadable `mode` on an **enabled** output makes the whole reply unreadable:
  alpheus will not guess the state of a screen that is on. Unreadable entries
  in `modes` are skipped.
- `x`, `y`: position in the compositor's layout; `scale`: a number;
  `transform`: an integer 0 to 7 (Wayland's output transform: 0 normal,
  1 to 3 rotations, 4 to 7 flipped); `displaylink`: true for a USB-docked
  (DisplayLink) output; the tab marks it with a warning sign and a rollback
  re-enables it last.
- A fixture is in `crates/alpheus-commun/tests/fixtures/wm-set-outputs.json`
  (descriptions anonymised).
- The arguments alpheus passes to `monitor` are the mode as above, the
  position as `<x>x<y>`, the scale as `1`, `1.25` or `1.2` (trailing zeroes
  trimmed) and the rotation as the same 0 to 7 integer.
- Enabling steps run before disabling ones, and any failure triggers a
  best-effort rollback with the same calls; exit non-zero on failure.

### Other commands

| Command | Who runs it | Configurable | Expects |
|---|---|---|---|
| `alpheus-toggle <tab>` (`sound`, `network`, `bluetooth`, `home`) | the bar, on a click on volume, network, Bluetooth or battery | yes: `[actions] open_tab`, `{tab}` replaced | open (or toggle) the TUI at that tab, typically `alpheus --tab <tab>` in a floating terminal. Without the script a click does nothing, so set `open_tab` (for example to `["foot", "--app-id", "alpheus-tui", "-e", "alpheus", "--tab", "{tab}"]`) |
| `foot -e btop` | the bar, left click on the resources module | yes: `[actions] resources` | any terminal command |
| `foot` / `foot --hold` | the launcher, for terminal apps (`terminal`, `terminal_hold`) | yes: `launcher.toml` | a terminal that accepts a command after its arguments |
| `systemd-run --user --scope --quiet --collect -- <command>` | the launcher, to start apps | no (falls back to a detached spawn when `systemd-run` is not installed) | systemd with a user manager |
| `loginctl lock-session`, `systemctl reboot`, `systemctl poweroff` | the quick panel and the ⏻ menu | yes: `[quick] lock`, `reboot`, `poweroff` (empty hides the entry); `logout` is empty by default | any command; reboot and power off ask for confirmation first |
| `hyprlock` | the TUI's Power tab "Lock" | no | a screen locker started detached (non-Hyprland users: put a `hyprlock` shim on `PATH` that runs yours) |
| `systemctl suspend`, `reboot`, `poweroff` | the TUI's Power tab | no | systemd |
| `shell-ctl dnd-status` / `dnd-on` / `dnd-off` | the TUI's Home tab (do not disturb) | no | `dnd-status` prints `on` or `off` on standard output (anything else is an error); the other two set the state, exit 0. The bar's own notifications module does not use it: it has `alpheus-launch --dnd` |
| `tools-menu "Garder éveillé"` | the TUI's "Keep awake" | no | toggles keep-awake; alpheus reads the state from the flag file `$XDG_RUNTIME_DIR/garder-eveille`, which the script must create or remove (names kept as the author's contract) |
| `wpctl`, `nmcli`, `bluetoothctl`, `brightnessctl`, `powerprofilesctl` | the TUI's tabs | no | the stock tools |
| `wiremix`, `nmtui`, `bluetui` | the TUI, to open a full tool | yes: `[commands] mixer`, `network`, `bluetooth` in `config.toml` | an interactive terminal program |
| `foot -e nmtui` | the quick panel's Wi-Fi "More…" | yes: `[quick] wifi_more` | a terminal command |
| `[[sources]]` `list` / `run` | the launcher's command sources | yes: `launcher.toml` | `list` prints one entry per line within 1 s; `run` receives the chosen line as `{line}` |

The TUI hard-wires `hyprlock`, `shell-ctl` and `tools-menu` today; making them
configurable with neutral fallbacks is the portability work that is still open
(the daemon's bar side is already configurable, as above). Until then, a user
of another setup either provides small scripts with these names or leaves the
corresponding TUI actions unused: each one fails with a visible
"<tool> not found" message and affects nothing else.
