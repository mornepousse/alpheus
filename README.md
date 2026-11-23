# alpheus

> *Alpheus*: the pistol shrimp, tiny and the fastest in the ocean.

A **lightweight, terminal-spirited** Wayland desktop shell, written in Rust: a bar,
a launcher, notifications, an on-screen display, quick settings and a terminal control
center, what DankMaterialShell or Noctalia bring, **without their weight**. Aimed at
modest hardware with modern features, and light first: it keeps nothing resident that it
does not need and wakes up for nothing at rest. Mouse and keyboard everywhere.

![The alpheus bar, sample data, real font, default theme (catppuccin, dark) (zoomed ×2, split in two halves)](docs/images/bar.png)

![The quick settings panel: volume and output, brightness, microphone, do-not-disturb, power profile, keep-awake, night light, session buttons (fictional data, real font, default theme (catppuccin, dark), zoomed ×2)](docs/images/quick.png)

![The control center's Home dashboard at 87×26, catppuccin dark, sample data](docs/images/control-center-home.png)

## Status

A personal project, used daily by its author, who keeps adding to it; expect rough edges outside
that setup. It works under
Mango, Sway, Hyprland and niri ([what each compositor gives the bar](docs/compositors.md)).
It is not packaged for any distribution yet: it builds with Nix or Cargo
([install](docs/install.md)). Some defaults still call commands alpheus does not ship,
such as `wm-set` to switch a workspace or `shell-ctl` in the control center; those of the
bar and the launcher are configurable, and a missing command fails visibly and breaks
nothing else ([the contract](docs/compositors.md#wm-set)).

What it leaves to other tools: idle, sleep and screen power-off (hypridle, swayidle…), the lock screen
(a replacement is paused), file choosers and the wallpaper (swaybg works). alpheus never touches sleep
or screen power-off. On some outputs (DisplayLink in particular), a misplaced
power-off can freeze the machine.

## Memory

What a desktop shell keeps resident, measured as PSS on release builds
([how, and the tests that produce each figure](docs/measurements.md#method)). The rows are
copied from [`docs/measurements.md`](docs/measurements.md#memory), which has the rest,
component by component.

| Shell | Toolkit | Resident memory | What it covers |
|---|---|---:|---|
| **alpheus** (today, hybrid mode: the alpheus bar and sound source, plus the borrowed tools in the rows below) | Rust, no toolkit | **≈ 15 MB** (to be re-measured) | alpheus's live total ≈ 14 MB (bar and sound source) + swaybg 1.4 MB (optional) borrowed; swayosd (7.6 MB) is no longer used since the OSD replaced it, and mako (6.7 MB) since the notifications did; the live figure was measured before several deliveries and is to be re-measured |
| waybar + mako + swayosd + swaybg | GTK / C | ≈ 55 MB ([see the note](docs/measurements.md#alpheus-component-by-component)) | bar, notifications, OSD, wallpaper |
| HyprPanel | GJS | ≈ 151 MB | bar, notifications, OSD (Hyprland only) |
| Wayle | Rust, GTK4 | ≈ 168 MB | bar, panels |
| ashell + swaync | Rust, iced | ≈ 194 MB | bar, notifications |
| Noctalia 4.7 | Quickshell (QML) | ≈ 517 MB | full shell (v5, a native C++ rewrite released in September 2026, not measured yet) |
| DankMaterialShell (trimmed) | Quickshell (QML) | ≈ 512 MB | full shell, sound/visualizer/weather/temperatures off |
| DankMaterialShell | Quickshell (QML) | ≈ 537 MB | full shell |
| Caelestia | Quickshell (QML) | ≈ 730 MB | full shell |

| Component | Memory | Notes |
|---|---:|---|
| Bar daemon, all modules + launcher, 3 outputs — at start | ≈ 8.0 MB | headless sway test, `tests/memory.rs`, recorded D-Bus services on a private bus; libdbus shared with the session; tray unused: `startup_pss_per_source` ("everything", 3 runs: 8,144–8,246 KB); ≈ 8.4–8.5 MB with the tray and 10 fake items (`startup_pss_with_the_tray_under_15_mb`, 3 runs: 8,614–8,701 KB, without the launcher module) |
| Bar daemon — after 8,300 events and 5 plug cycles | ≈ 9.6–9.7 MB | stable, 0 wake-ups at rest, the theme watch adds no wake-up, 6 threads (D-Bus through dbus-rs/libdbus: one thread; the launcher and the tray add none) |

alpheus against DankMaterialShell, Noctalia, ashell and the waybar stack, feature by
feature: [`docs/comparison.md`](docs/comparison.md).

## What is in the box

Three programs from one Cargo workspace: `alpheus-demon` (the resident daemon), `alpheus-launch`
(a tiny client that talks to it from a key bind or a script) and `alpheus` (the control center).

- [**The bar**](docs/features/bar.md): workspaces, window title, media, sound, network, Bluetooth, battery,
  resources, tray, clock, one bar per screen; with the [tray](docs/features/tray.md),
  [calendar](docs/features/calendar.md), [media](docs/features/media.md),
  [resources](docs/features/resources.md), [timer](docs/features/timer.md),
  [privacy indicators](docs/features/privacy.md), [failed units](docs/features/failed.md)
  and the [⏻ menu](docs/features/power.md).
- [**The launcher**](docs/features/launcher.md): apps, windows, commands, a calculator, emoji, web search,
  a window switcher, your own command sources, and a `--dmenu` mode for scripts.
- [**The Tools menu**](docs/features/tools-menu.md): one box for "do something to my session".
- [**Notifications**](docs/features/notifications.md): alpheus's own server, bubbles, a bell, a history panel.
- [**The OSD**](docs/features/osd.md): a pill for volume, microphone and brightness.
- [**Quick settings**](docs/features/quick-settings.md): volume, brightness, Wi-Fi, Bluetooth, VPN,
  do-not-disturb, power profile, session buttons.
- [**The control center**](docs/features/control-center.md): a terminal program, eight tabs from Sound to Diag.
- [**Displays**](docs/features/displays.md): screen layouts and profiles, with a "keep these settings?" trial.
- [**Themes and bar styles**](docs/features/themes.md): fifteen presets or your wallpaper, light and dark, six bar shapes.
- [**Shield mode**](docs/features/shield.md): a public-network security mode, shown and switched by alpheus.
- [**Event hooks**](docs/features/hooks.md): a command of your own when something changes.
- [Clipboard history](docs/features/clipboard.md) (off unless enabled) and a [quick notepad](docs/features/notes.md).

Every key of every configuration file is in [`docs/configuration.md`](docs/configuration.md); the command
lines are in [`docs/cli.md`](docs/cli.md).

## Install

```sh
nix build .#alpheus-demon .#alpheus-launch .#alpheus   # or: cargo build --release
alpheus-demon                                          # from your compositor's startup
alpheus-launch --toggle                                # bind this to a key
alpheus --tab sound                                    # the control center, on a tab
```

Build and runtime dependencies, a flake input, and key binds for Hyprland, Sway, niri and Mango:
[`docs/install.md`](docs/install.md). Everything is optional, under `$XDG_CONFIG_HOME/alpheus/`
(`~/.config/alpheus/` by default).

## Configure

Four small TOML files, all optional, no message when one is missing: `bar.toml` (the daemon: modules,
font, per-module keys, hooks), `launcher.toml`, `theme.toml` (colours, mode, bar style) and `config.toml`
(the control center). The daemon reads `bar.toml` once at start and picks up a new `theme.toml` by itself.
A taste, a whole `theme.toml`:

```toml
# ~/.config/alpheus/theme.toml — every key optional
theme = "catppuccin"   # a preset, or "wallpaper"
mode  = "auto"         # "auto" (follow the desktop), "dark", "light"
module_colors = "icons" # "none", "icons" (default), "full", "pills"
style   = "flat"        # "islands", "powerline", "floating", "underline", "translucent"
opacity = 0.8           # translucent only
```

Every key, its default and its bounds: [`docs/configuration.md`](docs/configuration.md).

## Keys

alpheus binds no global key itself: your compositor runs `alpheus-launch`, a small client that sends the
daemon one verb. For example, in Hyprland (`hyprland.conf`):

```
bind = SUPER, space, exec, alpheus-launch --toggle
bind = SUPER, S, exec, alpheus-launch --quick
bind = SUPER, A, exec, foot --app-id alpheus-tui -e alpheus
```

`alpheus-launch` exits 3 when the daemon is not running, which lets a bind fall back to another
menu. Other verbs open the Tools menu, the window switcher, the calculator, the notepad or the clipboard
history, set do-not-disturb or drive the timer; `--dmenu` makes it a menu for your scripts. They are all
in [`docs/cli.md`](docs/cli.md), and binds for Sway, niri and Mango in [`docs/install.md`](docs/install.md).

## How this is built

alpheus is written with substantial help from an AI coding assistant (Claude Code), directed by its maintainer, who sets the scope, the guardrails and the budgets, decides what merges and runs it every day. Every change goes through tests written first and shown to fail without the change, guardrail tests, release memory and wake-up budgets, and automated reviews. See [CONTRIBUTING](CONTRIBUTING.md).

## Principles

- **Lightness first.** An unused option costs nothing; no heavy dependency
  "for config". Budgets are checked by tests (memory and idle wakeups of
  the bar, measured on a headless sway).
- **Never `dpms`** nor sleep handling: turning off a screen on a
  DisplayLink output can freeze the machine.
- **No direct writes to the compositor**: only configurable commands, with
  two exceptions, each in one file: the daemon sets the screens' layout through
  `zwlr_output_configuration_v1` (`outputs/wlr.rs`), and the clipboard history
  sets its own selection (`clipboard/control.rs`); never output power
  management, never `dpms`.
- **Tests without a graphical session**: every command goes through an
  injectable executor, no test acts on the session.

## Documentation

- [`docs/install.md`](docs/install.md): build, dependencies, starting the daemon, key binds.
- [`docs/configuration.md`](docs/configuration.md): every key of every file, its default and bounds.
- [`docs/cli.md`](docs/cli.md): the command lines of `alpheus`, `alpheus-demon` and `alpheus-launch`.
- [`docs/compositors.md`](docs/compositors.md): what each compositor gives the bar, and the external commands alpheus expects.
- [`docs/features/`](docs/features/README.md): one page per feature.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), [`docs/GLOSSARY.md`](docs/GLOSSARY.md), [`docs/tour.md`](docs/tour.md)
  (one change followed through the code) and [`docs/adding-a-module.md`](docs/adding-a-module.md): how it is built.
- [`docs/measurements.md`](docs/measurements.md) and [`docs/comparison.md`](docs/comparison.md): what it costs, and how it compares.
- [`docs/decisions.md`](docs/decisions.md), [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`SECURITY.md`](SECURITY.md):
  why it is the way it is, how to contribute, what it can do to your session.
- [`CHANGELOG.md`](CHANGELOG.md): what each delivery brought, newest first.
- [`docs/SCOPE.md`](docs/SCOPE.md): the original scope (historical).
- [`docs/specs/`](docs/specs/README.md): the design history, with a status index.

## License

[MIT](LICENSE) © 2026 Mae PUGIN. The launcher's emoji table is Unicode data under the
Unicode License v3: see [NOTICE](NOTICE).
