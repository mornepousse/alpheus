# Configuration reference

Every key of every file alpheus reads, with its type, its default, the bounds the code enforces and
one sentence. The tables are checked against the configuration structs, not against other
documents: `crates/alpheus-daemon/src/config.rs` (`bar.toml`),
`crates/alpheus-daemon/src/launcher/config.rs` and `launcher/tools/config.rs` (`launcher.toml`),
`crates/alpheus-common/src/theme/settings.rs` (`theme.toml`),
`crates/alpheus-common/src/config.rs` (`config.toml`) and
`crates/alpheus-common/src/displays/profiles.rs` (`displays.toml`).

- [Where the files are and when they are read](#where-the-files-are-and-when-they-are-read)
- [`bar.toml`](#bartoml): the bar and everything else the daemon runs
- [`launcher.toml`](#launchertoml): the launcher and the Tools menu
- [`theme.toml`](#themetoml): colours, mode, bar style (and the wallpaper colour files)
- [`config.toml`](#configtoml): the control center
- [`displays.toml`](#displaystoml): screen profiles

## Where the files are and when they are read

Everything is optional, under `$XDG_CONFIG_HOME/alpheus/` (`~/.config/alpheus/` when the variable
is unset). A missing file means the defaults, with no message. An invalid file falls back to
default values with a warning, never a crash: `bar.toml`, `launcher.toml`, `config.toml` and
`displays.toml` reject unknown keys, so a single typo (or an unknown module name) discards that
whole file. `theme.toml` is lenient: an unknown key or a bad value is ignored with a warning and
the rest applies; unknown keys in `colors*.toml` are ignored.

Where a warning appears: for `bar.toml`, `launcher.toml`, `theme.toml` and `displays.toml` it goes to
the daemon's standard error (run it from a terminal, or read its journal, to see it); for
`config.toml` it shows on the control center's status line.

When the files are read: `bar.toml` **once, when the daemon starts** (restart `alpheus-demon`
after editing it); the theme files (`theme.toml`, `colors.toml`, `colors-light.toml`) are
**watched and reloaded live**; `launcher.toml` is read **at each open** of the launcher;
`config.toml` once, when the control center starts (restart it to apply an edit); `displays.toml`
when the control center starts, and by the daemon at the end of each hotplug debounce.

Only one `alpheus-demon` runs per session: a second one refuses to start (it logs `alpheus-demon
is already running` and exits). To apply a `bar.toml` change, stop the running `alpheus-demon` (for
example with `kill` and its pid), then start it again.

| File | Configures | Read |
|---|---|---|
| `bar.toml` | the bar, its modules, and the other daemon features (notifications, OSD, quick panel, timer, hooks, Shield…) | once, at the daemon's start |
| `launcher.toml` | the launcher (terminal command, result rows, history size, mode prefixes, the focus command, command sources) and the Tools menu; nothing kept while the launcher is closed | at each open of the launcher |
| `theme.toml` | the theme (a preset or the wallpaper), light/dark mode, per-role overrides, the bar style and its opacity | watched, live |
| `colors.toml`, `colors-light.toml` | the wallpaper's colours, written by matugen | watched, live |
| `config.toml` | the control center (keys, tools opened for sound, network, Bluetooth, the Diag tab) | once, at the control center's start |
| `displays.toml` | screen profiles, written by the Displays tab and applied by the daemon on hotplug (exact match of the connected screens, by `make model serial`, else description) | at the control center's start, and by the daemon at the end of each hotplug debounce |

The bar follows theme changes without a restart; the control center reads them when it opens.

Conventions in the tables below:

- A **command** is an argv list (`["program", "arg", …]`): the program, then its arguments, run as
  given, never through a shell (no `~` or `$VAR` expansion; a command that wants a shell says
  `["sh", "-c", "…"]`).
- "`[]` hides it" means an empty list removes the row or the action; "absent" means the key is not
  in the file.
- A bound is enforced by the code: a value outside it makes the file invalid (the whole file falls
  back to its defaults) unless the row says otherwise.
- Some defaults name commands alpheus does not ship (`wm-set`, `alpheus-toggle`).
  [compositors.md](compositors.md#external-commands) says what each should do, so that you can
  set your own. For example, `open_tab` can be
  `["foot", "--app-id", "alpheus-tui", "-e", "alpheus", "--tab", "{tab}"]`; for
  `[workspaces] command` and launcher.toml's `focus`, see the table of calls in compositors.md.

## `bar.toml`

Read once at the daemon's start. Unknown keys and unknown module names make the file invalid.

Sections: [top level](#top-level), [`[font]`](#font), [`[clock]`](#clock), [`[workspaces]`](#workspaces), [`[actions]`](#actions), [`[bindings.<module>]`](#bindingsmodule), [`[resources]`](#resources), [`[battery]`](#battery), [`[tray]`](#tray), [`[media]`](#media), [`[privacy]`](#privacy), [`[failed]`](#failed), [`[notifications]`](#notifications), [`[osd]`](#osd), [`[quick]`](#quick), [`[timer]`](#timer), [`[keyboard]`](#keyboard), [`[displays]`](#displays), [`[clipboard]`](#clipboard), [`[shield]`](#shield), [`[captures]`](#captures), [`[hooks]`](#hooks), [the example](#example).

### Top level

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `modules` | list of module names | every module below but `keyboard`, in this order | no duplicate; an unknown name is an error | The bar's modules, left to right, and which daemon features run at all. |
| `height` | integer (logical px) | `22` | 12 to 64 | The bar's height. |
| `tooltips` | bool | `true` | | A one-line tooltip after ½ s of hovering, on the modules and the tray items. `false` (here or as `[tray] tooltips`, its old spelling): no tooltip and no hover timer at all. |

The module names, in order: `launcher`, `workspaces`, `window`, `keyboard`, `media`, `privacy`,
`shield`, `failed`, `volume`, `network`, `bluetooth`, `battery`, `resources`, `tray`,
`notifications`, `osd`, `quick`, `displays`, `timer`, `clock`, `power`. `osd`, `quick` and
`displays` draw nothing in the bar (they are features with their own surface or behaviour); a
module that has nothing to show (`privacy`, `shield`, `failed`, `media` while idle) draws nothing.
`keyboard` is opt-in: the default list leaves it out, a `modules` list of your own adds it.
On a narrow screen secondary modules disappear; workspaces and the clock remain. A `modules` list of
your own must name every module you want, including those added since you wrote it (`privacy` after
`media`, `shield` after `privacy`, `failed` after `shield`, `timer` before `clock`).

### `[font]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `family` | string | `"JetBrainsMono Nerd Font Mono"` | not blank | The monospace family; absent from the system: another monospace one. |
| `size` | float (logical px) | `14.0` | 6 to 48 | The font size. |
| `fallback` | list of strings | `["DejaVu Sans Mono"]` | none | Families tried, in order, for characters `family` lacks (symbols, emoji, CJK); setting the key replaces the default (list DejaVu to keep it), `[]` disables fallback. A character with no glyph anywhere is masked, never drawn as a box. |

### `[clock]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `format` | string (strftime) | `"%H:%M"` | not blank, valid | The time, in bold accent. |
| `date` | string (strftime) | `"%a %e %b"` | valid; `""` = no date | The date, muted, before the time. |
| `calendar` | bool | `true` | | A left click on the clock opens the month. |
| `week_numbers` | bool | `false` | | ISO week numbers in the calendar. |

See [the calendar](features/calendar.md).

### `[workspaces]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `command` | command | `["wm-set", "workspace", "{workspace}", "{output}"]` | non-empty, non-blank program | Run to go to a workspace (a click or the wheel); `{workspace}` and `{output}` are replaced. `wm-set` is an external script, see [compositors.md](compositors.md#external-commands). |

### `[actions]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `open_tab` | command | `["alpheus-toggle", "{tab}"]` | non-empty, non-blank program | Opens the control center on a tab: a click on volume, network, Bluetooth, battery or `failed`; `{tab}` becomes `sound`, `network`, `bluetooth`, `home` (battery) or `diagnostics` (`failed`); no other module runs it. With the quick panel (`quick` in `modules`) a left click on volume, network or Bluetooth opens the panel instead, and `open_tab` runs on the right click. The Tools menu's control-center entries use it too. `alpheus-toggle` is an external script, see [compositors.md](compositors.md#external-commands). |
| `resources` | command | `["foot", "-e", "btop"]` | non-empty, non-blank program | A system monitor: the right click on the resources module, and the popup's `Monitor…` row (a left click opens the popup); with it empty the popup has no `Monitor…` row. |

### `[bindings.<module>]`

One table per module binds its buttons to a command or a built-in verb; a button left out keeps
today's behaviour, and no table at all changes nothing.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `left`, `middle`, `right` | command or verb | absent | The mouse button on the module. |
| `wheel_up`, `wheel_down` | command or verb | absent | One wheel notch over the module. |

- **Modules**: every module that draws a segment, except `workspaces` and `tray` (their zones are
  per item): `launcher`, `window`, `media`, `privacy`, `shield`, `failed`, `volume`, `network`,
  `bluetooth`, `battery`, `resources`, `notifications`, `timer`, `clock`, `power`. `osd`, `quick`,
  `displays`, `workspaces`, `tray` or an unknown name is an error. A table for a module not in
  `modules` is accepted and costs nothing.
- **A command** is an argument list run as written (no shell, no `~` or `$VAR` expansion): 1 to 16
  items, a non-blank program, no NUL. `{tab}` (the module's control-center tab; only on volume,
  network, Bluetooth, battery and `failed`) and `{output}` (the bar's connector, such as `DP-1`) are
  replaced, each a whole argument, at most once, never the program. `[]` is an error: use `"none"`.
  `suspend`, `hibernate` or `hybrid-sleep` as a whole word in any item, whatever its case, is an
  error (a binding never puts the machine to sleep). Beyond those checks a binding runs exactly what
  you wrote, and what it does is yours: alpheus does not vet it.
- **A verb** is a string:

| Verb | Does | Needs |
|---|---|---|
| `"none"` | Nothing: the button's default silenced. | |
| `"popup"` | The module's own popup. | A module with a popup (`media`, `privacy`, `shield`, `volume`, `network`, `bluetooth`, `resources`, `notifications`, `timer`, `clock`, `power`); `quick` in `modules` when that popup is the quick panel (`shield`, volume, network, Bluetooth). |
| `"quick"` | The quick settings panel. | `quick` in `modules`. |
| `"open_tab"` | `[actions] open_tab` with the module's tab. | A module with a tab (volume, network, Bluetooth, battery, `failed`). |
| `"launcher"` | Toggles the launcher, as its button does. | |
| `"dnd"` | Toggles do not disturb. | `notifications` in `modules`. |
| `"mute"` | Toggles the default output's mute. | |

An invalid table makes the whole file fall back to the defaults, with one log line naming the key
(`bindings.volume.right`), never the command.

```toml
[bindings.volume]
right = ["pavucontrol"]   # its left click still opens the quick panel
wheel_up = "none"
[bindings.clock]
middle = ["foot", "-e", "calcurse"]
```

### `[resources]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `interval` | integer (seconds) | `2` | 1 to 60 | Seconds between two readings of `/proc` (the popup's graph spans 60 × `interval`). |
| `graph` | integer | `20` | 0 to 60 | Ignored; accepted so older files stay valid. |
| `sensor` | string | `""` | | An hwmon name or a thermal zone type for the temperature; `""`: automatic. |
| `show` | list | `["cpu", "memory", "temperature"]` | not empty, no repeat; items `cpu`, `memory`, `temperature`, `net`, `disk`, `gpu` | The figures shown, in this order. `net` is the IPv4 default route's interface, or `net_interface` (receive and send rates), `disk` is each `disk_mounts` path's filesystem, else `$HOME`'s (percent used, rounded up like `df`; the bar shows the fullest, the popup one block per path; `statvfs` on the event loop every `disk_interval` seconds; a listed path on a network filesystem is never read and shows `—`), `gpu` is the GPU's usage (amdgpu's busy share, with its VRAM in the popup; Intel's estimate from idle residency, `active` in the popup) or its hwmon temperature, per `gpu_metric`. Nothing is read for an item left out. |
| `gpu_sensor` | string | `""` | | A GPU hwmon name (`amdgpu`, `nouveau`, `i915`, `xe`, `nvidia`); `""`: automatic. Also selects the card read for usage by its driver (`amdgpu`, `i915`, `xe`); `""`: among the cards driving the panel (`device/boot_vga` is 1), the first `amdgpu`, then `i915`, then `xe` (a DisplayLink `evdi` card or NVIDIA's proprietary driver is never picked); a hybrid laptop's discrete GPU is read only when named here. Reading a GPU's usage or hwmon marks it busy and can keep it from falling asleep: its usage files are never read unless it is awake (`runtime_status` active), and at most once per its autosuspend delay and a second, about every 11 s on Intel and 6 s on amdgpu (the last figures repeat in between; in `both` the temperature is that same card's own hwmon, read likewise, and in `temp` a secondary card named here is read likewise, even one plugged in after start; docs/decisions.md, "A GPU's usage is read without keeping it awake"). NVIDIA's proprietary driver offers neither: `gpu` shows `—` in the popup and nothing on the bar. |
| `gpu_metric` | string | `"auto"` | `auto`, `usage`, `temp`, `both` | What `gpu` shows: `usage` the card's busy percent (`󰢮 23%`; VRAM in use, dim, in the popup), `temp` its hwmon temperature (`󰢮 54°`), `both` the two (`󰢮 23% 54°`); `auto` reads usage when a card offering it is found at start, else the temperature. Decided at start; only what it names is read. Usage comes from sysfs: amdgpu's busy share, or on Intel (i915, xe) an estimate from idle residency, 100 minus the idle share since the previous reading, shown after a dim `active` in the popup: not engine load (time out of the idle state), and the first figure needs two readings, one autosuspend delay and a second apart. The xe path follows the kernel's driver; it is not checked on hardware. |
| `net_interface` | string | `""` | at most 15 bytes, no `/`, `:` or space | The interface `net` follows (`wlan0`, `enp3s0`…); `""`: the IPv4 default route's. Named, `/proc/net/route` is never read; absent from `/proc/net/dev`, `net` shows no figure. |
| `disk_mounts` | list of paths | `[]` | at most 4, absolute, no repeat, no `..` | The paths whose filesystems `disk` measures, in order (`["/", "/fictional/data"]`); `[]`: `$HOME`'s. A path that cannot be read shows `—` in its block. Each is `statvfs`-ed on the event loop, except a path on a network filesystem (`nfs`, `nfs4`, `cifs`, `smb3`, `smbfs`, `sshfs`, `9p`, `ceph`, `glusterfs`, `davfs`, `afs`, `lustre`, an untyped `fuse`, and every `fuse.*` type but the local `fuse.gocryptfs`, `fuse.cryfs` and `fuse.encfs`) or on an automount point (`autofs`, which a `statvfs` would mount there and then), which is never read and shows `—`, so a server that went away can never block the bar; a local automount (systemd's `/boot`) shows `—` too. The type is read once at start from `/proc/self/mountinfo` (the mount with the longest mount point containing the path, matched by its text: a symlink to a network mount is not followed); a share mounted later is found at the next start. `$HOME` (an empty list) is read whatever it lies on. A config error names the entry's position, never its path. |
| `disk_interval` | integer (seconds) | `15` | 1 to 300 | Seconds between two readings of the disks: the first pass reads them, then once this long has passed; in between, every pass repeats the last figures (the graph has no gaps). |

See [the resources popup](features/resources.md).

### `[battery]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `peripherals` | `"always"`, `"low"` or `"never"` | `"always"` | | Peripherals that report a real percentage. |
| `coarse` | `"always"`, `"low"` or `"never"` | `"low"` | | Peripherals that report only a coarse level (`"low"`: shown only when low, critical or action). |
| `warn` | list of integers (percent) | `[20, 10, 5]` | each 1 to 99; sorted descending and de-duplicated | A notification when the discharging laptop battery crosses one; the lowest is critical; `[]` turns it off. Needs `battery` and `notifications` in `modules`; read at start. |

### `[tray]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `passive` | `"dim"`, `"show"` or `"hide"` | `"dim"` | | How an item that says it is idle is drawn. |
| `tooltips` | bool | `true` | | The old spelling of the top-level `tooltips`: `false` here turns every tooltip off, the modules' too. |

`[tray.icons]` (glyph overrides): a table of `name = "glyph"`. A key is matched, lower-cased, as a
substring of the item's id, icon name, title or process name; at most 128 entries, no blank key,
no blanks or control character in a key, no two keys that differ only by case, each glyph one or two cells wide
with no control character. See [the tray](features/tray.md).

### `[media]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `title_max` | integer (cells) | `30` | 8 to 80 | Width of the title in the bar, `…` included. |
| `ignore` | list of strings | `["kdeconnect"]` | | Substrings of a player's bus name: that player is never queried. `playerctld` is always ignored. |

See [the media module](features/media.md).

### `[privacy]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `ignore` | list of strings | `[]` | | App names (whole name, any case) never shown; an indicator left with no name and nothing unnamed goes off. |
| `dev_dir` | absolute path | `"/dev"` | absolute | Where video devices are listed (watched, never opened). |
| `proc_dir` | absolute path | `"/proc"` | absolute | Where processes are listed (the camera scan). |

Only with `privacy` in `modules`. See [the privacy indicators](features/privacy.md).

### `[failed]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `user_units` | bool | `true` | | Also count the user manager's failed units (the session bus); `false`: that bus is never touched. |
| `subscribe` | bool | `true` | | `true`: call `Subscribe()` on each manager so a change arrives at once (the cost lands on systemd and dbus-daemon, a signal per unit change); `false`: read at start, on resume, on a refresh and piggybacked on another domain's wake-up (at most once a minute). |
| `notify` | bool | `false` | | One notification when the total rises (needs `notifications` in `modules`); re-armed at 0. |

See [the failed-units count](features/failed.md).

### `[notifications]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `timeout` | integer (seconds) | `5` | `timeout_min` ≤ `timeout` ≤ `timeout_max` | How long a bubble stays when the app sends no timeout (−1). A critical notification stays until dismissed. |
| `timeout_min` | integer (seconds) | `2` | 1 ≤ `timeout_min` ≤ `timeout` | An app's own timeout is raised to at least this. |
| `timeout_max` | integer (seconds) | `20` | `timeout` ≤ `timeout_max` ≤ 600 | An app's own timeout is cut to at most this; an app asking 0 (never expire) gets `timeout_max`. |
| `max_visible` | integer | `4` | 1 to 8 | Bubbles shown at once; the others wait. |
| `width` | integer (cells) | `40` | 24 to 80 | A bubble's width. |
| `history` | integer | `50` | 0 to 200 | Entries kept in memory for the panel (0: none). |
| `dnd_while_sharing` | bool | `false` | | Do not disturb while a screen capture runs (needs `privacy` and `notifications` in `modules`); a change you make meanwhile wins. |

`[[notifications.rules]]`: per app, the first match wins, **at most 32**.

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `app` | string | required | 1 to 64 bytes after trimming, no control or invisible character | The app's whole name, any case. |
| `action` | `"mute"` or `"drop"` | absent | | `mute`: no bubble, kept in the history and counted unread; `drop`: ignored entirely (the sender is answered as usual, nothing stored). |
| `urgency` | `"low"`, `"normal"` or `"critical"` | absent | not with `drop` | Replaces the app's own urgency. |

A rule needs `action`, `urgency` or both. See [notifications](features/notifications.md).

### `[osd]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `timeout_ms` | integer (ms) | `1300` | 600 to 5000 | How long the pill stays after the last change. |
| `width` | integer (cells) | `32` | 24 to 60 | The pill's row. |
| `margin` | integer (px) | `48` | 0 to 400 | Distance above the output's bottom edge. |
| `backlight` | string | `""` | a plain device name (no `/`, at most 64 bytes) | A device in `backlight_dir`; `""`: the first one, in sorted order. |
| `backlight_dir` | absolute path | `"/sys/class/backlight"` | absolute | Where backlight devices are listed; read only, on `alpheus-launch --osd brightness` and when the quick panel opens. |
| `kinds` | list of `"volume"`, `"mic"`, `"brightness"`, `"bluetooth"`, `"dnd"`, `"profile"`, `"layout"`, `"track"` | `["volume", "mic", "brightness"]` | each once; `"dnd"` needs `"notifications"` in `modules` | Which pills show. Each kind brings the source it reads with `osd` in `modules`: `volume` and `mic` `pw-dump`, `bluetooth` BlueZ, `profile` power-profiles-daemon, `track` the MPRIS players, `layout` the compositor's keyboard layout (without the `keyboard` module); without `volume` and `mic` the OSD starts no `pw-dump`, without `brightness` the verb shows nothing. `layout` shows the layout's full name; `track` shows the new title while playing, on screen only, never logged. |

See [the on-screen display](features/osd.md).

### `[quick]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `step` | integer (percent) | `5` | 1 to 25 | Percent per ←/→ or wheel notch on a slider. |
| `keep_awake` | inline table `{ toggle, state }` | `{ toggle = [], state = "" }` | see below | The keep-awake row. |
| `night_light` | inline table `{ toggle, state }` | `{ toggle = [], state = "" }` | see below | The night light row. |
| `lock` | command | `["loginctl", "lock-session"]` | non-blank program, no NUL | The Lock button and the ⏻ menu's entry; `[]` hides it. |
| `logout` | command | `[]` | same | Log out; empty by default, so it shows only once you set it. |
| `reboot` | command | `["systemctl", "reboot"]` | same | Reboot (asks once more). |
| `poweroff` | command | `["systemctl", "poweroff"]` | same | Power off (asks once more). |
| `wifi_more` | command | launcher.toml's `terminal` + `nmtui` | same | `More…` in the Wi-Fi detail; absent: the default; `[]` hides it. |
| `vpn_more` | command | launcher.toml's `terminal` + `nmtui-edit` | same | `More…` in the VPN detail; absent: the default; `[]` hides it. |
| `mixer` | command | launcher.toml's `terminal` + `wiremix` | same | `Mixer…` under the microphone; absent: the default (not checked against `$PATH`: without wiremix its terminal closes at once); `[]` hides it. |
| `dnd_until` | string `HH:MM` | `"08:00"` | a local time 0:00 to 23:59 | The do-not-disturb detail's "Until" entry (tomorrow if already past). |
| `kbd_backlight` | bool | `true` | | The keyboard backlight row; still hidden without a `*::kbd_backlight` LED. `false`: nothing is read when the panel opens. |
| `kbd_backlight_dir` | path | `"/sys/class/leds"` | absolute | Where LEDs are listed; read when the panel opens (and 150 ms after its command), never written. |
| `airplane` | bool | `true` | | The airplane mode row; still hidden without a Wi-Fi, Bluetooth or WWAN radio. `false`: nothing is read when the panel opens. |
| `rfkill_dir` | path | `"/sys/class/rfkill"` | absolute | Where rfkill switches are listed; read when the panel opens (and 300 ms after a radio command), never written. |

A toggle's `toggle` is a command run as given (`[]` hides the row); its `state` is `""` (unknown,
the row shows `·`), `"file:<path>"` (on while the file exists; the path starts with `/`, `~/` or
`$XDG_RUNTIME_DIR/`) or `"process:<name>"` (on while a process with that name runs; 1 to 15 bytes,
no `/`). A `state` without a `toggle` is refused. A row whose tool, device or command is missing
does not show. Nothing in `[quick]` handles sleep or screen-off. Reboot and power off ask once more;
the ⏻ menu, the quick panel and the control center's Power tab share `lock`, `logout`, `reboot` and
`poweroff` (the control center reads them from this file, once at its start; an empty one hides the
button there too). See
[the quick settings panel](features/quick-settings.md) and [the power menu](features/power.md).

### `[timer]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `countdown` | integer (minutes) | `10` | 1 to 180 | The popup's Timer row at start. |
| `work` | integer (minutes) | `25` | 1 to 180 | Pomodoro work length. |
| `short_break` | integer (minutes) | `5` | 1 to 180 | Pomodoro short break. |
| `long_break` | integer (minutes) | `15` | 1 to 180 | Pomodoro long break. |
| `rounds` | integer | `4` | 1 to 12 | Work sessions before the long break. |
| `sound` | command | `[]` | non-blank program, no NUL | Run at each end (not under do-not-disturb), for example `["pw-play", "/path/bell.oga"]`; `[]`: none. |
| `idle` | `"glyph"` or `"hidden"` | `"glyph"` | | The chip when nothing runs: a dim glyph, or nothing drawn. |
| `proc_dir` | absolute path | `"/proc"` | absolute | Where the scheduled shutdown lists processes to tell a locked session (a screen locker running). Only a test changes it. |

See [the focus timer](features/timer.md).

### `[keyboard]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `short` | bool | `true` | | The layout's short name: the 2 or 3 ASCII letters in its parentheses (`English (US)` → `us`), else its first two characters, lower case; `false`: the full name as the compositor gives it. |
| `show_mode` | bool | `true` | | The compositor's bind mode after the layout, in the accent, when it is not `default`. |

Read only with `keyboard` in `modules`. See [the keyboard module](features/keyboard.md).

### `[displays]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `auto` | bool | `true` | | Apply the `displays.toml` profile whose screens are exactly the connected ones, 500 ms after they change; `false` keeps the Displays tab and drops the hotplug behaviour. |
| `drm_dir` | absolute path | `"/sys/class/drm"` | absolute | Where DRM connectors are listed: a card driven by `evdi` marks a DisplayLink screen (120 Hz cap). Read, never watched. |

See [displays](features/displays.md) and [`displays.toml`](#displaystoml).

### `[clipboard]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `enabled` | bool | `false` | | `true`: follow the Wayland clipboard (data-control) and keep a text history in memory, 50 entries of at most 4,096 bytes, never on disk, never a copy with a password-manager hint. Read at start. |

The history is opened with `alpheus-launch --clipboard` or the `"` prefix; [Clipboard history](features/clipboard.md)
says more.

### `[shield]`

Shield mode: with none of `flag`, `on` and `off` set, no Shield exists anywhere (no watch, glyph or
row; every `--shield` verb exits 2). **Configured** means `flag`, `on` and `off` are all set. Read
at start.

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `flag` | absolute path | `""` | absolute, a file (no trailing `/`), no NUL | The mode is on while this file exists; keep it in a directory of its own, not alpheus's config directory. |
| `on` | command | `[]` | non-blank program, no NUL | Switches the mode on; its program asks for authorisation itself. |
| `off` | command | `[]` | same | Switches it off. |
| `allow` | command | `[]` | exactly one `{id}` element, whole | Allows a blocked USB device; `{id}` is replaced once, as a whole element. Absent: the blocked devices are listed only. |
| `blocked` | command | `["usbguard", "list-devices", "--blocked"]` | same program rules | Prints USBGuard's blocked-device lines; `[]`: no device list. |
| `on_text` | string | `"Protections for public networks are on."` | at most 256 bytes, no control or format character | The "on" notice's body. |
| `off_text` | string | `"Back to the usual settings."` | same | The "off" notice's body. |

See [Shield mode](features/shield.md).

### `[captures]`

Screenshots and recordings, driven by `alpheus-launch --capture` and the Tools menu's Captures category. Read at start. The copy command is `launcher.toml`'s `copy`.

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `dir` | path | `""` | absolute or `~/…`, no NUL | Where `area-save` writes `%Y%m%d-%H%M%S.png`. Empty: `$XDG_PICTURES_DIR/screenshots` (else the pictures entry of `user-dirs.dirs`), else `~/Pictures/screenshots`. |
| `annotate` | command | `["satty", "--filename", "{file}", "--output-filename", "{out}", "--early-exit", "--copy-command", "cat >/dev/null", "--save-after-copy", "--disable-notifications"]` | non-blank program, no NUL; `{file}` and `{out}` once each, whole arguments, not the program; `[]` turns it off | The annotation tool of `--capture annotate`: `{file}` is the area's private PNG, `{out}` the private PNG it writes back, which alpheus copies (exit 0 with nothing written: a silent cancel; another exit: critical). The default flags make satty's own copy a save to `{out}` (its copy command reads the image and throws it away: a command that does not read its input breaks satty's pipe and nothing is saved), so only alpheus copies. |
| `ocr` | command | `["tesseract", "{file}", "-", "-l", "eng"]` | same rules; `{file}` once, no `{out}` | The text recognizer of `--capture ocr`: its standard output, trimmed, is copied as text (empty: a silent cancel; another exit than 0, or more than 256 KiB: critical). |
| `record` | command | `["wf-recorder", "-g", "{geometry}", "-f", "{out}"]` | same rules; `{out}` once, `{geometry}` at most once, no `{file}` | The recorder of `--capture record` and `record-area`: `{geometry}` is `x,y wxh` in the compositor's logical pixels (the whole focused output for `record`, the area for `record-area`), `{out}` a free `screenrecord-%Y%m%d-%H%M%S.mp4` in `$XDG_VIDEOS_DIR` (else `user-dirs.dirs`, else `~/Videos`). `--capture stop` sends SIGINT to its process group; exit 0 with a non-empty file is saved. The recorder must stay in that group (no `setsid`): a program that leaves it gets the signal alone, without what it started. Without `{geometry}` the area is not passed: `record-area` records whatever the tool picks (wf-recorder with several outputs and neither `-g` nor `-o` asks which output on its standard input, which is `/dev/null`). |
| `pick_format` | string | `"{hex}"` | at least one placeholder, only `{r}`, `{g}`, `{b}`, `{hex}` between braces, no control character, at most 256 bytes | The colour picker's copied text: `{r}`, `{g}`, `{b}` are 0 to 255, `{hex}` is `#RRGGBB` in upper case. Any other `{…}` is refused, so the text cannot hold a literal brace pair. |

### `[hooks]`

A command per event, its data in `ALPHEUS_*` variables (never as arguments). Absent or `[]`: no
hook; with none set, nothing runs. Each value is a command with a non-blank program and no NUL.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `started` | command | `[]` | Once, when the daemon has started its sources. |
| `theme_mode` | command | `[]` | The mode in effect changes. |
| `dnd` | command | `[]` | Do-not-disturb turns on or off. |
| `battery_charge` | command | `[]` | The laptop battery's charge state changes. |
| `battery_level` | command | `[]` | The battery's whole percentage changes. |
| `power_profile` | command | `[]` | The power profile changes. |
| `network` | command | `[]` | The primary connection goes online or offline, or changes kind. |
| `vpn` | command | `[]` | A VPN becomes active, or the last one goes. |
| `bluetooth` | command | `[]` | The adapter powers on or off, or a device connects or disconnects. |
| `privacy` | command | `[]` | The microphone, camera or screen indicator turns on or off. |
| `shield` | command | `[]` | Shield mode turns on or off. |
| `failed_units` | command | `[]` | The failed-units total changes. |
| `displays` | command | `[]` | A `displays.toml` profile was applied on hotplug. |

The variables each event sets, the modules each needs in `modules`, the rate limit and the other
rules are in [event hooks](features/hooks.md).

### Example

The default values, with the optional keys commented out (every key is optional; the example is
valid as written):

```toml
# bar.toml — default values
modules = ["launcher", "workspaces", "window", "media", "privacy", "shield", "failed", "volume", "network",
           "bluetooth", "battery", "resources", "tray", "notifications", "osd", "quick", "displays", "timer", "clock", "power"]
height = 22
tooltips = true      # a one-line tooltip after ½ s of hovering, modules and tray; false: no hover timer at all

[font]
family = "JetBrainsMono Nerd Font Mono"
size = 14.0
fallback = ["DejaVu Sans Mono"]  # for symbols the font lacks (✳ ◐…); setting the key replaces this default, [] disables it

[clock]
format = "%H:%M"
date = "%a %e %b"   # empty = no date
calendar = true     # a left click on the clock opens the month
week_numbers = false  # ISO week numbers in the calendar

[workspaces]
command = ["wm-set", "workspace", "{workspace}", "{output}"]

[actions]
open_tab = ["alpheus-toggle", "{tab}"]   # a click on volume, network, Bluetooth, battery or failed: {tab} becomes sound, network, bluetooth, home or diagnostics
resources = ["foot", "-e", "btop"]       # a right click on the resources module, and the popup's Monitor… row (a left click opens the popup)
# {tab} is "sound" for volume, "network" for network, "bluetooth" for Bluetooth, "home" for battery and "diagnostics" for failed; no other module runs open_tab.
# With the quick panel (`quick` in modules) a left click on volume, network or Bluetooth opens the panel instead, and open_tab runs on the right click.

# [bindings.volume]       # one table per module; a button left out keeps today's behaviour
# right = ["pavucontrol"] # left, middle, right, wheel_up, wheel_down: an argv ({tab}, {output} replaced) or a verb
# wheel_up = "none"       # verbs: none, popup, quick, open_tab, launcher, dnd, mute

[resources]
interval = 2         # seconds between two readings of /proc (1 to 60)
graph = 20           # ignored; accepted so older files stay valid
sensor = ""          # an hwmon name or a thermal zone type; "": automatic
show = ["cpu", "memory", "temperature"]   # in this order; also "net", "disk" (disk_mounts, else $HOME's filesystem; statvfs on the event loop), "gpu"
gpu_sensor = ""      # a GPU hwmon name (amdgpu, nouveau, i915, xe, nvidia), also the card's driver for usage; "": automatic. A discrete GPU's hwmon can wake it on a hybrid laptop: name the iGPU's driver
gpu_metric = "auto"  # "auto" | "usage" | "temp" | "both": auto shows usage when a card offers it (amdgpu, i915, xe), else the temperature; NVIDIA proprietary shows nothing
net_interface = ""   # the interface "net" follows (wlan0, enp3s0…); "": the IPv4 default route's
disk_mounts = []     # up to 4 absolute paths "disk" measures, in order; []: $HOME's filesystem; a path on a network filesystem (nfs, cifs, sshfs…) or an automount shows —
disk_interval = 15   # seconds between disk readings, 1 to 300; the last figures repeat in between

[battery]
peripherals = "always"   # "always" | "low" | "never": peripherals with a real percentage
coarse = "low"           # "always" | "low" | "never": peripherals with only a coarse level
warn = [20, 10, 5]       # a notification when the battery, discharging, crosses one (1 to 99); the lowest is critical; []: none

[tray]
passive = "dim"     # "dim" | "show" | "hide": items that say they are idle
# tooltips = true   # the old spelling of the top-level tooltips: false here turns every tooltip off

[tray.icons]        # glyph overrides, keyed like the built-in table (id, icon name, title, process)
# keepassxc = "󰌋"

[media]
title_max = 30            # cells of the title in the bar (8 to 80)
ignore = ["kdeconnect"]   # bus-name substrings never queried; playerctld is always ignored

[privacy]
ignore = []               # app names (whole name, any case) never shown; an indicator left with no name and nothing unnamed goes off
# dev_dir = "/dev"        # where video devices are listed (watched, never opened)
# proc_dir = "/proc"      # where processes are listed (the camera scan)

[failed]                                     # the failed-units count (the `failed` module)
user_units = true    # also count the user manager's failed units (the session bus); false: that bus is never touched
subscribe = true     # true (default): call Subscribe() on each manager so a change arrives at once; false: read at start, on resume, on a refresh and piggybacked (at most once a minute)
notify = false       # one notification when the total rises (needs "notifications" in modules); re-armed at 0

[notifications]
timeout = 5          # seconds, when the app leaves it to the server
timeout_min = 2      # an app's own timeout is kept within these
timeout_max = 20
max_visible = 4      # bubbles shown at once; more wait
width = 40           # cells (24 to 80)
history = 50         # entries kept in memory (0 to 200)
dnd_while_sharing = false   # true: do not disturb while a screen is shared (needs "privacy" and "notifications" in modules)

# Per app, the first match wins, at most 32. The app name is the whole name, any case.
# [[notifications.rules]]
# app = "Chatty"
# action = "mute"      # "mute": no bubble, kept in the history and counted; "drop": ignored entirely
# urgency = "low"      # "low" | "normal" | "critical": replaces the app's own (not with "drop")

[osd]
timeout_ms = 1300    # how long the pill stays after the last change (600 to 5000)
width = 32           # cells of its row (24 to 60)
margin = 48          # px above the bottom edge (0 to 400)
backlight = ""       # a device in backlight_dir; "": the first one, in sorted order
backlight_dir = "/sys/class/backlight"   # read only: on `alpheus-launch --osd brightness` and when the quick panel opens
kinds = ["volume", "mic", "brightness"]  # also "bluetooth", "dnd" (needs "notifications"), "profile", "layout", "track"; each brings its source

[quick]
step = 5                                   # % per ←/→ or wheel notch on a slider (1 to 25)
keep_awake = { toggle = [], state = "" }   # toggle: a command, run as given; [] hides the row
night_light = { toggle = [], state = "" }  # state: "", "file:<path>" (on while it exists) or "process:<name>" (on while it runs)
lock = ["loginctl", "lock-session"]        # session buttons and the ⏻ menu's entries, run as given; [] hides one
logout = []
reboot = ["systemctl", "reboot"]           # reboot and power off ask once more
poweroff = ["systemctl", "poweroff"]
# wifi_more = ["foot", "-e", "nmtui"]      # More… in the Wi-Fi detail; absent (the default): launcher.toml's terminal, then nmtui; [] hides it
# vpn_more = ["foot", "-e", "nmtui-edit"]  # More… in the VPN detail; absent (the default): launcher.toml's terminal, then nmtui-edit; [] hides it
# mixer = ["foot", "-e", "wiremix"]        # Mixer… under the microphone; absent (the default): launcher.toml's terminal, then wiremix; [] hides it
dnd_until = "08:00"                         # the do-not-disturb detail's "Until" entry, HH:MM (local time, tomorrow if past)
kbd_backlight = true                        # the keyboard backlight row (hidden without a *::kbd_backlight LED); false: nothing read
kbd_backlight_dir = "/sys/class/leds"       # where LEDs are listed (absolute); read when the panel opens, never written
airplane = true                             # the airplane mode row (hidden without a Wi-Fi, Bluetooth or WWAN radio); false: nothing read
rfkill_dir = "/sys/class/rfkill"            # where rfkill switches are listed (absolute); read when the panel opens, never written

[timer]
countdown = 10       # minutes, the popup's Timer row at start (1 to 180)
work = 25            # Pomodoro, minutes (1 to 180)
short_break = 5
long_break = 15
rounds = 4           # work sessions before the long break (1 to 12)
sound = []           # an argv run at each end (not under do-not-disturb), e.g. ["pw-play", "/path/bell.oga"]; [] none
idle = "glyph"       # "glyph" | "hidden": the chip when nothing runs
proc_dir = "/proc"     # where the scheduled shutdown looks for a screen locker

[keyboard]           # with "keyboard" in modules (not in the default list)
short = true         # "English (US)" -> "us"; false: the full name
show_mode = true     # the bind mode after the layout when it is not "default"

[displays]
auto = true                          # apply the displays.toml profile that matches the connected screens, 500 ms after they change
# drm_dir = "/sys/class/drm"         # where connectors are listed: a card driven by evdi marks a DisplayLink screen (120 Hz cap)

[clipboard]
enabled = false           # true: follow the clipboard and keep a text history in memory (50 x 4 KiB, never a copy with a password-manager hint); read at start

# [captures]              # screenshots and recordings (see the table above); read at start
# dir = ""                # empty: $XDG_PICTURES_DIR/screenshots, else ~/Pictures/screenshots
# annotate = ["satty", "--filename", "{file}", "--output-filename", "{out}", "--early-exit", "--copy-command", "cat >/dev/null", "--save-after-copy", "--disable-notifications"]   # [] turns it off
# ocr = ["tesseract", "{file}", "-", "-l", "eng"]          # its standard output is copied; [] turns it off
# record = ["wf-recorder", "-g", "{geometry}", "-f", "{out}"]   # stopped by SIGINT to its group; [] turns it off
# pick_format = "{hex}"   # the colour picker's text: {r}, {g}, {b}, {hex}
# [shield]                # Shield mode: none set, no Shield anywhere; read at start (restart the daemon after a change)
# flag = "/var/lib/shield/on"      # absolute: the mode is on while this file exists (not in alpheus's config directory)
# on = ["pkexec", "/usr/local/bin/shield-root", "on"]     # argv, run as given; their program asks for authorisation itself
# off = ["pkexec", "/usr/local/bin/shield-root", "off"]
# allow = ["pkexec", "/usr/local/bin/shield-root", "allow", "{id}"]   # {id}: one whole element, once; absent: devices are listed only
# blocked = ["usbguard", "list-devices", "--blocked"]   # the default; []: no device list
# on_text = "Protections for public networks are on."     # the notices' bodies, one line, 256 bytes at most
# off_text = "Back to the usual settings."

[hooks]            # a command per event, its data in ALPHEUS_* variables (see features/hooks.md); none set: nothing runs
# started = ["notify-send", "alpheus up"]   # an argv, run as given (no ~ or $VAR expansion); [] or absent: no hook
# theme_mode = []                           # also dnd, battery_charge, battery_level, power_profile, network, vpn, bluetooth, privacy, shield, failed_units, displays
```

An unknown module name in `modules` is an error that makes the whole `bar.toml` fall back to its
defaults (one warning in the log), like any other invalid key.

## `launcher.toml`

Read at each open of the launcher (nothing kept while it is closed). Unknown keys make the file
invalid. `terminal`, `terminal_hold` and `focus` are separate commands; switching terminal means
setting both of the first two.

### Top level

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `terminal` | command | `["foot", "-e"]` | non-empty, non-blank program | Runs `Terminal=true` apps, plain. |
| `terminal_hold` | command | `["foot", "--hold", "-e"]` | non-empty, non-blank program | A command or typed shell text on Enter: the terminal stays open to read the output (`Ctrl+Enter` runs detached instead). |
| `rows` | integer | `10` | 3 to 30 | Result rows. The `--tools` command panel's box ignores it: about 106 cells wide, at most 90 % × 80 % of the output. |
| `history` | integer | `200` | 0 to 1000 | Entries kept; 0: no history file (never read, never written). |
| `proc_dir` | path | `"/proc"` | absolute | A testing seam for the kill list (`alpheus-launch --kill`): it decides only which pids are listed (a folder of links to `/proc/<pid>` narrows the list). Start times, ancestors and signals always use the real `/proc`. Leave it alone: the headless tests point it at a folder of their own so that they can never list a real process. |
| `focus` | command | `["wm-set", "focus-window", "{window}"]` | non-empty, non-blank program that does not contain `{window}`; some element must contain `{window}` | Run to focus a window; `{window}` is `address:0x…` on Hyprland, `id:N` elsewhere. `wm-set` is an external script, see [compositors.md](compositors.md#external-commands). |
| `copy` | command | `["wl-copy"]` | non-blank program when set | The calculator's Enter, the emoji copy, the notepad's Ctrl+C/X: the text on its standard input; `[]` hides the action. |
| `search` | string (URL) | `"https://duckduckgo.com/?q={query}"` | `""`, or `<scheme>://…` holding `{query}` | The `?` prefix's URL; `{query}` is percent-encoded; `""` turns `?` off. |
| `open` | command | `["xdg-open"]` | non-blank program when set | Opens that URL (appended as the last argument); `[]` turns `?` off. |

The `?`, `:`, `$` and `"` prefixes of Mod+Space's All mode are fixed (`:` and `$` are always
claimed); [the launcher page](features/launcher.md) says what each does.

### `[prefixes]`

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `commands` | string | `">"` | non-empty, no leading whitespace | Switches to the `$PATH` commands mode. |
| `windows` | string | `"w "` | non-empty, no leading whitespace | Switches to the windows mode. |
| `tools` | string | `"/"` | may be empty (off); no leading whitespace | Jumps into the Tools menu (it swallows a path-like query); `""` turns it off. |

No prefix may be a prefix of another (equal included). A `tools` the file never sets that clashes
with your `commands` or `windows` is turned off with a note in the log; one the file sets is
refused.

### `[[sources]]`

Menu scripts whose lines the launcher searches next to the apps. **At most 8.**

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `name` | string | required | 1 to 32 characters, not blank, unique, no control character or U+FFFD | The source's name (in logs and the history key). |
| `list` | command | required | non-empty, non-blank program | Run once per open on the scan thread, after the apps: 1 s at most, 2,000 lines, 256 KiB. |
| `run` | command | required | non-blank program without `{line}`; `{line}` exactly once, as a whole element | Run when an entry is chosen; `{line}` becomes the cleaned line, as one argument, never through a shell. |

A line longer than 512 bytes once cleaned is skipped. [The launcher page](features/launcher.md) describes command sources in use.

```toml
[[sources]]
name = "Tools"                     # 1 to 32 characters, unique, no control character (logs, history)
list = ["my-menu", "--list"]       # argv, no shell
run  = ["my-menu", "{line}"]       # "{line}": the chosen line, as one argument
```

### `[tools]`, `[[tools.category]]`, `[[tools.entry]]`

The Tools menu (`alpheus-launch --tools`); see [the Tools menu](features/tools-menu.md). Past a
bound the first ones are kept, with one log line of counts.

| Key of `[tools]` | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `builtins` | bool | `true` | | `false`: no built-in entry. |
| `hide` | list of ids | `[]` | | Built-in ids never shown (`"tab.power"`, `"quick.shield"`…); an unknown one is logged and ignored. |
| `root` | list of ids | `[]` | | Built-in ids also listed at the root, first, in this order (`"keep_awake"`): the command panel's Pinned section. |
| `panel` | bool | `true` | | `false`: Super+X opens on the category list instead of the command panel, with no letters. |
| `keys` | table of id to letter | `{}` | each letter one lowercase ASCII letter, `a` to `z` | The letter that runs a built-in from the command panel, for example `{ "quick.wifi" = "w" }`; an unknown id is logged and ignored. |
| `reload` | command | `[]` | non-blank program | The "Reload the bar" built-in's command, for example `["my-reload"]`; `[]` hides it. |

`[tools.builtin]`:

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `usb_reset` | command | `["sh", "-c", "sudo modprobe -r xhci_pci && sudo modprobe xhci_pci"]` | non-blank program | The "Reset USB (xHCI)" built-in's command, run in `terminal_hold` after a second Enter (so `sudo` can ask there; alpheus never runs as root); `[]` hides the entry. |

`[[tools.category]]`, **at most 16**:

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `name` | string | required | 1 to 48 characters, unique, no `/`, no control character | May be a built-in category's name (`Quick settings` 10, `Control center` 20, `Utilities` 30, `Captures` 40, `Diagnostics` 70, `System` 80, `Session` 90): it then restyles it, its `icon` and `order` replacing the built-in ones. |
| `icon` | one character | a generic folder glyph | exactly 1 character | The row's glyph. |
| `order` | integer | `50` | 0 to 1000 | Root rows sort by `order`, then name. |

`[[tools.entry]]`, **at most 128**:

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `name` | string | required | 1 to 48 characters, no `/`, unique within its category | The entry's name. |
| `category` | string | the root | a `[[tools.category]]` or a built-in category | Where the entry lives. |
| `icon` | one character | none | exactly 1 character | The row's glyph. |
| `order` | integer | `50` | 0 to 1000 | Sort order among its siblings. |
| `run` | command | required | non-blank program; with choices, `{choice}` exactly once as a whole element (not the program); without choices, no `{choice}`; with `ask`, `{text}` exactly once as a whole element (not the program), and without `ask`, no `{text}`; `{text}` never in `state` or `choices_cmd` | What the entry runs (no shell). |
| `terminal` | bool | `false` | not with `hold` | `true`: the launcher's `terminal` is put in front. |
| `hold` | bool | `false` | not with `terminal` | `true`: `terminal_hold` instead (the output stays readable). |
| `confirm` | bool | `false` | not with `ask` | `true`: the first Enter asks `Name? ⏎`, a second one 0.4 s to 3 s later runs it. |
| `key` | one letter | none | one lowercase ASCII letter, `a` to `z` | The letter that runs the entry from the command panel. |
| `description` | string | none (the category's name shows) | 1 to 80 characters, not blank, no control character | One line on what the entry does, shown under its name in the command panel. |
| `ask` | string | none | 1 to 48 characters, not blank; `run` must hold `{text}` | A label: the entry asks for a text (after its choice, if it has choices) and runs with it as the one element `{text}`. Never logged, never kept in the history. |
| `state` | command | `[]` | not with choices; read for at most 16 entries | Run when the box opens: exit 0 shows the switch on, any other exit off; a hung one is killed at 1 s. |
| `choices` | list of strings | `[]` | at most 32, each 1 to 64 characters; not with `choices_cmd` | Fixed choices for `{choice}`. |
| `choices_cmd` | command | `[]` | non-blank program; not with `choices` | A command whose output lines are the choices, run when the entry is opened (1 s, 2,000 lines, 256 KiB). |

Names, commands and lines are never logged: a message names an entry by its position in the file.

```toml
# launcher.toml — default values
terminal = ["foot", "-e"]                        # Terminal=true apps, plain
terminal_hold = ["foot", "--hold", "-e"]         # a command or typed shell text on Enter: the terminal stays open
rows = 10                                        # result rows (3 to 30)
history = 200                                    # entries kept (0 to 1000; 0: no history file)
proc_dir = "/proc"                               # testing seam: only narrows which pids the kill list shows; leave it
focus = ["wm-set", "focus-window", "{window}"]   # run to focus a window; {window} is required (address:0x… on Hyprland, id:N elsewhere)
copy = ["wl-copy"]                               # the calculator's Enter, the emoji copy, the notepad's Ctrl+C/X: the text on its standard input; [] hides the action
search = "https://duckduckgo.com/?q={query}"    # the ? prefix's URL; {query} is required, percent-encoded; "" turns ? off
open = ["xdg-open"]                              # opens that URL (appended as the last argument); [] turns ? off

[prefixes]
commands = ">"       # non-empty, no leading space, neither a prefix of the other
windows = "w "
tools = "/"          # jumps into the Tools menu; "" turns it off

[tools]              # the Tools menu (`alpheus-launch --tools`), see features/tools-menu.md
builtins = true      # false: no built-in entry
hide = []            # built-in ids never shown ("tab.power", "quick.shield", …); an unknown one is logged and ignored
root = []            # built-in ids also listed at the root, first, in this order ("keep_awake")
panel = true         # false: Super+X opens on the category list, with no letters
keys = {}            # built-in id to the letter that runs it from the panel, e.g. { "quick.wifi" = "w" }; an unknown id is logged and ignored
reload = []          # the "Reload the bar" built-in's argv, e.g. ["my-reload"]; [] hides it

[tools.builtin]
usb_reset = ["sh", "-c", "sudo modprobe -r xhci_pci && sudo modprobe xhci_pci"]   # "Reset USB (xHCI)", in terminal_hold after a second Enter; [] hides it

# --- example, not a default: no category or entry exists unless you write one ---
[[tools.category]]   # at most 16
name = "Nix"         # 1 to 48 characters, unique; may be a built-in category's name (it then restyles it)
icon = "󰏖"           # one character; absent: a generic folder glyph
order = 50           # 0 to 1000, default 50; root rows sort by order, then name

[[tools.entry]]      # at most 128
name = "Update"      # 1 to 48 characters, unique within its category
category = "Nix"     # a [[tools.category]] or a built-in category; absent: at the root
icon = "󰚰"           # one character; absent: none
run = ["my-update", "--now"]   # argv, no shell; required
terminal = false     # true: `terminal` is put in front
hold = false         # true: `terminal_hold` instead (the output stays readable); not with `terminal`
confirm = false      # true: asks "Update? ⏎", runs on a second Enter 0.4 s to 3 s later
key = "u"            # one lowercase letter a to z: runs the entry from the command panel; default none
description = "Update the system now"   # 1 to 80 characters: one line under the name; absent: the category's name
# ask = "Host"       # instead of confirm: asks for a text (after the choice, if any); run then holds "{text}" once, as a whole element

[[tools.entry]]
name = "Generation"
category = "Nix"
run = ["my-switch", "{choice}"]      # with choices, `run` holds "{choice}" as a whole element
choices = ["boot", "test", "switch"] # at most 32 of 1 to 64 characters; or: choices_cmd = ["my-list"]

[[tools.entry]]
name = "Gaming mode"
run = ["my-gaming", "toggle"]
state = ["my-gaming", "status"]      # exit 0 = on (a switch is shown), else off; not with choices
```

## `theme.toml`

Watched and reloaded live. With no `theme.toml`, or no `theme` key in it, alpheus uses `wallpaper`
when `colors.toml` exists in the configuration directory, and
`catppuccin` otherwise. An unknown name falls back to `catppuccin`, with one line in the log. This
file is lenient: an unknown key, or a value of the wrong kind, is ignored with one warning and the
rest applies. The Theme tab of the control center edits the same keys. If the configuration
directory is itself a symlink (home-manager), the watch follows it to its target, so replacing the
link is not seen until the bar restarts (the bar logs it once at start).

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `theme` | string | `"catppuccin"` (`"wallpaper"` when no `theme` key and `colors.toml` exists) | a preset below, or `"wallpaper"` | The colours. |
| `mode` | `"auto"`, `"dark"` or `"light"` | `"auto"` | | `auto` follows the desktop (the portal's `org.freedesktop.appearance` `color-scheme`, read by the bar, which passes it on to the control center); the others fix it. |
| `module_colors` | `"none"`, `"icons"`, `"full"` or `"pills"` | `"icons"` | | How much of each module takes its hue (see [`module_colors`](#module_colors)). |
| `style` | `"flat"`, `"islands"`, `"powerline"`, `"floating"`, `"underline"` or `"translucent"` | `"flat"` | | The bar's shape (see [`style`](#style)). |
| `opacity` | float | `0.8` | 0.3 to 1.0, clamped with one log line | `translucent` only; a value out of range still logs its warning under any other style. Set in the file only. |
| `[colors]` | table of role or hue = `"#rrggbb"` | empty | a role or hue name; a bad name or colour is ignored with a warning | Overrides one role or hue, whatever the theme and mode. |

```toml
# ~/.config/alpheus/theme.toml — every key optional
theme = "catppuccin"   # a preset below, or "wallpaper"
mode  = "auto"         # "auto" (follow the desktop), "dark", "light"
module_colors = "icons" # "none", "icons" (default), "full", "pills": see the `module_colors` section
style   = "flat"        # "islands", "powerline", "floating", "underline", "translucent": see the `style` section
opacity = 0.8           # translucent only

[colors]               # override any role, whatever the theme and mode
# accent = "#ff9e64"
```

### Presets

| `theme` | Dark | Light |
|---|---|---|
| `catppuccin` | Mocha | Latte |
| `gruvbox` | dark | light |
| `tokyo-night` | Night | Day |
| `rose-pine` | main | Dawn |
| `everforest` | dark medium | light medium |
| `kanagawa` | Wave | Lotus |
| `solarized` | dark | light |
| `nord` | Nord | dark only |
| `dracula` | Dracula | dark only |
| `ember` | Ember | Ember |
| `glacier` | Glacier | Glacier |
| `ink` | Ink | Ink |
| `hicon` | High contrast | High contrast |
| `sorbet` | Sorbet | Sorbet |
| `phosphor` | Phosphor | dark only |

A preset with no light variant stays dark in light mode.

### Roles

A theme is 16 colours, named by use (the keys of `[colors]` and of a colour file):

| Role | Used for |
|---|---|
| `bar_bg` | the bar's background |
| `pill_bg` | raised cells of the bar (a visible workspace); the blocks of the block styles |
| `popup_bg` | menus, calendar, tooltip, launcher |
| `raised` | reserved for hover; powerline's alternate blocks; a visible workspace inside a block |
| `selection` | the selected row |
| `fg`, `fg_dim` | text, secondary text (frames are `fg_dim` at 40 %) |
| `on_accent` | text on an accent |
| `accent`, `accent_container` | the clock, the active workspace |
| `accent2` | checks and radios in menus |
| `accent2_container` | reserved: no surface uses it |
| `ok`, `warn`, `error`, `urgent` | states: low battery is `warn`, critical `error`, an urgent workspace `urgent` |

A theme also carries seven hues, `red`, `orange`, `yellow`, `green`, `cyan`, `blue` and `purple`;
`[colors]` can override a hue like any role.

### `module_colors`

| `module_colors` | Look |
|---|---|
| `none` | no hue: icons and text keep the module's own style (states such as low battery still show) |
| `icons` (default) | each module's icon takes its hue |
| `full` | the icon and the module's text |
| `pills` | each block filled with its hue (block styles, see below) |

### `style`

| `style` | Look |
|---|---|
| `flat` | one line on the bar colour, modules separated by a dim dot |
| `islands` | each group of modules in its own rounded block; the bar between them is transparent |
| `powerline` | full-height blocks joined by arrows, alternating two tones |
| `floating` | one rounded bar, 6 px from the top and sides of the screen, with a light shadow |
| `underline` | blocks one step lighter than the bar, a thin line under it, the active workspace in the accent, underlined |
| `translucent` | the bar at `opacity` over the wallpaper, solid blocks |

### Wallpaper colours (matugen)

`theme = "wallpaper"` reads `colors.toml` (dark) and `colors-light.toml` (light). If only the dark
file exists, light mode uses it too. A file gives the 16 roles above. The 7 keys of earlier
versions (`bg fg accent muted surface error ok`) still work, and the other roles are derived from
them.

A colour file may carry the seven hues as optional keys (`red = "#f38ba8"`, and so on); a hue that
is absent is derived from Catppuccin's (Mocha for dark, Latte for light) mixed 30 % toward the
accent, so it stays in tune with the theme.

A matugen template for the 16 roles, `~/.config/matugen/templates/alpheus.toml`:

```toml
bar_bg            = "{{colors.surface_container_lowest.default.hex}}"
pill_bg           = "{{colors.surface_container_high.default.hex}}"
popup_bg          = "{{colors.surface_container.default.hex}}"
raised            = "{{colors.surface_container_highest.default.hex}}"
selection         = "{{colors.secondary_container.default.hex}}"
fg                = "{{colors.on_surface.default.hex}}"
fg_dim            = "{{colors.on_surface_variant.default.hex}}"
on_accent         = "{{colors.on_primary.default.hex}}"
accent            = "{{colors.primary.default.hex}}"
accent_container  = "{{colors.primary_container.default.hex}}"
accent2           = "{{colors.tertiary.default.hex}}"
accent2_container = "{{colors.tertiary_container.default.hex}}"
ok                = "{{colors.tertiary.default.hex}}"
warn              = "{{colors.amber.default.hex}}"
error             = "{{colors.error.default.hex}}"
urgent            = "{{colors.error_container.default.hex}}"
```

`amber` is a custom colour, tinted toward the wallpaper. The seven hues are custom colours too,
harmonised with the wallpaper: add these lines to the template, in the same way (the keys are
optional; without them alpheus derives the hues from the accent):

```toml
red    = "{{colors.red.default.hex}}"
orange = "{{colors.orange.default.hex}}"
yellow = "{{colors.yellow.default.hex}}"
green  = "{{colors.green.default.hex}}"
cyan   = "{{colors.cyan.default.hex}}"
blue   = "{{colors.blue.default.hex}}"
purple = "{{colors.purple.default.hex}}"
```

One matugen configuration per mode, each writing its own file.
`~/.config/matugen/alpheus-dark.toml`:

```toml
[config.custom_colors]
amber  = { color = "#f9a825", blend = true }
red    = { color = "#f38ba8", blend = true }
orange = { color = "#fab387", blend = true }
yellow = { color = "#f9e2af", blend = true }
green  = { color = "#a6e3a1", blend = true }
cyan   = { color = "#94e2d5", blend = true }
blue   = { color = "#89b4fa", blend = true }
purple = { color = "#cba6f7", blend = true }

[templates.alpheus]
input_path = "~/.config/matugen/templates/alpheus.toml"
output_path = "~/.config/alpheus/colors.toml"
```

`alpheus-light.toml` is the same with `output_path = "~/.config/alpheus/colors-light.toml"`. Then,
at each wallpaper change:

```sh
matugen -c ~/.config/matugen/alpheus-dark.toml  image "$WALLPAPER" -m dark  -t scheme-vibrant
matugen -c ~/.config/matugen/alpheus-light.toml image "$WALLPAPER" -m light -t scheme-vibrant
```

`-t scheme-vibrant` gives livelier colours than matugen's default (`scheme-tonal-spot`). The bar
follows the new files on its own. Run from a script with no terminal (a wallpaper hook), matugen
cannot ask which of a picture's dominant colours to use: add `--source-color-index 0` to pick the
first.

## `config.toml`

Read once, when the control center starts. Unknown keys make the file invalid, except that an
out-of-range `[diagnostics]` number is put back to its default with a warning (the other keys keep
their values).

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `[keys] dvorak_nav` | bool | `false` | | `true`: `d`/`h`/`t`/`n` also move left/down/up/right, beside the arrow keys (their hjkl positions on a Dvorak layout). |
| `[keys.bindings]` | table, action = list of chords | empty | see below | Replaces that action's keys. |
| `[commands] mixer` | command | `["wiremix"]` | | Opened in alpheus's own window. |
| `[commands] network` | command | `["nmtui"]` | | Opened in alpheus's own window. |
| `[commands] bluetooth` | command | `["bluetui"]` | | Opened in alpheus's own window. |
| `[diagnostics] copy` | command | `["wl-copy"]` | | The bug report's clipboard program (the text on its standard input); `[]` disables Copy. |
| `[diagnostics] system_actions` | bool | `true` | | Restart and reset-failed on system units, in the control center's own window (polkit asks there); `false` hides both keys on system units. |
| `[diagnostics] restart_loop` | integer | `3` | 1 to 1000 | `NRestarts` at or above this is "restarting in a loop". |
| `[diagnostics] journal_lines` | integer | `500` | 1 to 5000 | Entries read when the journal view opens. |
| `[[diagnostics.checks]]` | table | none | **at most 8**; a bad one is dropped with a warning | Checks of your own, run on showing the Diag tab and on F5. |

`[[diagnostics.checks]]` keys: `name` (required, 1 to 40 characters, no control or invisible
character), `run` (required, a non-empty command; exit 0 is fine), `fix` (optional, at most 200
characters, no control or invisible character; shown as text, never run).

Action names for `[keys.bindings]`: `quit`, `back`, `help`, `palette`, `search`, `next_tab`,
`prev_tab`, `tab_1`…`tab_8`, `unit_restart`, `unit_reset`, `refresh`, `left`, `right`, `up`,
`down`, `page_up`, `page_down`, `home`, `end`, `toggle`, `activate`, `decrease`, `increase`. A
chord is optional modifiers joined by `+` (`ctrl`, `alt`, `shift`) then a key: a single character
(`?`, `-`, `+`, `k`…), or `esc`, `enter`, `space`, `tab`, `backspace`, `up`, `down`, `left`,
`right`, `pageup`, `pagedown`, `home`, `end`, `f1`…; case does not matter (`ctrl+p`,
`shift+tab`). A key given to one action is taken away from the others; an unknown action, modifier
or key resets all key bindings to their defaults (one warning on the status line); the rest of the
file still applies.

See [the control center](features/control-center.md).

```toml
[keys]
dvorak_nav = false   # true: d/h/t/n also move left/down/up/right, beside the arrow keys

[keys.bindings]      # optional: action = ["chord", …], replaces that action's keys
# palette = ["ctrl+k"]

[commands]           # opened in alpheus's own window (program, then arguments)
mixer = ["wiremix"]
network = ["nmtui"]
bluetooth = ["bluetui"]

[diagnostics]        # the Diag tab
copy = ["wl-copy"]           # the bug report's clipboard program, the text on its standard input; [] disables Copy
system_actions = true        # restart / reset-failed on system units, in the control center's own window (polkit asks there)
restart_loop = 3             # NRestarts at or above this is "restarting in a loop" (1 to 1000)
journal_lines = 500          # entries read when the journal view opens (1 to 5000)

# At most 8 checks of your own, run on showing the tab and on F5; exit 0 is fine, `fix` is shown, never run.
# [[diagnostics.checks]]
# name = "One hypridle"
# run = ["sh", "-c", "[ \"$(pgrep -cx hypridle)\" -le 1 ]"]
# fix = "pkill -x hypridle && systemctl --user restart hypridle"
```

## `displays.toml`

Screen profiles. Normally written by the Displays tab, not by hand; read by the control center
when it starts and by the daemon at the end of each hotplug debounce (read capped, from a regular
file only). With `[displays] auto = true` (the default), the profile whose screens are exactly the
connected ones is applied after the debounce, never during a trial. A screen is matched by
identity (`make model serial`, else its `description`); the saved connector is only a hint between
two identical screens. An invalid file means no profile and a warning. Unknown keys make it
invalid. See [displays](features/displays.md).

`[[profile]]`:

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `name` | string | required | | The profile's name. |
| `[[profile.display]]` | table | none | | One entry per screen of the profile. |

`[[profile.display]]`:

| Key | Type | Default | Bounds | Meaning |
|---|---|---|---|---|
| `description` | string | required | | The screen's identity (`make model serial`, or an older compositor description). |
| `connector` | string | absent | | A connector hint, only to tell identical screens apart. |
| `enabled` | bool | `true` | | A disabled screen is switched off. |
| `mode` | string `"WxH@Hz"` | absent | required when `enabled`; width and height above 0 | For example `"1920x1080@60"`; a refresh of 0 means one the compositor does not know. |
| `scale` | float | `1.0` | above 0 and at most 10 | The output's scale (rounded to 1/1000). |
| `rotation` | integer | `0` | 0 to 7 | `wl_output_transform`. |
| `placement` | inline table | origin (0, 0) | one of the two forms below | Where the screen sits. |

`placement` is either `{ relation, reference, align }` (`relation` one of `right_of`, `left_of`,
`above`, `below`; `reference` the identity of the screen it is placed against; `align` one of
`start` (default), `center`, `end`) or `{ x, y }` (absolute integer coordinates).
