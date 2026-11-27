# Compositors and external commands

alpheus never writes to a compositor's socket. It only **reads** each
compositor's own socket for state, and every action that must reach a
compositor (switching a workspace, focusing a window, logging out) goes through
an external command that you provide. One exception: the daemon changes the
screens' layout itself through the Wayland protocol `zwlr_output_configuration_v1`
(`outputs/wlr.rs`; the Displays tab and screen profiles, below), never output
power management, never `dpms`. This page lists what each compositor gives
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
no = not available; "unverified" = reasoned from the code, never run (for the
Displays row: the protocol code is shared, but only Sway's was run, headless).

| Feature | Hyprland | niri | Sway | Mango | Other layer-shell |
|---|---|---|---|---|---|
| Workspaces in the bar | yes: socket events | yes: event stream | yes: i3 IPC (workspace and window events) | yes: one workspace per (output, tag); empty inactive tags hidden | no: the bar shows "–" |
| Switching workspace (click, wheel) | via `[workspaces] command` (`wm-set workspace` by default) | same | same | same | n/a |
| Window title | yes | yes | yes | yes (the active output's client) | no |
| Keyboard layout and mode | no (retired) | no (retired) | next task | yes: the monitors snapshot (`keyboardlayout`, `keymode`), the active monitor's; read only with the `keyboard` module | no |
| Launcher's windows list and focus | yes: `address:0x…` selectors | yes: `id:N` | yes: `id:<con_id>` | yes: `id:<client id>` | no: the list is empty |
| Order of that list | previous window first, focused last | same | same (Sway's own focus order) | same (a focus history the daemon keeps) | n/a |
| Screen-capture privacy indicator | yes: PipeWire, plus the compositor's `screencast` events | yes: PipeWire, plus niri's cast events | partial: PipeWire only; a recorder that does not go through PipeWire (wf-recorder) is not seen | partial: PipeWire only, same limit | partial, unverified: PipeWire only |
| Displays tab (TUI) and profiles on hotplug | implemented, unverified: through `zwlr_output_manager_v1`, in the daemon | implemented, unverified: same | yes: same, tested against a headless Sway | implemented, unverified: same | unverified: works if the compositor implements `zwlr_output_manager_v1`; otherwise the tab says "the compositor has no output management" |
| Keyboard-grab caveat | On every Hyprland version the bar stays non-exclusive while a menu grabs (Hyprland hands the grab the keyboard itself). The `alpheus-launch --quick` panel has no grab and makes the bar exclusive; on Hyprland before 0.56.2 (hyprwm/Hyprland#15729) a click inside it may then close it | none known | a grabbing popup gets no keyboard unless the bar surface is exclusive: alpheus makes it exclusive during a grab | unverified | unverified |

Notes.

- Sway and Mango were added last. They are covered by tests against recorded
  Mango captures and a headless Sway, and the daemon's memory and wake-up
  tests run under both. The author's daily trial under them is still pending,
  so treat them as less proven than Hyprland and niri. That does not apply to
  the Displays row, the other way round: Sway is the one compositor its tests
  run against.
- A configuration the Displays tab or a profile applied lasts until the
  compositor reloads its own: Sway keeps it until `swaymsg reload`, niri and
  Hyprland lose it on a config reload. The daemon re-applies a matching profile
  only when the screens change, not on a compositor reload.
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
kills a tool call after 2 seconds, except the Power tab's `[quick]` session commands, which it
starts detached like the daemon does; the daemon starts its commands detached
(clicks, the launcher, `[quick]`), so they are not timed, except the launcher's
`list` sources (1 s). A missing program is reported in the UI ("<tool> not found" in the tab, or
a log line) and nothing else breaks.

### `wm-set`

A single script is the one place where compositor-specific writes live (the
screens' layout aside: the daemon sets it through the output protocol, below). The
repository does not ship it (the author's lives in a private configuration), so
here is the contract. Write one for your compositor, or point the configurable
commands at your own tools.

| Call | Who runs it | Configurable | What it must do |
|---|---|---|---|
| `wm-set workspace <ref> <output>` | the bar, on a click or wheel on a workspace | yes: `[workspaces] command` in `bar.toml`, with `{workspace}` and `{output}` | switch `<output>` to the workspace `<ref>` |
| `wm-set focus-window <selector>` | the launcher, when a window is chosen | yes: `focus` in `launcher.toml`, with `{window}` (required) | focus that window |

`<ref>` and `<selector>` are opaque strings that the daemon takes from the
compositor you run, so your script only has to hand them back to it:

| Compositor | workspace `<ref>` | window `<selector>` |
|---|---|---|
| Hyprland | the workspace id (`3`), or `name:<name>` for a named one (negative id) | `address:0x<hex address>` |
| niri | the workspace's index on its output | `id:<window id>` |
| Sway | the workspace number (`3`), or `name:<name>` when it has none | `id:<con id>` |
| Mango | the tag index | `id:<client id>` |

`<output>` is the compositor's own output name (`eDP-1`, `HDMI-A-1`).

### The Displays tab needs no command

The tab and the screen profiles do not call `wm-set`: the daemon binds
`zwlr_output_manager_v1` (versions 1 to 4) and the TUI asks it over the
launcher's socket (`outputs`, `outputs-try`, `outputs-keep`, `outputs-revert`).
The daemon holds the 15 s trial, so a TUI that dies cannot leave a bad layout,
and applies the `displays.toml` profile that matches the connected screens when
they change (`[displays] auto`, on by default; the `displays` module must be in
`modules`). A DisplayLink screen is recognised from
`/sys/class/drm/card*-<name>/device/device/driver` ending in `evdi`
(`[displays] drm_dir`), and is kept at 120 Hz or below. A compositor without
the protocol gets the message "the compositor has no output management" and
nothing else breaks. At most 3 profiles are applied automatically per set of screens per minute (a dock reconnecting in a loop must not modeset forever); a profile held back is applied when that minute ends, if the screens are still the same. When the daemon stops during a trial it sends back the layout from before. Known limits: a daemon that crashes loses the trial (the trial layout stays), and a try still waiting for the compositor's answer at shutdown may keep its layout (the restore can be cancelled by it).

### Captures and layer animations

`alpheus-launch --capture` asks for one frame through `zwlr_screencopy_manager_v1` (Sway, Hyprland,
Mango and niri offer it; without it the verbs exit 2 and the Captures entries are hidden). Every
capture waits for one round trip with the compositor before the frame is asked, so a surface that
has just been destroyed (the area selector, the Tools menu the verb came from) is gone from the
scene. A compositor that fades layer surfaces out, as Hyprland does by default, may still be
drawing the fade when the frame is taken, so a screenshot can hold the closing selector's dim or the
launcher's box half transparent. Turn the animation off for the namespaces alpheus uses:

- `alpheus-selector`: the area selector;
- `alpheus-launcher`: the launcher and the Tools menu, and `alpheus-launcher-scrim` behind them.

On Hyprland, a `noanim` layer rule for each (`layerrule = noanim, alpheus-selector`; newer releases
write it `layerrule = no_anim on, match:namespace alpheus-selector`: use the form your version
accepts). Mango's layer close animation has not been checked: if a capture shows a fade, give the
same namespaces no animation there. Sway draws no such animation, and the headless test of the
dim passes on it.

### Other commands

| Command | Who runs it | Configurable | Expects |
|---|---|---|---|
| `alpheus-toggle <tab>` (`sound`, `network`, `bluetooth`, `home`, `diagnostics`) | the bar, on a click on volume, network, Bluetooth or battery | yes: `[actions] open_tab`, `{tab}` replaced | open (or toggle) the TUI at that tab, typically `alpheus --tab <tab>` in a floating terminal. Without the script a click does nothing, so set `open_tab` (for example to `["foot", "--app-id", "alpheus-tui", "-e", "alpheus", "--tab", "{tab}"]`) |
| `foot -e btop` | the bar, right click on the resources module, and the popup's `Monitor…` row | yes: `[actions] resources` | any terminal command |
| `foot` / `foot --hold` | the launcher, for terminal apps (`terminal`, `terminal_hold`) | yes: `launcher.toml` | a terminal that accepts a command after its arguments |
| `systemd-run --user --scope --quiet --collect -- <command>` | the launcher, to start apps | no (falls back to a detached spawn when `systemd-run` is not installed) | systemd with a user manager |
| `loginctl lock-session`, `systemctl reboot`, `systemctl poweroff` | the quick panel and the ⏻ menu | yes: `[quick] lock`, `reboot`, `poweroff` (empty hides the entry); `logout` is empty by default | any command; reboot and power off ask for confirmation first |
| `[quick]` `lock`, `logout`, `reboot`, `poweroff` | the TUI's Power tab, the same commands as the bar's | yes: `bar.toml`; empty hides the button | any command; started detached, like the bar's |
| `systemctl suspend` | the TUI's Power tab | no | systemd |
| `shell-ctl dnd-status` / `dnd-on` / `dnd-off` | the TUI's Home tab (do not disturb) | no | `dnd-status` prints `on` or `off` on standard output (anything else is an error); the other two set the state, exit 0. The bar's own notifications module does not use it: it has `alpheus-launch --dnd` |
| `tools-menu "Garder éveillé"` | the TUI's "Keep awake" | no | toggles keep-awake; alpheus reads the state from the flag file `$XDG_RUNTIME_DIR/garder-eveille`, which the script must create or remove (names kept as the author's contract) |
| `wpctl`, `nmcli`, `bluetoothctl`, `brightnessctl`, `powerprofilesctl` | the TUI's tabs | no | the stock tools |
| `wiremix`, `nmtui`, `bluetui` | the TUI, to open a full tool | yes: `[commands] mixer`, `network`, `bluetooth` in `config.toml` | an interactive terminal program |
| `foot -e wiremix` | the quick panel's "Mixer…" | yes: `[quick] mixer` | a terminal command; the default is shown even without wiremix installed |
| `foot -e nmtui` | the quick panel's Wi-Fi "More…" | yes: `[quick] wifi_more` | a terminal command |
| `[[sources]]` `list` / `run` | the launcher's command sources | yes: `launcher.toml` | `list` prints one entry per line within 1 s; `run` receives the chosen line as `{line}` |

The TUI hard-wires `shell-ctl` and `tools-menu` today; making them
configurable with neutral fallbacks is the portability work that is still open
(the daemon's bar side is already configurable, as above). Until then, a user
of another setup either provides small scripts with these names or leaves the
corresponding TUI actions unused: each one fails with a visible
"<tool> not found" message and affects nothing else.
