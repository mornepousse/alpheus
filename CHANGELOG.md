# Changelog

All notable changes to alpheus, in the layout of [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Deliveries are listed under their version, by the month they were merged, newest first; what is
not released yet goes under a new *Unreleased* heading above the latest version. Each entry links to its feature page (or, when there is none, to the
code it touches). Costs are measured in release builds, back to back with the build before; the method and the
tables are in [docs/measurements.md](docs/measurements.md). The number in parentheses after a title is the day of the month the delivery merged.

## [Unreleased]

### 2026-11

#### Added

- **Keyboard backlight and airplane mode in quick settings** (27): a keyboard backlight slider right
  under the brightness, over the LED's own levels (← → one level, a step of `step` % above 10
  levels, Space or Enter cycle, 0 allowed), set with `brightnessctl --class=leds`; an airplane mode
  switch at the top of the radios that blocks
  Wi-Fi, Bluetooth and WWAN in one `rfkill` command and unblocks all three to leave. Both are read
  from sysfs when the panel opens (airplane mode again 300 ms after any radio command) and hold
  nothing while it is closed: no fd, thread or timer. New keys: `[quick] kbd_backlight`,
  `kbd_backlight_dir`, `airplane`, `rfkill_dir`. Cost against main: +24 KiB in the stripped binary,
  nothing measurable at start; the first open of the panel keeps +58…+72 KB more than main's, rows
  shown or not (the binary's code pages: the open path spans 16 more of them); both rows shown,
  +28…+34 KB PSS while the panel is open (+0…+8 KB anonymous), within noise once closed; 0
  wake-ups at rest.
  [Page](docs/features/quick-settings.md#what-it-does),
  [configuration](docs/configuration.md#quick),
  [decisions](docs/decisions.md#airplane-mode-blocks-wi-fi-bluetooth-and-wwan-through-rfkill-and-leaving-it-unblocks-all-three).

#### Changed

- **A `gpu_sensor` naming a card also picks its usage** (27): with `gpu_metric = "auto"`, a
  `[resources] gpu_sensor` naming a driver (`amdgpu`, `i915`, `xe`) now also picks that card for
  the GPU's usage, a hybrid laptop's discrete GPU included, where it used to choose the hwmon only.
  The card is read only while awake and at most once per its autosuspend delay and a second, so it
  is never kept from falling asleep.
  [Page](docs/features/resources.md),
  [configuration](docs/configuration.md#resources),
  [decisions](docs/decisions.md#a-gpus-usage-is-read-without-keeping-it-awake).
- **The Tools menu opens on a command panel** (27): `alpheus-launch --tools` (Super+X) shows a search
  field, tabs (All, with Pinned, Recent and every category as a section, then each category), each tool
  on two lines with what it does, a detail card with its actions and keys, and a letter that runs a
  tool in one key. A letter acts only on a tool whose capsule is drawn on
  screen (a tool scrolled out of the pane starts the search with its letter); `/` or Space
  start the search; Mod+Space never shows the panel nor has live letters. The search groups its
  results by category and shows fixed choices as chips; no chip is picked until ←/→, a click or the
  query picks one. New keys: `[tools] panel` (`false` keeps the category list) and `keys`, an entry's
  `key` and `description`. Cost against main, the same cycle on both sides: open +2.0…+2.2 MB PSS (main +1.1…+1.2), of which +1.8 MB is the larger box's pools (main +0.9); +0.25 MB at rest before any open; anonymous memory levels off over 60 cycles; 0 wake-ups at rest. [Page](docs/features/tools-menu.md#the-command-panel),
  [configuration](docs/configuration.md#tools-toolscategory-toolsentry).
- **Resident budget: 15 MB at rest** (27) instead of 120 MB; the memory suite holds the steady state to it.
  [Decision](docs/decisions.md#everything-alpheus-keeps-resident-fits-in-15-mb-at-rest).

## [0.1.0] - 2026-11-25

The first public release of alpheus, a light Wayland desktop shell written in Rust: a layer-shell
bar daemon with a launcher, notifications, an on-screen display and quick settings, and a terminal
control center, for Hyprland, Sway, niri and Mango. Every feature that is not enabled costs nothing.
It ships as the source, x86_64 Linux binaries built by CI and a Nix flake; everything below was
delivered before it, by the month it was merged.

### 2026-11

#### Added

- **Native captures** (25): `alpheus-launch --capture full|area|area-save|pick|annotate|ocr|record|record-area|stop`
  and a Captures category in the Tools menu: a screenshot of the focused output or of an area dragged
  over a live dim, copied through `copy` or saved to `[captures] dir`; a colour picker with a zoom lens
  over the frozen output; an area handed to an annotation editor (satty) or a text recognizer
  (tesseract), whose result alpheus copies; a recording of the output or an area through
  `[captures] record` (wf-recorder), stopped with SIGINT. The clipboard is touched only on success, a
  cancel is silent, and nothing stays resident after a capture. [Page](docs/features/captures.md),
  [configuration](docs/configuration.md#captures).
- **Privacy glyph while alpheus records** (25): the privacy module's screen glyph shows `alpheus`
  while its own recorder runs, which no other source sees on Sway or Mango. [Page](docs/features/privacy.md).
- **Kill list** (25): `alpheus-launch --kill`, or Tools › System › Kill a process, lists your
  own processes, by memory at open and by CPU 300 ms later; Enter asks one to quit, Ctrl+Enter then
  Enter kills it. The compositor, alpheus and the session's ancestors are never offered, and a
  reused pid is never signalled. [Page](docs/features/tools-menu.md#kill-a-process).
- **Scheduled shutdown** (25): Tools › System › Schedule shutdown or `alpheus-launch --timer shutdown
  <duration>` powers off through `[quick] poweroff`, beside any timer (`⏻ 1h20 · 󰔛 25m`), always after
  a one-minute warning that a click cancels, even when the end is noticed late;
  `--timer shutdown-cancel` cancels it. Nothing powers off while a screen locker runs: once the
  session is unlocked the warning is given fresh, a whole minute. A power off that cannot start or
  ends with an error shows a critical `Shutdown failed` notice. [Page](docs/features/timer.md#the-scheduled-shutdown).
- **Typed text in the Tools menu** (25): a `[[tools.entry]]` with `ask` asks for a text and passes it
  as the one whole argument `{text}`, never through a shell, never logged nor kept in the history.
  [Page](docs/features/tools-menu.md), [configuration](docs/configuration.md#tools-toolscategory-toolsentry).
- **Diagnostics tools** (25): a Diagnostics category in the Tools menu with ten nmap scans (a target
  typed in the box, except the subnet sweep), USB events, a USB reset (`[tools.builtin] usb_reset`)
  and the Diag tab's eight journal presets, opened by the control center's new `--preset <slug>`;
  the scans, USB events and USB reset run in the held terminal, and a preset opens the control
  center in `terminal`. [Page](docs/features/tools-menu.md#the-diagnostics-tools), [control
  center](docs/features/control-center.md).
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

- **glibc's mmap threshold pinned** (25): the daemon sets `M_MMAP_THRESHOLD` to 128 KiB at start, so a
  large block (a capture's frame) is unmapped when freed instead of staying in an arena; the launcher
  keeps 56 to 148 KB less after closing, and a user's `GLIBC_TUNABLES` threshold is overridden.
  [Decision](docs/decisions.md#large-blocks-are-always-mapped-on-their-own).
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

#### Fixed

- **System-bus signals addressed to the bar are dropped** (25): a signal with a destination reaches
  a client whatever its match rules say, and the bus's default policy lets any local user send one,
  so another user could forge a Bluetooth removal or a wake and make the bar re-read every
  service. Only a broadcast, which a sender-filtered rule matched, is heard now, as on the session
  bus. [Decision](docs/decisions.md#a-system-bus-signal-is-heard-only-when-it-is-a-broadcast).
- **A Bluetooth removal is confirmed** (25): the quick panel reads a device again when BlueZ says it
  left, and drops it only when it is really gone, so a forged or stray signal no longer hides a
  device or ends a pairing. [Decision](docs/decisions.md#a-bluetooth-removal-is-confirmed-by-a-read).

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
