> **Historical document.** This is the original scope, written on 2026-10-02
> before any code existed, and kept as the record of why the project exists
> and what its guardrails are. Its breakdown and targets have since been
> overtaken (four compositors instead of two, the daemon is far lighter than
> the 30-50 MB first aimed at, sub-project 3's integration lives outside this
> repository). For what alpheus is today, read the [README](../README.md); for
> how it is built, [ARCHITECTURE](ARCHITECTURE.md); for the per-feature design
> record, [`specs/`](specs/README.md).

# alpheus — overall scope

Date: 2026-10-02 · Author: the project owner · Designed in session with Claude from
`~/nixos-config`.

> *Alpheus*: the pistol shrimp, tiny and the fastest in the ocean. The
> whole brief fits in the name.

## Why

A "homemade DMS": what DankMaterialShell brings (centralized settings, real
panels, notifications, toasts), **without its weight**, in a **TUI
spirit**, driven by **keyboard and mouse**.

### Measurements that led here (pc1, PSS ≈ 1 min after startup)

| Stack | RAM | Verdict |
|---|---|---|
| Old stack (waybar, mako, swayosd, swaybg) | ≈ 120 MB | light, but fuzzel menus too limited |
| HyprPanel (GJS) | ≈ 151 MB | "not great", Hyprland only, dispatch refused in Lua config |
| Wayle (Rust, GTK4) | ≈ 168 MB | "not thrilled" |
| ashell + swaync | ≈ 194 MB | "rough around the edges" |
| Trimmed-down DMS (sound, visualizer, weather, temperatures cut) | ≈ 512 MB | the weight comes from Quickshell, not the features |
| DMS (Quickshell) | ≈ 537 MB | liked, but too heavy |
| Caelestia (Quickshell) | ≈ 730 MB | nice-looking, too heavy |

Conclusion: every shell in DMS's class is built on Quickshell (~500 MB
floor); the lightweight alternatives aren't polished enough.

## Requirements (validated by the owner on 2026-10-02)

1. **Resident budget: 120 MB maximum** for EVERYTHING that stays in memory
   (bar, notifications, OSD, wallpaper). Whatever opens on demand (the TUI)
   doesn't count, but must be fast.
2. **Replace everything**: bar, notification toasts, OSD — eventually no
   more waybar, mako, swayosd.
3. **TUI spirit, both ways**: a minimalist bar with a terminal look
   (character grid, monospace) + TUI panels opened on demand.
4. **Mouse AND keyboard** everywhere, with **standard conventions** from
   TUIs and apps (command palette `Ctrl+P`, shortcut bar, Space toggles,
   Enter activates) — not vim habits. Expected to evolve with use.
5. **Rust.**
6. **Hyprland AND niri** (both sessions on the workstation).
7. **Nothing resident for comfort** beyond the budget: the TUI launches on
   demand and closes.

## Non-negotiable guardrails (inherited from nixos-config)

- **No sleep or screen-off handling whatsoever.** A `dpms off` on a
  DisplayLink output (evdi) freezes the machine; sleep stays with
  hypridle + `idle-dpms` (nixos-config). Observed: DMS and Caelestia arm
  `dpms off` by default — exactly what alpheus will never do.
- **Never write to the compositor directly**: everything goes through
  `wm-set` (nixos-config), which routes to Hyprland (Lua or hyprlang
  config) and niri. In Lua config, `hyprctl dispatch`/`keyword` no longer
  have hyprlang syntax. *Amended 2026-10-30 (displays spec § 2): the one
  exception is `zwlr_output_configuration_v1` in the daemon's
  `outputs/wlr.rs`, which changes the screens' layout (apply, trial,
  profiles on hotplug); no `dpms`, no power state, no command.*
- **Tests without a graphical session, never a real call** to tools that
  act on the session (fake command executor).

## Breakdown into sub-projects

Each has its own full cycle: spec → plan → implementation.

1. **TUI control center** (ratatui) — *v1 spec:*
   [`specs/2026-10-02-alpheus-v1-design.md`](specs/2026-10-02-alpheus-v1-design.md).
   Useful right away with the current stack (classic or dms), with no risk
   to the session. Sets the style and keybindings reused afterward.
2. **Resident daemon** (a single Rust process): bar + notification toasts
   (`org.freedesktop.Notifications`) + OSD. Direct Wayland rendering
   (smithay-client-toolkit + lightweight text rendering, character grid),
   **no GTK or iced** (ashell in iced: 150 MB on its own). Target
   30-50 MB, **to be validated by a measured prototype before committing**.
3. **Full NixOS integration**: `desktop.shell = "alpheus"` alongside
   `classic` and `dms`, `shell-ctl`, matugen theme, Hyprland + niri. The
   minimal wiring of sub-project 1's v1 (input, keybinding, window rule)
   happens as part of v1.

Wallpaper: swaybg (≈ 30 MB) stays in place; to be replaced only if the
budget demands it once the daemon is measured.
