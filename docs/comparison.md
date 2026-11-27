# Features compared

This table guides the roadmap and is updated with every delivery.

The roadmap's yardstick: what DankMaterialShell and Noctalia offer, within a
fraction of their memory. ✅ done · 🚧 planned (delivery) · 💭 to decide ·
❌ out of scope by design · ➖ partial or through another tool.

| Feature | **alpheus** | DMS | Noctalia | ashell | waybar stack¹ |
|---|---|---|---|---|---|
| **Resident memory** | **≈ 12 MB** today (test harness: bar and sound source), ≤ 15 MB budget at rest (see `decisions.md`) | ≈ 537 MB | ≈ 517 MB (4.7) | ≈ 194 MB² | ≈ 55 MB³ |
| **Bar** | | | | | |
| Workspaces, active window | ✅ | ✅ | ✅ | ➖ | ✅ |
| Clock | ✅ | ✅ | ✅ | ✅ | ✅ |
| Calendar popup | ✅ month grid, locale first day, ISO weeks | ✅ | ➖ | ✅ | ✅ |
| Volume, network, Bluetooth, battery | ✅ | ✅ | ✅ | ✅ | ✅ |
| CPU, memory, temperature | ✅ and, opt-in through `[resources] show`, network rates, the disk's use and the GPU's temperature; a popup with a 60-sample graph per item (left click), the monitor on the right click | ✅ | ➖ | ✅ | ✅ |
| System tray (icons and menus) | ✅ Nerd Font glyphs, no images | ✅ | ✅ | ✅ | ✅ |
| Media (MPRIS) | ✅ `▶ Title` in the bar, middle click play/pause, wheel next/previous; a popup with time, progress, ⏮ ⏯ ⏭ and the player choice | ✅ | ✅ | ✅ | ✅ |
| Notification counter, do-not-disturb | ✅ a neutral bell with the unread count, crossed under do-not-disturb (middle click, or `alpheus-launch --dnd on\|off\|toggle`) | ✅ | ✅ | ➖ | ➖ |
| Timed do-not-disturb, and while sharing the screen | ✅ `alpheus-launch --dnd on --for 30m` or `--until 22:30`, or the quick panel's Do not disturb detail, kept across a restart; `[notifications] dnd_while_sharing` | ➖ | ✅ | ❌ | ❌ |
| Per-app notification rules | ✅ `[[notifications.rules]]`: mute (kept in the history), drop, or change the urgency | ➖ | ✅ | ❌ | ➖ |
| Low battery alerts | ✅ a notification at 20, 10 and 5 % while discharging (`[battery] warn`), the lowest one critical | ➖ | ✅ | ❌ | ➖ |
| Event hooks | ✅ `[hooks]` in `bar.toml`: a command (an argv, run as given) per event — started, theme mode, do-not-disturb, battery charge and level, power profile, network, VPN, Bluetooth, privacy, Shield, failed units, displays — with the data in `ALPHEUS_*` variables, run in its own systemd scope, rate-limited, never logged | ➖ | ✅ | ❌ | ➖ |
| Shield mode (public-network security: state, toggle, blocked USB devices) | ✅ `[shield]` in `bar.toml`: a green glyph while the mode is on, a quick-panel row and a blocked-USB detail, `alpheus-launch --shield on\|off\|toggle\|status\|allow`, notices, a hook; the root work stays in your system configuration (alpheus runs the argv you give it, nothing else); nothing exists unconfigured | ❌ | ❌ | ❌ | ❌ |
| Focus timer (countdown, Pomodoro, stopwatch) | ✅ a chip in the bar (`󰔛` idle, `󰔛 25m` running, `󰔟`/`󰅶` work and break, `󱎫` stopwatch, `󰏤` paused, `󰂞 done`), a popup under it, `alpheus-launch --timer start 25m\|pomodoro\|stopwatch\|pause\|resume\|toggle\|stop\|skip\|popup`; the end through the notification server and an optional sound; kept across a restart; no seconds on the chip, so one wake-up a minute | ❌ | ➖ a countdown plugin | ❌ | ❌ |
| Keyboard layout | ✅ opt-in `keyboard` module: `󰌌 us` from the compositor's own state (Mango, and Sway's `GET_INPUTS` and `input` events), the bind mode in the accent when not `default`, `[keyboard] short` and `show_mode`; display only, no lock keys | ✅ | ➖ | ✅ | ✅ |
| Privacy indicators (mic, camera, screen share) | ✅ glyphs in the bar for the microphone, a camera (through PipeWire, or opened directly as `/dev/video*`, found by an inotify watch and one `/proc` scan, no polling) or screen share seen through PipeWire, and a screen cast announced by niri or Hyprland; a click lists the apps; wf-recorder under Sway and Mango is invisible unless alpheus started it | ✅ | ➖ | ✅ | ✅ |
| Failed units (systemd) | ✅ `󰀦 N` in the bar, only above 0: `NFailedUnits` of the system and the user manager, pushed at once by systemd (`[failed] subscribe`, on by default: systemd then signals every client) or, with `subscribe = false`, read on triggers (start, resume, a refresh, a piggyback), a left click opens the Diag tab; a `failed_units` hook; an optional notification (`[failed] notify`) | — | — | — | ➖ a custom module of your own |
| Weather | 💭 | ✅ | ✅ | ✅ | ➖ |
| **Panels** | | | | | |
| Control center (quick settings) | ✅ a panel under the volume, network and Bluetooth modules (`alpheus-launch --quick [sound\|wifi\|bluetooth\|vpn]`): volume and output, brightness, Wi-Fi (networks, saved, open and WPA-personal with a password field), Bluetooth (power, paired and nearby devices, connect, pair with codes, forget), VPN (the NetworkManager profiles, up and down), a Bluetooth headset's Hi-Fi/Mic mode, microphone, do-not-disturb (with a duration), power profile, keep-awake, night light (switches on the toggle rows), labelled session buttons; the TUI (≈ 2 MB + terminal, only while open) for the rest: a left rail, a Home dashboard, shapes that follow the bar's style | ✅ | ✅ | ➖ | ❌ |
| Power menu | ✅ a ⏻ button at the bar's end: Lock, Log out, Reboot, Power off with the quick panel's commands, and up to 8 rows of your own (`[[power.entry]]`, never a sleep word) (reboot, power off and a `confirm` row ask first; a double click never confirms); the panel's session buttons and the TUI too | ✅ | ✅ | ✅ | ❌ |
| Display settings | ✅ TUI (enable, place, resolution, scale, profiles, 15 s rollback held by the daemon) through `zwlr_output_manager_v1`, no external script; profiles applied on hotplug (at most 3 a minute per set of screens, a held-back one applied when the minute ends); a trial restored when the daemon stops (lost if it crashes); DisplayLink kept at 120 Hz or below | ✅ | ➖ | ➖ | ❌ |
| Network, Bluetooth, per-app audio | ✅ Wi-Fi, Bluetooth and VPN in the quick panel (Bluetooth: connect, pair with codes, forget, headset mode; VPN: a row and a detail of the saved profiles, up and down (a failing `nmcli` shows `…` until NetworkManager answers or 20 s pass; a profile needing a secret no agent provides fails; `More…` (nmtui-edit) is where to store the secret); `More…` opens nmtui, and nmtui-edit for the VPN profiles, for enterprise, hidden networks, hotspot); per-app audio in the TUI (wiremix) and a `Mixer…` row in the quick panel that opens it | ✅ | ➖ | ✅ | ❌ |
| Notification center, history | ✅ a panel under the bell: the player, the last 50 notifications (in memory), do-not-disturb, Clear all | ✅ | ✅ | ➖ | ➖ |
| Session health, journal viewer, boot report, crash list, bug report | ✅ the control center's Diag tab: the compositor, `WAYLAND_DISPLAY` and the systemd user environment checked (your own checks in `config.toml`), failed units and units restarting in a loop with restart and reset-failed (system units: systemd's own password prompt in the tab's window), eight journal presets, filters by unit, priority and text, follow, the boot's `systemd-analyze`, recent crashes, and a redacted bug report shown in full before it is copied | — | — | — | ➖ |
| USB devices (debugging) | 🚧 TUI tab | ❌ | ❌ | ❌ | ❌ |
| **Launcher** | | | | | |
| Apps, run command, window switcher | ✅ | ✅ | ➖ | ❌ | ➖ |
| Script/dmenu mode | ✅ (fuzzel's exit codes) | ➖ | ➖ | ❌ | ✅ |
| Menu scripts' entries in the search (`[[sources]]`: a `list` command per open, `run` with the chosen line) | ✅ the entry, then its path dimmed; nothing resident, nothing per keystroke | ➖ | ✅ | ❌ | ❌ |
| Calculator and unit conversion | ✅ type `6*7`, `2^10`, `0xff + 1` or `5 km to mi` in Mod+Space, or `alpheus-launch --calc` (a bind of your own, Super+C) to open it directly; Enter copies the answer through `wl-copy` | ✅ | ➖ | ❌ | ❌ |
| Quick notepad | ✅ one pad of plain text on `alpheus-launch --notes` (a bind of your own): autosaved 2 s after an edit and on every close to `$XDG_STATE_HOME/alpheus/notes.txt`, Ctrl+C/X/V through `wl-copy` and the Wayland clipboard; nothing resident while closed | ➖ | ➖ | ❌ | ❌ |
| Web search | ✅ `? rust ratatui` in Mod+Space, Enter opens `search` through `open` (`xdg-open`) | ➖ | ➖ | ❌ | ❌ |
| Emoji picker | ✅ `:heart`, Enter copies it through `wl-copy`; 1,906 emoji compiled in, no resident cost | ✅ | ➖ | ❌ | ➖ |
| Shell command line | ✅ `$ …` runs `sh -c`, detached or in the held terminal | ➖ | ➖ | ❌ | ➖ |
| Window switcher (Alt+Tab) | ✅ `alpheus-launch --windows` from a compositor bind: the previous window first, the bind again steps | ➖ | ➖ | ❌ | ❌ |
| Currencies, dates | 🚧 later | ✅ | ➖ | ❌ | ❌ |
| System actions, sound, Wi-Fi/Bluetooth/VPN, control center tabs (the Tools menu) | ✅ `alpheus-launch --tools` (a bind of your own) or `/` in Mod+Space: categories to browse, a flat search across all of them, built-ins that open alpheus's own features, no configuration needed; nothing resident while closed | ➖ | ➖ | ❌ | ❌ |
| Your own entries in the Tools menu (a command, a terminal, a confirmation, an on/off state, choices) | ✅ `[[tools.entry]]` in `launcher.toml`: argv with no shell, `confirm` asks `Name? ⏎` (second Enter after 0.4 s), `state` shows the quick panel's switch, fixed `choices` or a `choices_cmd` list; read on a short-lived thread, no plugin runtime | ➖ | ➖ | ❌ | ❌ |
| SSH | 🚧 planned | ➖ | ➖ | ❌ | ❌ |
| Clipboard history | ✅ off unless `[clipboard] enabled = true`: text only, 50 entries of 4 KiB in memory (nothing on disk), pins, `alpheus-launch --clipboard` (a bind of your own) and the `"` prefix in Mod+Space; a copy with a password-manager hint is never stored (see [Clipboard history](features/clipboard.md)) | ✅ | ➖ | ❌ | ➖ |
| Files | 🚧 planned | ✅ | ➖ | ❌ | ➖ |
| **Other** | | | | | |
| Notification toasts | ✅ bubbles top centre on the focused output; a click runs the app's action, critical ones stay; replaces mako (≈ 6.7 MB) | ✅ | ✅ | ✅ | ✅ |
| OSD (volume, brightness, and more) | ✅ volume and mute, and the microphone's mute (never its level: call apps move it), from PipeWire's own state (any app, any key, a Bluetooth headset), laptop brightness on `alpheus-launch --osd brightness`; opt-in kinds in `[osd] kinds`: Bluetooth power, do-not-disturb, the power profile, the keyboard layout and a new track, each starting only the source it reads; bottom centre on the focused output, never takes a click; replaces swayosd (≈ 7.6 MB) | ✅ | ✅ | ✅ | ✅ |
| Colours from the wallpaper (matugen) | ✅ dark and light files, live | ✅ | ✅ | ❌ | ❌ |
| Colour themes, light/dark following the desktop | ✅ 15 presets, 7 hues per theme, per-module colours, auto light/dark, live reload | ✅ | ✅ | ➖ | ➖ |
| Bar styles (islands, powerline, floating…) | ✅ 6 styles (flat, islands, powerline, floating, underline, translucent), pills, chosen in the Theme tab, live reload | ✅ | ✅ | ➖ | ➖ |
| Appearance settings | ✅ the TUI's Theme tab: theme, light/dark mode, module colours, bar style, with a preview | ✅ | ✅ | ❌ | ❌ |
| Wallpaper management | 💭 | ✅ | ✅ | ❌ | ❌ |
| Lock screen | 💭 | ✅ | ✅ | ❌ | ❌ |
| Settings GUI | ➖ TUI + TOML | ✅ | ✅ | ❌ | ❌ |
| Dock, desktop widgets | 💭 | ➖ | ✅ | ❌ | ❌ |
| Screenshots, recording | ✅ native screenshots (screen, area, saved), a colour picker; annotation, OCR and recording through configurable tools (satty, tesseract, wf-recorder by default), stopped by alpheus | ➖ | ➖ | ❌ | ❌ |
| Idle and power management | ❌ left to hypridle (DisplayLink safety) | ✅ | ✅ | ✅ | ➖ |
| Shows on (layer-shell compositors: Hyprland, niri, sway, river, labwc…) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Workspaces and windows read from | Hyprland, Sway, Mango, niri | 7 compositors | 6 compositors | Hyprland, niri, sway, Mango | many (one module each) |
| Sway: workspaces, title, the launcher's windows | ✅ the i3 IPC on `$SWAYSOCK`, read only (GET_WORKSPACES, GET_TREE, SUBSCRIBE to workspace and window events, and with the keyboard layout one GET_INPUTS per session and the `input` and `mode` events), one re-read per burst | ✅ | ✅ | ✅ | ✅ |
| Mango: tags, title, the launcher's windows | ✅ Mango's own socket, read only (two `watch` lines), one `poll`: tags per output (active, urgent, occupied; empty ones hidden), the overview, a focus history for the launcher | ✅ | ❌⁴ | ✅ | ✅ |
| Extensions | script mode, no plugin API | ✅ | ✅ ~100 | ❌ | ➖ |

¹ waybar + mako + swayosd + fuzzel. ² ashell + swaync. ³ waybar + mako +
swayosd + swaybg, from PSS figures of different days (see the waybar stack note
in [measurements](measurements.md#memory)). Other shells' features from their documentation or source (October 2026);
the other shells' memory and alpheus's live figure were read on the author's laptop (alpheus's per-component rows are tests, see [measurements](measurements.md#method)). ⁴ No Mango backend found in Noctalia
(October 2026, unverified). — : not found in that shell's documentation (not verified).

