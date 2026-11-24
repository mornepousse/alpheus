# Installing alpheus

alpheus is three programs built from one Cargo workspace:

| Program | Role |
|---|---|
| `alpheus` | the TUI control center |
| `alpheus-demon` | the resident daemon: bar, launcher, notifications, OSD, quick settings |
| `alpheus-launch` | a tiny client that talks to the daemon (open the launcher, dmenu mode, do-not-disturb, quick panel) |

You need `alpheus-demon` for anything graphical, `alpheus-launch` to bind
keys to it, and `alpheus` for the control center. Command lines are in
[cli.md](cli.md), compositor specifics in [compositors.md](compositors.md).

## Download a release

Each release at <https://github.com/mornepousse/alpheus/releases> carries an
x86_64 Linux tarball (built by CI on Ubuntu 24.04) and a `SHA256SUMS` file:

```sh
v=0.1.0
base=https://github.com/mornepousse/alpheus/releases/download/v$v
curl -fLO "$base/alpheus-$v-x86_64-linux.tar.gz"
curl -fLO "$base/SHA256SUMS"
sha256sum -c SHA256SUMS                      # must print: alpheus-0.1.0-x86_64-linux.tar.gz: OK
tar -xzf "alpheus-$v-x86_64-linux.tar.gz"
install -Dm755 -t ~/.local/bin "alpheus-$v-x86_64-linux"/alpheus{,-demon,-launch}
```

The tarball also holds `LICENSE`, `NOTICE`, `README.md` and an `INSTALL` note. The
binaries link libdbus-1 and libxkbcommon dynamically and need a Nerd Font (see
[Without Nix](#without-nix) and [Runtime requirements](#runtime-requirements)); they
are built against Ubuntu 24.04's glibc, so on an older distribution build from
source or use Nix.

## Build

### With Nix

The flake has one package per program, plus the default (the TUI):

```sh
nix build .#alpheus          # the TUI
nix build .#alpheus-demon    # the daemon (carries its fonts and links libdbus and libxkbcommon)
nix build .#alpheus-launch   # the client
nix develop                  # dev shell: cargo, rustfmt, clippy, sway for the tests
```

Without cloning, the flake runs or builds a program straight from GitHub (each
package names its main program):

```sh
nix build github:mornepousse/alpheus#alpheus-demon     # ./result/bin/alpheus-demon
nix run github:mornepousse/alpheus#alpheus             # the control center
nix run github:mornepousse/alpheus#alpheus-launch -- --help
```

The daemon package sets `ALPHEUS_POLICES` at build time to a folder holding
JetBrainsMono Nerd Font Mono and DejaVu Sans Mono (the fallback for glyphs the
first one lacks), so no font needs installing on Nix.

As an input of your own flake (the repository is the flake; there is no
NixOS or home-manager module, you wire the packages yourself):

```nix
{
  inputs.alpheus = {
    url = "github:mornepousse/alpheus";    # or "git+file:///path/to/alpheus"
    inputs.nixpkgs.follows = "nixpkgs";    # the daemon then maps the same libdbus as the rest of your session
  };

  # ... in a NixOS module, or home.packages for home-manager:
  environment.systemPackages = with inputs.alpheus.packages.${pkgs.system}; [
    alpheus alpheus-demon alpheus-launch
  ];
}
```

Whatever is not committed in a `git+file://` checkout is not seen by the build.

### Without Nix

```sh
cargo build --release          # all three programs, in target/release/
```

Build-time requirements:

- Rust 1.95 or newer (edition 2024; the `rust-version` in the workspace `Cargo.toml`, which CI builds with on every push).
- `pkg-config`.
- The development files of **libdbus-1** (the system-bus connection goes
  through the `dbus` crate, which links libdbus) and **libxkbcommon** (the
  launcher's keyboard; SCTK's `xkbcommon` feature).

On Debian or Ubuntu that is `pkg-config libdbus-1-dev libxkbcommon-dev`; on
Fedora `pkgconf-pkg-config dbus-devel libxkbcommon-devel`; on Arch
`pkgconf dbus libxkbcommon`. (Package names are given from memory of those
distributions, not tested here.) The TUI alone (`cargo build --release -p alpheus`)
needs none of these.

Put the three binaries on your `PATH` (for example in `~/.local/bin`).
Fonts: the daemon looks in `$ALPHEUS_POLICES` (a folder of font files; the run-time
variable wins over the build-time one), otherwise in the system's fonts. Install
a **Nerd Font** (the default is "JetBrainsMono Nerd Font Mono"; change
it in `bar.toml`'s `[font]`): the bar's icons, the tray and the power button
are Nerd Font glyphs.

## Runtime requirements

The daemon needs a Wayland compositor that supports the layer-shell protocol,
and a session bus. Every module degrades on its own when its tool or service is
missing (the module stays hidden or dim; nothing crashes), so install only
what you use.

| Used by | What it needs |
|---|---|
| Sound (bar module, volume OSD, quick panel, privacy indicators) | PipeWire with WirePlumber: `pw-dump` (a long-running `pw-dump -m -N -R` child) to read, `wpctl` to act |
| Network (bar, quick Wi-Fi) | NetworkManager on the system D-Bus to read; `nmcli` to act (Wi-Fi radio, connect, disconnect) |
| Bluetooth (bar, quick panel) | BlueZ on the system D-Bus (the daemon talks to it directly); `rfkill` to unblock a blocked radio |
| Battery | UPower on the system D-Bus |
| Power profile (quick panel) | `power-profiles-daemon` on the system bus to read; `powerprofilesctl` to set |
| Brightness (OSD, quick panel) | a backlight in `/sys/class/backlight`; `brightnessctl` to change it from the quick panel (the OSD only reads sysfs) |
| Resources | `/proc` and `/sys` only |
| Tray | the session bus: the daemon owns `org.kde.StatusNotifierWatcher` |
| Notifications | the session bus: the daemon owns `org.freedesktop.Notifications`, queued behind any other server (mako, dunst) already running; stop that one first if you want alpheus to take over |
| Media | a player exposing MPRIS on the session bus (alpheus talks MPRIS itself; **`playerctl` is not used**) |
| Launcher | `systemd-run --user` (used to start applications in a scope; a detached spawn is the fallback); a terminal for terminal apps (`foot` by default, `terminal` in `launcher.toml`) |
| Quick panel, power menu and control center Power tab actions | the commands of `[quick]` in `bar.toml`: `loginctl lock-session`, `systemctl reboot` and `systemctl poweroff` by default; log out is empty by default |
| Control center (`alpheus`) | the tools of its tabs: `wpctl`, `nmcli`, `bluetoothctl`, `brightnessctl`, `powerprofilesctl`, `systemctl`; `wiremix`, `nmtui` and `bluetui` are opened by default for deeper changes (set `[commands]` in `config.toml`) |
| Workspace switching, focusing a window from the launcher | an external `wm-set` command, see [compositors.md](compositors.md#external-commands) |
| Displays tab, screen profiles on hotplug | nothing external: a compositor that implements `zwlr_output_manager_v1` (the daemon speaks it itself) and the `displays` module |

Not used: `playerctl`, `pactl`, any compositor client such as `hyprctl` or
`swaymsg` (the daemon reads each compositor's socket itself and never writes
to it).

## Starting the daemon

`alpheus-demon` takes no arguments; start it once per session. A second copy
exits with a message. It needs `WAYLAND_DISPLAY` and the variable that
identifies your compositor (`HYPRLAND_INSTANCE_SIGNATURE`, `NIRI_SOCKET`,
`SWAYSOCK` or `MANGO_INSTANCE_SIGNATURE`), so the easiest way is the
compositor's own autostart (below). Without any of those variables the bar
still runs but shows "–" in place of workspaces.

Under systemd, a user unit must import the session environment first. A
minimal sketch (the unit name is yours to choose; this is a generic example, not
shipped):

```ini
# ~/.config/systemd/user/alpheus-demon.service
[Unit]
Description=alpheus bar daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=%h/.local/bin/alpheus-demon
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

For `WAYLAND_DISPLAY` and the compositor's socket variable to reach the unit, the
compositor must run `systemctl --user import-environment WAYLAND_DISPLAY
HYPRLAND_INSTANCE_SIGNATURE ...` (or `dbus-update-activation-environment
--systemd --all`) at startup. The daemon exits 1 on a failure of its event
loop, which `Restart=on-failure` relaunches; a compositor ending the session
exits 0. After editing `bar.toml`, restart the daemon (theme files reload live).

## Compositor wiring

Autostart `alpheus-demon`, bind a key to `alpheus-launch --toggle`, optionally
one to `alpheus-launch --quick` (the quick settings panel), and one to the TUI in
a floating terminal. These snippets are examples to adapt, and have not been tested against each compositor version; the key names are
yours. Window-rule syntax changes between compositor versions, so check your
own version's manual for the floating rule.

`alpheus-launch` exits 3 when the daemon is not running, which lets a bind fall
back to another launcher: `alpheus-launch --toggle; [ $? -eq 3 ] && fuzzel`
(inside `sh -c '...'` for compositors that do not run a shell). Do not use `||`:
exit 2 (busy, no answer, protocol error) would open the other launcher too.

### Hyprland

```ini
exec-once = alpheus-demon

bind = SUPER, space, exec, alpheus-launch --toggle
bind = SUPER, S, exec, alpheus-launch --quick
bind = SUPER, A, exec, foot --app-id alpheus-tui -e alpheus
windowrule = float on, size 1000 700, center on, match:class alpheus-tui
```

(The `windowrule` line uses the 0.5x syntax; older Hyprland versions write
`windowrulev2 = float, class:alpheus-tui`.) Hyprland re-fires a held press
bind; the daemon ignores a second `--quick` or `--toggle` arriving within 250 ms.

### Sway

```
exec alpheus-demon

bindsym $mod+space exec alpheus-launch --toggle
bindsym $mod+s exec alpheus-launch --quick
bindsym $mod+a exec foot --app-id alpheus-tui -e alpheus
for_window [app_id="alpheus-tui"] floating enable, resize set 1000 700
```

### niri

```kdl
spawn-at-startup "alpheus-demon"

binds {
    Mod+Space { spawn "alpheus-launch" "--toggle"; }
    Mod+S { spawn "alpheus-launch" "--quick"; }
    Mod+A { spawn "foot" "--app-id" "alpheus-tui" "-e" "alpheus"; }
}

window-rule {
    match app-id="alpheus-tui"
    open-floating true
}
```

### Mango

Mango's configuration syntax is not documented here from the code (alpheus
only reads its socket); the following follows mangowc's `config.conf`
conventions and is **not tested**:

```
exec-once=alpheus-demon
bind=SUPER,space,spawn,alpheus-launch --toggle
bind=SUPER,s,spawn,alpheus-launch --quick
bind=SUPER,a,spawn,foot --app-id alpheus-tui -e alpheus
windowrule=isfloating:1,appid:alpheus-tui
```

### Any other layer-shell compositor

The bar, launcher, notifications, OSD and quick panel only need layer-shell and
xdg-shell popups. Autostart the daemon and bind keys as above. Workspaces and
the window title are empty there (see [compositors.md](compositors.md)).

## Checking that it works

```sh
alpheus-demon            # in a terminal of the session: the log goes to standard error
alpheus-launch --toggle  # opens the launcher; exit code 3 means the daemon is not running
alpheus                  # the control center; 1 to 8 switch tabs, q quits
```

The first log lines name the detected compositor (`compositor: Sway ($SWAYSOCK)`)
and warn about any invalid configuration file.
