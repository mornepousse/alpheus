# Themes and bar styles

Colours come from a **theme**: one of fifteen compiled-in presets, or your
wallpaper through [matugen](https://github.com/InioX/matugen). Each has a dark
and a light variant. The mode follows the desktop (the portal's
`org.freedesktop.appearance` `color-scheme`, read by the bar, which passes it
on to the control center) or is fixed. The bar, its popups, the launcher and the
control center change together. The bar picks up a new
`theme.toml`, `colors.toml` or `colors-light.toml` by itself, without a
restart: one inotify watch, nothing wakes at rest. If `~/.config/alpheus` is
itself a symlink (home-manager), the watch follows it to its target, so
replacing the link is not seen until the bar restarts (the bar logs it once at start).

![The bar in Tokyo Night (dark), Catppuccin (light) and Gruvbox (dark), then the light theme's menu and calendar (sample data, zoomed ×2)](../images/themes.png)

The keys of `theme.toml` are in [`docs/configuration.md`](../configuration.md#themetoml).

With no `theme.toml`, or no `theme` key in it, alpheus uses `wallpaper` when
`~/.config/alpheus/colors.toml` exists,
and `catppuccin` otherwise. An unknown name falls back to `catppuccin`, with
one line in the log.

The fifteen presets are listed in [`docs/configuration.md`](../configuration.md#presets).

A preset with no light variant stays dark in light mode. Every preset is
checked for legibility in both modes by a test using WCAG contrast:
- text reaches 4.5:1 on the bar and the popups;
- secondary text and the state colours reach 3:1 on the bar;
- `fg` reaches 3.5:1 on the `raised` and `pill` fills, and the six presets added in the UI refresh keep secondary text and the module hues at 3:1 on `pill` (secondary text also on `raised`); where a hue or secondary text falls short on a fill, the control center draws that glyph or word in `fg` instead (a test checks every pair the rail and its tabs draw, in every preset and mode).

Where an official colour falls short in a light variant, alpheus uses a
darker shade of it. Nord's bar sits on `nord0` rather than `nord1`, so that
its red stays readable.

A theme is 16 colours, named by use; the roles are listed in [`docs/configuration.md`](../configuration.md#roles).

## Module colours

A theme also carries **seven hues**: `red`, `orange`, `yellow`, `green`,
`cyan`, `blue` and `purple`. Each preset takes them from its own palette
(darker shades in the light variants where the official one is too pale to
read, 3:1 on the bar at least). `module_colors` in `theme.toml` decides how
much of each module wears its hue; its values are in
[`docs/configuration.md`](../configuration.md#module_colors).

The mapping is fixed:

| Module | Hue |
|---|---|
| window title (its icon) | purple |
| media | purple |
| sound | blue |
| network | cyan |
| Bluetooth | purple |
| battery | green |
| CPU (its icon and figure) | orange |
| RAM | yellow |
| temperature | red |

Workspaces, the clock, the tray icons, the notification bell
(its count in the accent), the launcher button and the separators keep their colours. **States win over the hue**: a low (`warn`) or
critical (`error`) battery, a hot temperature, a network with limited
connectivity, a muted volume, an offline or
down module, an urgent workspace and a tray item that needs attention keep
their state colours whatever `module_colors` says. A charging battery is
green at any level (its glyph shows it charges, so it never warns); under `none` it
keeps the accent. The Theme tab of the
control center has a "Module colours" row (None, Icons, Full, Pills) with a preview.


## Bar styles

The bar has six shapes. `flat`, the default, is the bar as it always was.


Only `translucent` uses `opacity`; a value out of range still logs its warning under any other style.

The six values of `style` are described in [`docs/configuration.md`](../configuration.md#style).

![flat](../images/style-flat.png)
![islands](../images/style-islands.png)
![powerline](../images/style-powerline.png)
![floating](../images/style-floating.png)
![underline](../images/style-underline.png)
![translucent](../images/style-translucent.png)

*The six styles on catppuccin (dark), over a fictional wallpaper (sample data, real font, zoomed ×2, each bar split in two halves).*

The groups are the workspaces, the window title, the status modules (sound, network, Bluetooth,
battery), the resources, the tray, the clock and the launcher button. A module that is absent
makes no block. The text inside each block is the same in every style; only the background
changes.

`module_colors = "pills"` fills each block with the hue of its first module. Its text and icons
take whichever of the bar colour or the text colour reads better on that hue, and states still win
(a low battery stays `warn`). Pills apply to `islands`, `powerline`, `floating` and `translucent`.
Under `flat` and `underline`, `pills` looks like `full`.

![pills](../images/style-pills.png)

The Theme tab of the control center has a "Style" row with a preview of each style. The preview is
drawn in terminal cells, so it is an approximation, not the real pixels. `opacity` is set in the
file only. A change applies at once: `floating` moves the bar 6 px down and in from the sides, and
windows start right under it (6 px lower than under the other styles), then their usual gaps.

alpheus never asks for blur. On Hyprland, a layer rule on the bar's namespace blurs what is behind
the `translucent` bar, and behind the `islands` and `floating` blocks:

```
# hyprland.conf
layerrule = blur on, ignore_alpha 0, match:namespace alpheus-bar
```

Clicks in the transparent gaps between `islands` blocks still reach the bar. The shadow under
`floating` lets clicks through to the window below.

## Wallpaper colours (matugen)

`theme = "wallpaper"` reads the colour files matugen writes; the matugen template, its two configurations and the commands are in [`docs/configuration.md`](../configuration.md#wallpaper-colours-matugen).

## Known limits

- An empty `theme.toml` fixes the colour source: if `colors.toml` appears afterwards, it is not
  picked up until the file is edited or the daemon restarts.
- In solarized dark, the hues and the secondary text stay under 3:1 on the `raised` fill.
- In the Theme tab, each Enter that applies a theme shows its own toast, so several stack over the
  preview.
