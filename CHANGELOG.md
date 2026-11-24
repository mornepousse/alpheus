# Changelog

All notable changes to alpheus, in the layout of [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Deliveries are listed under their version, by the month they were merged, newest first; what is
not released yet goes under a new *Unreleased* heading above the latest version. Each entry links to its feature page (or, when there is none, to the
code it touches). Costs are measured in release builds, back to back with the build before; the method and the
tables are in [docs/measurements.md](docs/measurements.md). The number in parentheses after a title is the day of the month the delivery merged.

## [0.1.0] - unreleased

The first public release of alpheus, a light Wayland desktop shell written in Rust: a layer-shell
bar daemon with a launcher, notifications, an on-screen display and quick settings, and a terminal
control center, for Hyprland, Sway, niri and Mango. Every feature that is not enabled costs nothing.
It ships as the source, x86_64 Linux binaries built by CI and a Nix flake; everything below was
delivered before it, by the month it was merged.

### 2026-11

#### Added

- **Tools menu** (22): `alpheus-launch --tools` opens one launcher-style box of categories and
  entries to do something to the session, with your own entries from `launcher.toml`. [Page](docs/features/tools-menu.md).
- **Diagnostics** (22): a Diag tab in the control center (failed units, crash list, journal, a
  redacted bug report) and a `failed` bar module showing `󰀦 N` when systemd has failed units.
  [Page](docs/features/failed.md), [control center](docs/features/control-center.md).
- **Shield mode** (22): a public-network mode you switch from the bar or the quick panel, with
  blocked USB devices listed and allowed one by one; alpheus runs your commands, no root work.
  Nothing exists until configured. [Page](docs/features/shield.md).
- **Clipboard history** (22): a text history through data-control, opened by `alpheus-launch
  --clipboard` or the `"` prefix; off unless enabled. [Page](docs/features/clipboard.md).
- **Quick notes** (16): one pad of plain text on `alpheus-launch --notes`. [Page](docs/features/notes.md).
- **Timer** (16): a countdown, Pomodoro and stopwatch chip in the bar with a popup and an
  `alpheus-launch --timer` verb. [Page](docs/features/timer.md).
- **Resources popup** (16): a click on the resources module shows each figure with a graph of its
  last 60 samples; the right click now runs `[actions] resources`. [Page](docs/features/resources.md).
- **Event hooks** (16): `[hooks]` runs a command of your own when the theme mode, network, VPN,
  battery, privacy and other states change. [Page](docs/features/hooks.md).
- **VPN in the quick panel** (16): a VPN row and detail to bring a profile up or down; alpheus never
  handles a VPN secret. [Page](docs/features/quick-settings.md).
- **Small features** (15): a launcher calculator, timed do-not-disturb, notification rules, battery
  alerts, more resources items (`net`, `disk`, `gpu`) and a do-not-disturb duration detail in the quick
  panel. [Quick settings](docs/features/quick-settings.md), [notifications](docs/features/notifications.md),
  [launcher](docs/features/launcher.md), [resources](docs/features/resources.md).
- **Launcher 2** (15): `?` web search, `:` emoji and `$` shell-line prefixes, and an
  `alpheus-launch --windows` switcher. [Page](docs/features/launcher.md).
- **UI refresh** (15): six more theme presets (15 in all), a rail and a Home dashboard in the control
  center, and a calmer bar with an icon and a figure per module. [Themes](docs/features/themes.md),
  [control center](docs/features/control-center.md).
- **Displays through the output protocol** (3): the Displays tab and screen profiles are driven by
  the daemon's `zwlr_output_manager_v1` support, with a 15 s trial and profiles on hotplug, under
  every compositor that has the protocol. [Page](docs/features/displays.md).

#### Changed

- **smithay-client-toolkit 0.21** (24): the Wayland toolkit moves from 0.20; memory, binary size and
  wake-ups are unchanged. [Measurements](docs/measurements.md).
- **cosmic-text 0.19** (24): the bar's text library moves from 0.12 to 0.19, so the
  `rustybuzz` "unmaintained" advisory leaves the tree (`ttf-parser`'s stays, through `fontdb`); the
  daemon starts about 0.6 MB lighter and its binary grows by 280 KiB. [Measurements](docs/measurements.md).
- **Failed units, instant icon** (24): `[failed] subscribe` now defaults to `true`, so a failed unit
  shows at once; the cost lands on systemd and dbus-daemon, a signal per unit change. Set it to
  `false` to read the count on triggers only. [Page](docs/features/failed.md).
- **Control center session actions** (24): the Power tab's Lock, Log out, Reboot and Power off run
  `[quick]`'s commands from `bar.toml` (the bar's own, neutral defaults), no longer `hyprlock` and
  `wm-set exit`; an empty command hides the button. [Page](docs/features/control-center.md).
- **Open-source cleanup** (22-23): the daemon's code is organised in one folder per feature, a house
  standard ([docs/STYLE.md](docs/STYLE.md)) governs code comments and tests, and the documentation
  was rewritten: a page per feature, a tour, an architecture overview, the settled
  [decisions](docs/decisions.md), the configuration reference and the [measurements](docs/measurements.md).
  Behaviour is unchanged.
- The right click on the resources module runs `[actions] resources`; the left click opens the popup.
- **Log lines are fixed words** (24): a failed start of a command logs the entry's word and the error's
  kind (`not found`, `permission denied`, `io`), never the program, the argv or the OS message; the
  quick panel, the power menu, the resources popup and the module clicks follow it, as do the lock,
  the theme and Shield watches, the launcher's socket, the buffers, the media popup and player
  lines (no track, artist or player name) and the source threads. A source
  thread that panics logs `source down: panic` and no payload. A guardrail reads the whole daemon for
  it ([decisions](docs/decisions.md), "Logs carry fixed words and numbers only").

### 2026-10

#### Added

- **Quick settings, Bluetooth** (23): a Bluetooth row and detail to power the adapter, connect, pair
  (with passkey) and forget devices, and a headset profile switch. [Page](docs/features/quick-settings.md).
- **Privacy indicators** (20): `󰍬`, `󰄀` and `󰍹` when the microphone, a camera or the screen is
  captured, with the apps in a popup; a camera held outside PipeWire is seen too. The two parts cost
  −80 to +116 KB and −236 to +100 KB at start over the build before each. [Page](docs/features/privacy.md).
- **Quick settings, Wi-Fi** (13): a Wi-Fi row and a detail listing networks, connecting to saved, open
  and new WPA-personal ones. [Page](docs/features/quick-settings.md).
- **Mango and Sway backends** (13): the bar follows workspaces, the window title and the launcher's
  window list under Mango and Sway as under Hyprland and niri. [Compositors](docs/compositors.md).
- **Power button** (13): a `⏻` at the right end of the bar opens a Lock, Log out, Reboot and Power off
  menu; reboot and power off ask first. [Page](docs/features/power.md).
- **Quick settings** (11): a panel under the volume, network or Bluetooth module (or
  `alpheus-launch --quick`) with volume, brightness, microphone, do-not-disturb, power profile, keep
  awake, night light and session buttons. [Page](docs/features/quick-settings.md).
- **Launcher command sources** (10): `[[sources]]` in `launcher.toml` adds entries from your own
  commands to Mod+Space, and `alpheus-launch --dmenu --visible N` limits a menu's first rows.
  [Page](docs/features/launcher.md).
- **Notifications** (9): alpheus's own notification server with bubbles, a bell, a history panel and
  do-not-disturb. Linking it in cost +122 to +248 KB at start. [Page](docs/features/notifications.md).
- **On-screen display** (9): a pill that shows a volume, microphone or brightness change for 1.3 s.
  Linking it in cost −28 to +172 KB at start (within noise). [Page](docs/features/osd.md).
- **Media** (9): what is playing in the bar, and a popup to control the player. Linking it in cost
  −52 to +32 KB at start (within noise). [Page](docs/features/media.md).
- **Bar shapes** (8): `islands`, `powerline`, `floating`, `underline` and `translucent` styles next to
  `flat`. [Page](docs/features/themes.md).
- **Themes, colours** (7): 9 presets in dark and light, the wallpaper's colours, `theme.toml`, hot
  reload and module colours. [Page](docs/features/themes.md).
- **System tray and calendar** (7): status icons of running applications with their menus, and a
  month calendar under the clock. [Tray](docs/features/tray.md), [calendar](docs/features/calendar.md).
- **Launcher** (5): a centred box for apps, windows and `$PATH` commands, with frecency ranking, and
  `alpheus-launch` with a dmenu mode. [Page](docs/features/launcher.md).
- **Sound, network, Bluetooth, battery and resources modules** (5): the bar's system modules, fed by
  one `pw-dump` child, one read-only D-Bus connection and `/proc` and `/sys`. [Page](docs/features/bar.md).
- **The bar** (4): a layer-shell bar with workspaces, window title and clock on every output, under
  Hyprland and niri. Startup measured at about 12 MB PSS and no wake-up at rest. [Page](docs/features/bar.md).
- **Control center** (3): the terminal program (`alpheus`) with tabs for sound, network, Bluetooth,
  power, displays and more. [Page](docs/features/control-center.md).
