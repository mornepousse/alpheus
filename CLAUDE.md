# alpheus — Claude Code instructions

Mae's "homemade" desktop shell, light and TUI-spirited, written in Rust.
Start by reading [`docs/SCOPE.md`](docs/SCOPE.md) (why, budget, guardrails,
breakdown), then the spec of the sub-project currently in progress in
`docs/specs/`.

## Where things stand

- Sub-project 1 (TUI control center, ratatui): **v1 spec written**
  ([`docs/specs/2026-10-02-alpheus-v1-design.md`](docs/specs/2026-10-02-alpheus-v1-design.md)),
  implementation plan ([`docs/plans/2026-10-02-alpheus-v1-plan.md`](docs/plans/2026-10-02-alpheus-v1-plan.md))
  **executed** (16 tasks + fixes from the final review, merged into `main`
  on 2026-10-03) and wired into nixos-config (`Mod+A`). Real-world trial
  validated by Mae on 2026-10-03 (Hyprland and niri).
- Sub-project 1 bis, **Displays** tab (Hyprland first): spec
  ([`docs/specs/2026-10-03-alpheus-ecrans-design.md`](docs/specs/2026-10-03-alpheus-ecrans-design.md))
  and plan executed, merged into `main` on 2026-10-03 (through nixos-config's
  `wm-set`; moved to the output protocol on 2026-11-03, see
  below). Real-world trial by Mae still pending. Keyboard layouts tab: deferred.
- Fixtures and snapshots are **anonymized** (Wi-Fi, VPN, MAC, machine): never
  recommit a raw capture from the workstation, the repository is meant to
  go public.
- Sub-project 2 (resident daemon), first delivery **the bar**: spec
  ([`docs/specs/2026-10-04-alpheus-demon-barre-design.md`](docs/specs/2026-10-04-alpheus-demon-barre-design.md))
  and plan 1 (steps 1–2: workspace split, daemon with workspaces, window
  title and clock on every output, Hyprland and niri, hybrid mode in
  nixos-config) **implemented and merged** on 2026-10-04, after a final
  review and fix wave. Measured: ≈ 12 MB PSS at start, ≈ 13 MB under load
  (headless sway, 3 outputs), 5.5 MB live on Mae's laptop, no wake-up at
  rest. Real-world trial validated by Mae on 2026-10-04; **Mae now uses
  the alpheus bar daily** (`desktop.shell = "alpheus"`, DMS dropped), so
  every delivery lands on her machine.
- Bar **delivery 3** (sound, network, Bluetooth, battery, resources;
  plan [`docs/plans/2026-10-04-alpheus-bar-plan-2.md`](docs/plans/2026-10-04-alpheus-bar-plan-2.md),
  19 tasks) **implemented and merged** on 2026-10-05 after per-task reviews,
  a whole-D-Bus review, two whole-branch reviews and an optimisation round.
  Sound via a supervised `pw-dump -m -N -R` child; NetworkManager, UPower
  and BlueZ through one read-only D-Bus connection (allowlisted signals, no
  activation, per-domain outages); resources from `/proc` and `/sys` every
  2 s. Measured in **release** (the budget tests refuse debug builds):
  startup ≈ 7.9 MB PSS, steady ≈ 9.7 MB, **0 wake-ups at rest**, 8 threads.
  Tray plan 1 ([`docs/plans/2026-10-05-alpheus-tray-plan-1.md`](docs/plans/2026-10-05-alpheus-tray-plan-1.md),
  branch `feat/tray`) moved that connection to **dbus-rs** 0.9 over the
  system libdbus: zbus, its async stack and `blocking` left the tree (a
  boundary test guards it); same harness before/after — the budget tests
  now serve the recorded NetworkManager/UPower/BlueZ replies on the private
  bus, which is why "before" measures ≈ 8.4 MB here rather than delivery 3's
  7.9 MB above — libdbus shared with the session as installed: startup
  ≈ 8.4 → 7.0 MB PSS, steady ≈ 9.7 → 8.3 MB, 8 → 6 threads (the D-Bus source
  costs exactly one), 0 wake-ups.
  Merged with the launcher (`feat/launcher-1` → `main`) on 2026-10-05: on
  this same harness, with both deliveries together (all modules + the
  launcher linked in), startup ≈ 7.8 MB PSS, steady ≈ 8.6 MB after 8,300
  events and 5 plug cycles, still 6 threads and 0 wake-ups at rest — the
  launcher adds no thread while closed.
  Real trial on Mae's session done (she runs it daily).
- Bar **delivery 4, step 2** (the tray, menus, the calendar; spec
  [`docs/specs/2026-10-05-alpheus-tray-design.md`](docs/specs/2026-10-05-alpheus-tray-design.md),
  plan 2 [`docs/plans/2026-10-05-alpheus-tray-plan-2.md`](docs/plans/2026-10-05-alpheus-tray-plan-2.md))
  **implemented and merged into `main` on 2026-10-07** after per-task
  reviews, a three-slice whole-branch review and its fix rounds; Mae's
  trial on Hyprland done (Steam and Equibop icons, menus, pixel frames,
  calendar).
  `org.kde.StatusNotifierWatcher` and
  `org.kde.StatusNotifierItem`/`com.canonical.dbusmenu` on a second,
  non-blocking dbus-rs connection to the session bus (no new thread); icons
  as Nerd Font glyphs (no pixmaps ever requested); menus and the calendar as
  `xdg_popup`s on the bar's own layer surface, sharing the launcher's list
  rendering and keys. On Hyprland (any version; hyprwm/Hyprland#15729, fixed upstream in 0.56.2), the bar's keyboard interactivity stays None while a menu or the calendar grabs — detected from its `hyprland_*` globals,
  during a grab (3a6599e). Measured in **release** (headless sway, 3 runs):
  startup ≈ 7.9–8.0 MB PSS all modules with the tray unused, ≈ 8.4–8.8 MB
  with the tray and 10 fake items (that test runs without the launcher
  module), ≈ 9.6–9.7 MB after 8,300 events (absolute PSS shifts ≈ ±0.5 MB
  between boots, shared libraries being counted proportionally: trust the
  deltas), 0
  wake-ups at rest with a populated tray, 6 threads (the tray adds none),
  the module ≈ 380 KB and 10 items ≈ 490 KB (budget 2 MB), 20 popup cycles
  +32 KB in each of 3 runs. The baseline with the tray unused grew +0.4–0.7 MB: stripped
  binary 4.49 → 5.36 MB (+864 KB: libdbus, dbus-rs, menus, popups,
  calendar) — a one-time cost Mae accepts, like the launcher's. (Mae asked
  for a "one D-Bus stack" item to move NetworkManager/UPower/BlueZ off zbus:
  nothing to do, tray plan 1 already did it — zbus left the tree.)
  Launcher fix (main 9331286): a Toggle arriving less than 250 ms after the
  launcher opens is debounced, so Mod+Space is a press bind again.
- **Themes delivery 1 (colours)** (spec
  [`docs/specs/2026-10-07-alpheus-themes-design.md`](docs/specs/2026-10-07-alpheus-themes-design.md),
  plan [`docs/plans/2026-10-07-alpheus-themes-plan-1.md`](docs/plans/2026-10-07-alpheus-themes-plan-1.md))
  **implemented and merged into `main` on 2026-10-07** (per-task reviews, a final whole-branch review and fix wave; then, at Mae's request, the TUI **Theme tab** (Super+A, tab 7: theme list with preview, mode, module colours) and **module colours** (spec § 9: 7 hues per theme, `module_colors = "none" | "icons" | "full"`, default icons, fixed module→hue mapping, states win, a charging battery stays green)); Mae's trial done:
  - **Roles and presets.** 16 roles in `alpheus-commun::theme`, and 9 compiled-in presets (dark/light pairs, contrast-tested in both modes).
  - **Wallpaper.** One more theme, read from 16-role or legacy 7-key files.
  - **`theme.toml`.** Theme, mode `auto`/`dark`/`light`, and `[colors]` overrides. With no `theme.toml`: wallpaper if `colors.toml` exists, else catppuccin.
  - **Daemon.** Hot reload through one inotify fd (`watch.rs`; its parent is watched while `~/.config/alpheus` is missing). Mode `auto` follows the portal's `color-scheme` on the tray's session connection, or on one of its own when the tray is off.
  - **TUI.** Same resolver; no D-Bus. In mode `auto` it reads the mode the daemon publishes in `$XDG_RUNTIME_DIR/alpheus/mode` (dark when the daemon is not running).
  - **Measured in release** (vs `c85c0df`, the tray branch just before the theme code; not the merge-base with `main`, which has no tray yet): a one-time cost of +144–224 KB PSS at start (binary 5.11 → 5.18 MiB stripped, +76 KiB), +0–444 KB over 50 reloads (0 in four of five runs), and 0 loop wake-ups at rest after a reload.
  - **Pending.** Mae's matugen setup in nixos-config is her decision (spec § 5): the 16-role template, the dark and light runs, `-t scheme-vibrant`, and dropping `shell-ctl reload-bar` for colours.
  - **Next.** Themes delivery 2 (bar shapes, below): merged. The nixos-config branch `alpheus-16-roles` carries the 16-role matugen template (hues still to add).
- **Themes delivery 2 (bar shapes)** (spec
  [`docs/specs/2026-10-07-alpheus-shapes-design.md`](docs/specs/2026-10-07-alpheus-shapes-design.md),
  plan [`docs/plans/2026-10-07-alpheus-shapes-plan-1.md`](docs/plans/2026-10-07-alpheus-shapes-plan-1.md))
  **implemented and merged into `main` on 2026-10-08** (per-task reviews, a final whole-branch review and three fix waves); Mae's trial done ("ça marche bien"). Watch: once, Mae saw the TUI Style row show only the selected chip; not reproduced by her or in isolated foot/alacritty runs — ask for a screenshot if it comes back.
  Deferred minors: an empty `theme.toml` freezes the colour source if `colors.toml` appears later; hues and fg_dim on `raised` stay < 3:1 in solarized dark; apply toasts stack (one per Enter) over the preview.
  Mae's rulings during the trial (2026-10-08): MUTED cells in pills mixed 50 % toward the fill; powerline's workspaces block on `pill_bg`; underline marks the active workspace with accent text and the line only; a visible workspace inside a block is `raised`; `raised` one softer step from `pill_bg` in solarized dark/light and tokyo-night light (fg ≥ 3.5:1, tested).
  - **Six styles.** `style` in `theme.toml`: `flat` (default, byte-identical to before), `islands`, `powerline`, `floating`, `underline`, `translucent` (`opacity` 0.3–1.0, default 0.8). Plus `module_colors = "pills"` (contrast-picked text, MUTED cells mixed 50 % toward the fill (Mae), states win; like `full` under flat and underline).
  - **How.** The layout emits groups (one block each). The painter runs a background pass per style before the unchanged text pass: cached anti-aliased corner masks per (radius, scale), analytic powerline triangles, the floating shadow ramp. Wayland side: the opaque region per style; under floating, a 6 px margin, a taller surface for the shadow, an exclusive zone of the bar height (the compositor adds the margin: windows start 28 px down on the default bar, Mae's spec amendment ad309a0), an input region without the shadow, and an open popup closed when its bar is re-placed.
  - **Hot reload.** A style, opacity or module-colours change repaints. To or from floating, every bar's surface is reconfigured and waits for its configure before painting.
  - **TUI.** A Style row in the Theme tab, with a preview approximated in cells.
  - **Measured in release** (vs `6a6df10`, back to back, 3 runs each, separate target directories): one-time cost within noise (−48…+156 KB) (binary 5.20 → 5.24 MiB, +44 KiB stripped), per style within −76…+116 KB of flat (flat 7.9–8.0 MB), masks −32…+88 KB (islands − flat) and +36…+116 KB (floating − flat, its taller buffer included) on first use (computed: 128 B and 200 B at scale 1), +20 KB over 48 style switches, 0 loop wake-ups at rest under every style.
  - **Hyprland blur** is documented, never requested: `layerrule = blur on, ignore_alpha 0, match:namespace alpheus-bar` in Mae's hyprland.conf is her decision.
- **Bar step 5, delivery 1 (media)** (spec
  [`docs/specs/2026-10-08-alpheus-media-design.md`](docs/specs/2026-10-08-alpheus-media-design.md),
  plan [`docs/plans/2026-10-08-alpheus-media-plan-1.md`](docs/plans/2026-10-08-alpheus-media-plan-1.md))
  **implemented and merged into `main` on 2026-10-09** (per-task reviews, a three-slice final review and its fix wave):
  - **Bar.** `▶/⏸ Title` (Purple, its own block), hidden when nothing plays; middle click PlayPause, wheel Next/Previous, a capability the player lacks a no-op.
  - **Popup.** Title, artist, `m:ss` with a progress bar (hidden for an unknown length), ⏮ ⏯ ⏭ (dim when incapable), the players with ● from two up (Enter or a click pins one); keys and mouse as the menus; closes on Esc, an outside click, a restyle that moves its bar, or `player gone`.
  - **How.** `media.rs` (pure model) fed by `tray/mpris.rs` through the closed Call/Reply/Signal enums; on the tray's connection, else on one own link shared with the portal; Seeked followed and the position read only while the popup is open; a 1 s `DeadlineTimer` only while it is open and the track plays.
  - **Measured in release** (vs `55ba754`, back to back, 3 runs each, separate target directories): one-time cost within noise (−52…+32 KB) (binary 5.24 → 5.33 MiB, +92 KiB stripped), module with no player 0–4 KB in mode `auto` (its link is the portal's, already open), 286–395 KB in a fixed mode with the tray off (its own connection), 3 players 85–112 KB, +0 KB over 100 track changes, 0 loop wake-ups at rest with a player playing (tray on and off). With media now in the default `modules`, `pss_under_30mb_and_stable` (no session bus) starts +33…+123 KB over the base.
- **Launcher delivery 1** (the core; spec
  [`docs/specs/2026-10-05-alpheus-launcher-design.md`](docs/specs/2026-10-05-alpheus-launcher-design.md),
  plan [`docs/plans/2026-10-05-alpheus-launcher-plan-1.md`](docs/plans/2026-10-05-alpheus-launcher-plan-1.md))
  **implemented and reviewed** (per-task reviews plus a final whole-branch
  review in three slices): a centred Overlay box inside the daemon (apps,
  windows, `$PATH` commands, script mode), click-outside scrims on the
  box's own Overlay layer (not Top — Hyprland draws Top under a
  fullscreen window, so a scrim there would never catch a click while Mae
  is in a game), `alpheus-launch` (toggle, close, `--dmenu` with fuzzel's
  exit codes, plus 3 = daemon unreachable, the only code that makes
  nixos-config fall back to fuzzel) on an owner-only socket, nucleo
  ranking plus frecency, `.desktop` files and `$PATH` rescanned on a
  short-lived thread at each open, launches in a `systemd-run --user
  --scope` (detached fallback), keyboard bound only while open
  (libxkbcommon now linked). A bar fix landed alongside it: with no
  active window, the right-hand modules now stay right instead of
  collapsing left (611f103). Measured in **release** (headless sway, 300
  synthetic apps; refreshed on the tray branch, 3 runs): open latency
  median 0.8–1.3 ms (max 1.7–2.9 ms), open +2.0–2.1 MB PSS over closed,
  50 cycles +0.1–0.2 MB, 0 wake-ups in 5 s at rest after use (2 in one
  run). A 100,000-line dmenu peaks at +6.6–6.8 MB over closed, back down
  to +0.5 MB one second after a cancel. The daemon's own baseline also grew +0.3–0.7 MB over the
  delivery before this one (plan 1 alone: +524–540 KB; every module:
  +552–724 KB; binary +528 KiB, 5.06 → 5.58 MiB), measured `c256d33` vs
  this delivery back to back in release: a bigger statically-linked
  binary (`nucleo-matcher`, `xkeysym`, `xkbcommon`'s Rust bindings) plus
  `libxkbcommon.so` itself — confirmed via `ldd` and `/proc/<pid>/smaps`
  that only ≈ 60 KB of that growth is the library's own mapping, the rest
  the larger binary. A real one-time cost of the feature, present even
  with `launcher` left out of `modules`, not "startup noise". nixos-config
  switch done (`lanceur-alpheus` merged into Mae's nixos-config); Mae
  using the alpheus launcher daily since 2026-10-05 (her own feedback
  folded in: held terminal on Enter, detached on Ctrl+Enter, apps before
  windows on an empty query); `feat/launcher-1` merged into alpheus
  `main` on 2026-10-05.
  - **Keyboard fix (all popups and the launcher, sway/niri).** A popup closed by a key used to release the keyboard while the key was down, and SCTK's repeat then fired forever; the keyboard now goes once its keys are up or its focus is dead (`popup::KeysDown`, the entered surface tracked).
  - Deferred minors (progress notes in the worktree): a media popup tick may lag ≤ 1 s after a Seeked; `fit` builds its lines on each refresh while open; RTL titles in logical order (README).
- **Bar step 5, delivery 2 (notifications)** (spec
  [`docs/specs/2026-10-09-alpheus-notifications-design.md`](docs/specs/2026-10-09-alpheus-notifications-design.md),
  plan [`docs/plans/2026-10-09-alpheus-notifications-plan-1.md`](docs/plans/2026-10-09-alpheus-notifications-plan-1.md))
  **implemented and merged into `main` on 2026-10-09** (per-task reviews, a three-slice final review and two fix waves; the keyboard fix `fix/keyboard-drop` merged first); Mae's trial done ("ça marche"). Replaces mako:
  - **Server.** `org.freedesktop.Notifications` requested with flags 0 (queued behind mako, takes over on release; a mako started later is refused; NameAcquired/NameLost from the bus driver only), calls decoded raw (`image-data` never read), broadcast `NotificationClosed`/`ActionInvoked`; caps, markup stripped, 1 s merge window, a 10-token bucket per app, a 50-entry history in memory.
  - **Bell.** Neutral, in the status block, never dropped: `󰂚 N` unread, crossed under DND, dim when not serving; left click the panel, middle click DND; `alpheus-launch --dnd on|off|toggle`, `--clear-notifications`; DND persisted in `$XDG_STATE_HOME/alpheus/dnd`.
  - **Bubbles.** One Overlay layer surface on the focused output, top centre under the bar, created with the first bubble and dropped with the last (its own pool, no keyboard, an input rect per bubble); moved to the focused output only when the stack changes; left click default action (then dismissed unless resident), right click dismiss; critical ones first (at most `max_visible` × 2 live, the oldest demoted), a waiting bubble's timeout starting when it shows; one DeadlineTimer on the earliest finite expiry.
  - **Panel.** The media card (the media popup's rows and keys, its 1 s tick reused), the history newest first, Do not disturb, Clear all; opening marks everything read.
  - **Measured in release** (vs `900a519`, back to back, 3 runs each, separate target directories): one-time cost +122–248 KB (binary 5.34 → 5.47 MiB (+140 KiB stripped)), the module serving 0–4 KB (mode `auto`: the portal's link already open), a 50-entry history 192–198 KB, 4 bubbles 326–774 KB (−84…+76 KB once closed), 0 loop wake-ups at rest with a sticky bubble and after expiry; mako measured 6.7 MB on Mae's session.
  - **nixos-config side: done** (checked 2026-10-11 on nixos-config origin/main 56f2ee4: shell-ctl `alpheus:dnd-*`, mako neither relaunched nor activatable under alpheus, `alpheus:notif-restore` refused, Mod+Shift+V not bound under alpheus; tests AL3, W3/W4, TN1). Former notes:
    - *DND.* `tools-menu` runs `shell-ctl dnd-toggle`, gaming mode `shell-ctl dnd-on`/`dnd-off`, the TUI `shell-ctl dnd-status`/`dnd-on`/`dnd-off`; under `alpheus` they reach shell-ctl's classic branches (`makoctl mode -a/-r do-not-disturb`). One change covers all three: `alpheus:dnd-on|off|toggle|status` branches in shell-ctl calling `alpheus-launch --dnd on|off|toggle`; `status` reads the file (`$XDG_STATE_HOME/alpheus/dnd` exists → on, else off).
    - *mako.* Not started by systemd: `restore-wallpaper` (`home/scripts/wallpaper.nix`) runs `pkill mako; mako &`, and `services.mako.enable` keeps its D-Bus activation file (a Notify with no owner starts it). Under the alpheus shell the relaunch must stop and mako should not stay activatable (as under `dms`): mako first means alpheus waits in the queue; a mako started after alpheus is refused and exits ("File exists", sd-bus asks with DO_NOT_QUEUE).
    - *History.* `notif-history` (Mod+Shift+V, `makoctl history`) and `shell-ctl notif-restore` (`makoctl restore`) break without mako: drop them, or use the alpheus panel, which opens only from the bell (Mae, 2026-10-09: no verb to open it).
- **Bar step 5, delivery 3 (the OSD)** (spec
  [`docs/specs/2026-10-09-alpheus-osd-design.md`](docs/specs/2026-10-09-alpheus-osd-design.md),
  plan [`docs/plans/2026-10-09-alpheus-osd-plan-1.md`](docs/plans/2026-10-09-alpheus-osd-plan-1.md))
  **implemented and merged into `main` on 2026-10-09** (per-task reviews, a two-slice final review and its fix wave); Mae's trial done ("c'est bien"); replaces swayosd (7.6 MB PSS on Mae's session). The nixos-config switch (shell-ctl, swayosd-server) is Mae's decision; note: `shell-ctl.sh` maps `classic|alpheus` to one pile, so an `alpheus` pile must be split out first:
  - **Passive sound.** The pw-dump model now follows the default source and both defaults' names; `osd::Watch` shows a pill on a volume or mute change from any app or key, never for the start-up state or within 1 s of a device appearing; a default sink switch shows its name (a shown card follows a second switch or a key inside that second; a card whose volume arrives later still shows). **The microphone shows only when its mute flips** (Mae, 2026-10-09: WebRTC apps move the capture gain during calls); the old 500 ms rule after a sink pill is gone. Without `osd` in `modules`, the sound source publishes the sink's volume only (no wake-up for a microphone or a label); with it, never a microphone level alone.
  - **Brightness on request.** `alpheus-launch --osd brightness` reads `<backlight_dir>/<device>/{brightness,max_brightness}` once (`read_capped`), never writes, never watches (hypridle's dimming shows nothing).
  - **The pill.** One Overlay surface bottom centre on the focused output, empty input region, own pool, created on show and dropped 1.3 s after the last change by one `DeadlineTimer`; a held key keeps one surface; a theme change repaints it; another pill (brightness, then a volume change) replaces its content in place and pushes the deadline back.
  - **Measured in release** (vs `1662cb4`, back to back, 3 runs each, separate target directories): one-time cost within noise (−28…+172 KB) (binary 5.48 → 5.54 MiB, +56 KiB stripped), module at rest 0–96 KB, one pill 4–30 KB, −38…−12 KB once gone, 0 loop wake-ups at rest after it (0 for the whole daemon too). Absolute PSS on this harness ≈ 10.9–11.0 MB at 1 output (it drifts between boots: trust the deltas).
  - **nixos-config side: done** (checked 2026-10-11 on origin/main 56f2ee4: `alpheus:start` without swayosd-server, volume/mic on wpctl, brightness then `--osd brightness`, swayosd not installed under alpheus; tests AL1/AL2). Former notes:
    - *shell-ctl.* The alpheus pile: volume and mic keys on `wpctl`, brightness keys on `brightnessctl` then `alpheus-launch --osd brightness`; `alpheus:start` becomes `( restore-wallpaper & ); exit 0` (no `swayosd-server`). Until then both swayosd and the alpheus OSD pop, since `osd` is in the default `modules` and Mae sets none.
    - *Tests and docs there.* `COMPORTEMENTS.md` AL1/AL2 and `tests/shell-ctl-test.sh`'s AL1/AL2 cases; stale comments in `shell-ctl.sh` (header, `pile=classic`), `modules/desktop/desktop-shell.nix` (header, `description`), `hosts/p16/default.nix` (the pile comment), `keybinds.conf` and `keybinds.lua` ("route vers swayosd").
    - *Switching.* The running `swayosd-server` needs a `pkill`, or a re-login.
    - *Memory figures.* The README's 49.5 MB for swayosd is replaced by 7.6 MB PSS; the hybrid-mode row is recomputed (≈ 23 MB now, ≈ 15 MB after the switch); the waybar stack's ≈ 120 MB (DMS trial spec, 2026-09-30: swayosd 48, swaybg 31 MB as PSS) does not match later PSS measurements, restated as ≈ 55 MB from sourced figures until measured again side by side.
- **Quick settings, delivery 1** (the panel, sound and simple settings; spec
  [`docs/specs/2026-10-11-alpheus-quick-settings-design.md`](docs/specs/2026-10-11-alpheus-quick-settings-design.md),
  plan [`docs/plans/2026-10-11-alpheus-quick-settings-plan-1.md`](docs/plans/2026-10-11-alpheus-quick-settings-plan-1.md))
  **implemented and merged into `main` on 2026-10-11** (per-task reviews, a two-slice final review and its fix wave; Mae's trial pending):
  - **Opening.** A left click on volume, network or Bluetooth opens a `PopupBody::Quick` under the module (the right click now opens the TUI tab); `alpheus-launch --quick [sound]` opens it under the bar's right end on the focused output, the keyboard taken without a grab (the bar `Exclusive` while it is open — Mae's ruling: a click on a window does not close it; on Hyprland < 0.56.2 a click inside may close it); a verb within 250 ms of the verb's own open is ignored (`quick: toggle debounced`, the launcher's `TOGGLE_DEBOUNCE`: Hyprland re-fires a press bind), and the verb again closes it as `popup closed (verb)`. One popup at a time; Esc, a click elsewhere, a restyle or the output going close it.
  - **Rows.** Volume (› the outputs, `wpctl set-default <id>`), brightness (`[osd] backlight`, re-read 150 ms after `brightnessctl`, never below 1 % from the panel — Mae's ruling), microphone, do-not-disturb (`set_dnd`), power profile (`net.hadess.PowerProfiles`, read only: a fourth D-Bus domain; `powerprofilesctl set`), keep awake and night light (`[quick]` toggles, state `file:`/`process:` re-read 300 ms after), lock / log out / reboot / power off (reboot and power off ask for 3 s, and the confirming activation counts only 0.4 s after the question — Mae's ruling: a double click never confirms). A row whose tool, hardware or command is missing is hidden. With `quick` alone in `modules`, the D-Bus thread may start for the profile.
  - **How.** `quick.rs` (pure model), `quick_state.rs`, `quick_app.rs` (glue, spawns through the executor); the sink list through a `SoundTap` shared with the sound thread, `Scope::Panel` only while open; one `DeadlineTimer` (`quick_timer`).
  - **Measured in release** (vs `d483590`, back to back, 3 runs each, separate target directories): one-time cost +64…+196 KB with `quick` not in `modules` (the `everything` case of `startup_pss_per_source`; binary 5.59 → 5.70 MiB, +108 KiB stripped), module on and closed 96–108 KB, open 444–448 KB, +340–344 KB over idle once closed (the first open's one-time warm-up: `smaps` shows the binary's code pages ≈ +320 KB and the font file's ≈ +64 KB, heap and pool back to idle's; the test splits it into `closed <= idle + 512` and `cycled <= closed + 128`), 20 more cycles −18…+36 KB over closed, 0 loop wake-ups at rest after. Prove-it-bites note: a one-shot 1 s timer left armed by the close only shows if the rest window starts 0.3 s after it (the test does).
  - **To confirm with Mae at the trial:** the plan's "Decisions written down" (spec gaps closed during planning; decision 1 already accepted by her, decision 6 amended at the final review: the selection band spans a slider's whole row).
  - **Pending (Mae's decision, never edited from here):** her `[quick]` toggles and logout in `bar.toml`, and a key for `alpheus-launch --quick` — the French note in `.superpowers/sdd/quick/note-bar-toml.md`; after the switch, `shell-ctl reload-bar`. Delivery 2 (Wi-Fi) below.
- **Quick settings, delivery 2 (Wi-Fi)** (spec
  [`docs/specs/2026-10-11-alpheus-quick-settings-wifi-design.md`](docs/specs/2026-10-11-alpheus-quick-settings-wifi-design.md),
  plan [`docs/plans/2026-10-11-alpheus-quick-settings-plan-2.md`](docs/plans/2026-10-11-alpheus-quick-settings-plan-2.md))
  **implemented and merged into `main` on 2026-10-13** (per-task reviews, a two-slice final review and four fix rounds; Mae's trial pending, note in `.superpowers/sdd/quick/note-wifi.md`):
  - **The row and the detail.** A Wi-Fi row first under the sliders (glyph by signal, the SSID, on or off; Space: `nmcli radio wifi on|off`); its detail by Enter, →, a click, the network module or `alpheus-launch --quick wifi` (one `nmcli device wifi rescan`): one row per SSID, connected then saved then others by signal, at most 32, ●, four steps, the lock, `saved` (bars and lock in one column; the tag room is the widest tag's, `failed`, so an SSID keeps 16 cells); `More…` runs `[quick] wifi_more` (default: launcher.toml's terminal + nmtui).
  - **Connecting.** Connected: `connection down uuid` (the active connection's own); saved: `connection up uuid`; open: `device wifi connect <ssid>`; a new WPA-personal one: a Password row (a boxed 256-byte buffer wiped with volatile writes), `nmcli --ask device wifi connect <ssid>` with the password on stdin (`Executor::spawn_with_input`), never argv, log or Debug; typed characters also pass through SCTK's per-key heap string, which is not wiped (decision 14). `…` (**Mae's ruling**, 2026-10-12: short tags; was `connecting…`), then ● once activated; `failed` shown 4 s, with one `quick: wifi connection failed (<why>)` line (never an SSID or a password): `the activation failed` (device state 120, or leaving an activating state), `no answer in 20 s`, `still activating after 60 s`. NetworkManager gone is a log line only, `NetworkManager went away`: `Outcome::Gone` clears `failed`, the detail closes and the row hides, nothing is cleaned up. **Mae's ruling:** an attempt on a new network outlives the panel (a follower on the app, up to 20 s, 60 s while the device is still activating) so the profile that appeared during the attempt is deleted when it fails, only if still saved; a slow network shows `failed` at 60 s but keeps its profile. The cleanup never deletes under doubt: a NetworkManager restart (the saved list unknown until read again from the new owner) or a profile reserved to another user (skipped) deletes nothing, and **Mae's ruling:** a network-domain outage during a followed attempt ends it with no cleanup (the safe side; in that rare case a wrong-password profile may remain, removed via `More…`). During a NetworkManager-domain outage the Wi-Fi row hides and an open detail closes (its password field wiped); a panel opened during the backoff shows no Wi-Fi row until the reconnection. Only the network domain does this: a BlueZ, UPower or power-profile outage keeps the Wi-Fi, the detail and the field. That kept state is marked stale until the next session reads it: an attempt expiring on it shows `failed` but keeps its profile (`no fresh state since an outage`). One attempt at a time. Residual: while the saved list is unknown (failed listing, an unreadable profile, more than 64 profiles), Enter on an already-saved network may create a duplicate profile, never deleted (remove it via `More…`).
  - **How.** Reads on the D-Bus thread's one read-only connection, only while the panel is open: a `WifiTap` (scope Closed/Row/Detail + a socket pair the thread polls beside the bus, no thread, no timer), `WifiReader` (GetDevices, DeviceType, the device's active access point and `Device.ActiveConnection`'s `Uuid`; with the detail ListConnections, GetSettings, AccessPoints and five `WIFI_RULES` added and removed with it); `RemoveMatch` joined the method allowlist (a bus-driver call, nothing written on NetworkManager; `GetSecrets` never). `Msg::Wifi` goes to the panel, never the bar. The `quick` module now wants the network domain (alone in `modules`, NetworkManager is followed; with the default modules nothing changes), and `WirelessEnabled` re-reads that domain on a radio toggle even with the panel closed (event-only, no polling).
  - **Measured in release** (vs `dbb90b8`, back to back, 3 runs each, separate target directories; re-measured after the final fix wave): one-time cost +176…+372 KB over the nine pairings (binary 5.70 → 5.83 MiB, +136 KiB stripped): code linked in, unused — smaps at startup show the binary's resident code pages +244…+308 KB and its data +24 KB, heap, fonts and libraries unchanged within noise. The detail open on 40 access points (32 listed) 898–910 KB over the module at rest; +476…+488 KB once closed, which smaps splits into the binary's code pages +128, the font file's +320 and the main stack +20 (touched by the first open), the main heap +68…+76 (most likely the painter's glyph caches, bounded; attributed by elimination) and the shm pools −56; the D-Bus thread's arena is back to idle's. The test bounds what the daemon keeps without the shm pools (the compositor maps them too; their PSS moved −56…+104 KB between two readings): closed ≤ idle + 640 KB (measured +532…+544; +548…+556 since the total and the pools are read from one smaps pass), 10 more cycles ≤ +128 KB (measured +36…+72); the main heap still creeps slowly, +4…+16 KB per 10 cycles after the first 10 (a 40-cycle run), within that leak bound. The pools have their own, looser bound: closed ≤ idle + 128 KB (measured −56 in each of 3 runs). 0 loop and 0 D-Bus-thread wake-ups at rest after; that rest window now starts within a second of the close, so a stray one-second timer always shows. A close and a reopen the D-Bus thread reads as one poke are a new open since the final fix wave (the memory tests no longer wait between them).
  - **Unverified until Mae's trial:** `nmcli --ask` reading the password from a pipe, and polkit (does `nmcli` change Wi-Fi from her session without a prompt?).
  - **Pending (Mae's decision, never edited from here):** `wifi_more`, a key for `alpheus-launch --quick wifi` — the French note in `.superpowers/sdd/quick/note-wifi.md`; after the switch, `shell-ctl reload-bar`. Delivery 3 (Bluetooth) below.
- **Quick settings, delivery 3 (Bluetooth)** (spec
  [`docs/specs/2026-10-20-alpheus-quick-settings-bluetooth-design.md`](docs/specs/2026-10-20-alpheus-quick-settings-bluetooth-design.md),
  plan [`docs/plans/2026-10-22-alpheus-quick-settings-plan-3.md`](docs/plans/2026-10-22-alpheus-quick-settings-plan-3.md))
  **implemented and merged into `main` on 2026-10-23** (per-task reviews, a two-slice final review and its fix wave; Mae's trial pending, French note in `.superpowers/sdd/quick/note-bluetooth.md`):
  - **The row.** A Bluetooth row under the Wi-Fi row: the first connected device, else `on`, `off` or `blocked` (rfkill); Space powers the adapter (on a blocked radio, `rfkill unblock bluetooth` first, then power once BlueZ reports it unblocked); hidden without BlueZ or an adapter. A left click on the `bluetooth` module or `alpheus-launch --quick bluetooth` opens the panel on the detail.
  - **The detail.** Paired devices (● connected, then `paired`, by name), then `Nearby` and the named devices found while it is open (inserted by RSSI once, never moving); discovery only while it is open; at most 64 devices read, 32 rows. Enter disconnects a connected device, connects a paired one, pairs a nearby one (then `Trusted`, then `Connect` unless already connected); one action at a time (`…`), `failed` 4 s with one `quick: bluetooth <action> failed (<D-Bus error name>)` line (`(timeout)` after 30 s per call), never a name, address or object path in a log. Forget: Delete or a right click asks `Forget? ⏎` (the power menu's 0.4 s / 3 s rule), then `RemoveDevice`.
  - **Pairing codes.** alpheus's own `org.bluez.Agent1` (`KeyboardDisplay`, `RegisterAgent`, never `RequestDefaultAgent`), answering only BlueZ's unique name and only for the device being paired: `Type 482 913 then ⏎` under the device for a passkey or PIN to type; `Code 482 913? ⏎` to confirm (Enter OK, anything else or closing Rejected); a code to type into alpheus, authorization or a service: Rejected. Closing the panel or leaving the detail mid-pairing replies, cancels the pairing and unregisters the agent, in that order.
  - **The headset row.** With a Bluetooth headset as the default sink, `‹ Hi-Fi ›` / `‹ Mic ›` between the volume and the brightness switches its PipeWire profile (`a2dp-sink*` / `headset-head-unit*`, the highest-priority available one) with `wpctl set-profile <device id> <index>`; `pipewire.rs` keeps the bluez5 devices and their profiles (≤ 8 × 32) from the `pw-dump` child the sound source already runs.
  - **How.** Reads on the D-Bus thread's read-only connection, only while the panel is open: a `BtTap` (scope Closed/Row/Detail, its own socket pair the thread polls beside the bus) and a `BtReader` (`sources/dbus/bt.rs`; no match rule added). Acts through `sources/dbus/bt_act.rs`, a second system-bus connection on the event loop (the read-only guardrail's one named exemption): a typed `ActMethod` allowlist pinned by a guardrail (`GetNameOwner`; `Properties.Set` on `Adapter1.Powered` and `Device1.Trusted`; discovery, `RemoveDevice`, `Connect`, `Disconnect`, `Pair`, `CancelPairing`; `RegisterAgent`/`UnregisterAgent`), every call to `org.bluez`'s unique name with auto-start off, open only while the detail is open or a call is in flight. `quick_bt.rs` (pure model: row, detail, tags, codes, the forget question through `session::ask`/`expire`) and `bt_app.rs` (glue); the 30 s deadlines, the 4 s `failed` and the 3 s question on the panel's `quick_timer`. No thread, no timer, no dependency.
  - **Measured in release** (vs `9401cec`, back to back, 3 runs each, separate target directories): one-time cost +12…+208 KB over the nine pairings (`everything` of `startup_pss_per_source`, no `quick` there; binary 6.07 → 6.24 MiB, +168 KiB stripped): code linked in, unused. The detail open on 20 devices +1.1–1.8 MB over the module at rest (8 runs; ≈ 8.3–8.4 MB at rest, 1 output), split (4 runs) into the daemon's own +436…+540 KB and the shm pools +774…+1,240 KB, the moving part (most likely the detail growing one row per device found while open, its buffers re-made at each height; all of it gone once closed); +412…+516 KB of the daemon's own once closed (the first open's code, font and glyph-cache pages, as for Wi-Fi), the pools back to idle's; 10 more cycles +40…+64 KB. The acting connection +54…+116 KB (the detail with it against the rows without it, 5 runs), 6 threads either way, 0 loop wake-ups in 5 s while it idles. The headset row +12…+20 KB (5 runs). 0 loop, 0 D-Bus-thread and 0 whole-daemon wake-ups at rest after the detail closed (5 runs). The plan's bound `open − idle ≤ 1,536 KB` was exceeded in 3 of 8 runs (1,736–1,788 KB): the tests bound the daemon's own memory (≤ +768 KB) and the pools (≤ +1,536 KB) separately — the controller's and Mae's call.
  - **Unverified until Mae's trial:** BlueZ's D-Bus policy on NixOS (`bluetooth.conf`, no polkit) letting her session user call `Pair`, `Connect` and `RegisterAgent` with no `AccessDenied`; a real pairing of her keyboards (`DisplayPasskey`); the headset mode switch with WirePlumber's profile names on her headset; one left click on a connected device disconnects it (no question); the A2DP→HFP switch makes the headset row flicker (the sink goes and comes back).
  - **Pending (Mae's decision, never edited from here):** a key for `alpheus-launch --quick bluetooth` — the French note in `.superpowers/sdd/quick/note-bluetooth.md`; after the switch, `shell-ctl reload-bar`.
- **Launcher command sources** (spec
  [`docs/specs/2026-10-10-alpheus-launcher-sources-design.md`](docs/specs/2026-10-10-alpheus-launcher-sources-design.md),
  plan [`docs/plans/2026-10-10-alpheus-launcher-sources-plan-1.md`](docs/plans/2026-10-10-alpheus-launcher-sources-plan-1.md))
  **implemented and merged into `main` on 2026-10-10** (per-task reviews, a two-slice final review and its fix wave); Mae's trial done on 2026-10-10 ("ça marche bien"):
  - **Sources.** `[[sources]]` in `launcher.toml` (≤ 8; name, `list`, `run` with `{line}` as a whole element). `list` runs once per open on the scan thread, after the apps are delivered, through `Executor::capture` (1 s, 2,000 lines, 256 KiB, group killed and reaped). Lines cleaned (`text::invisible`, controls), split on ` · `; delivered second (`Mail::Entries`), adopted under the catalog's rule; one run at a time across opens.
  - **Mod+Space.** Entries only in All on a typed query; entry hits as is, hits needing the path (the whole line) below every other hit; ties Window, App, Entry, Command; frecency `source:<name>:<line>` (history ids up to 648 bytes). Row: the entry, its path dimmed (undimmed on the selection band, as `list::row` draws secondary text). Enter/Ctrl+Enter run `run` through the usual spawner.
  - **Mod+X.** `alpheus-launch --dmenu --visible N`: the first N non-empty lines on a blank query, every line by typing; refused with `--password`; an older daemon → exit 3.
  - **Final-review fixes.** `capture` checks the command's exit while data flows too and kills the group once it is gone (a chatty child left behind no longer reads to the deadline); source names refuse control characters and U+FFFD. Open cost at the bounds ≈ 6–8 MB (3× each kept line, released on close); a daemon killed mid-read leaves a list script's background children to systemd's `KillMode=control-group`.
  - **Measured in release** (vs `32711f5`, back to back, 3 runs each, separate target directories): one-time cost within noise (−4…+92 KB closed, ≈ +0.1–0.2 MB open) (binary 5.54 → 5.59 MiB stripped), open with a 300-line source +2.4–2.5 MB over closed (+1.9–2.4 MB without), entries 15–17 ms after the request (median of 21 opens), open latency 1.7–2.2 ms median against 1.6–1.9 ms with no source (the harness drifts between runs: trust the deltas), 0 wake-ups at rest after.
  - **nixos-config side done** (2026-10-10, at Mae's request, pushed as 89932d7 and 56f2ee4): `tools-menu --list` with three-level fixed choices, direct runs of them, the alpheus home menu with `--visible 11`, `lib.sh` `menu()` forwarding `--visible` to alpheus only, the alpheus input bumped. `launcher.toml` is not generated by nix: Mae adds `[[sources]]` by hand. Lesson from the trial: after a switch the running daemon stays the old binary until `shell-ctl reload-bar`; an old daemon makes `--visible` fall back to fuzzel (exit 3), as designed.
- **Mango and Sway backends** (spec
  [`docs/specs/2026-10-13-alpheus-mango-sway-design.md`](docs/specs/2026-10-13-alpheus-mango-sway-design.md),
  plan [`docs/plans/2026-10-13-alpheus-mango-sway-plan-1.md`](docs/plans/2026-10-13-alpheus-mango-sway-plan-1.md))
  **implemented and merged into `main` on 2026-10-13** (per-task reviews, a two-slice final review and its fix wave; Mae's trial pending, French note in `.superpowers/sdd/mango/note-essai.md`; the Displays tab no longer waits on `wm-set` since the displays protocol delivery, below):
  - **Detection.** `SWAYSOCK`, `HYPRLAND_INSTANCE_SIGNATURE`, `MANGO_INSTANCE_SIGNATURE`, `NIRI_SOCKET`, in nixos-config's order, with niri last, in the daemon and the TUI; one start line names the backend (`compositor: Sway ($SWAYSOCK)`).
  - **Mango.** Two read-only connections to `$MANGO_INSTANCE_SIGNATURE` (`watch all-monitors`, `watch all-clients`), one `poll` without timeout; one workspace per (output, tag), empty inactive tags hidden, several active tags visible, the overview shows none (an output left with no tag keeps its lowest one, dim, so its bar never shows another output's tags); the active output's client as the title; a focus history the model keeps for the launcher (`id:<client id>`). The path is read again at each session; a line nested past `MAX_NESTING` is unreadable, as under Sway.
  - **Sway.** The i3 IPC on `$SWAYSOCK`: SUBSCRIBE to workspace and window events on one connection, GET_WORKSPACES and GET_TREE on the other, once per burst (at most 256 events drained before a re-read); counts, title (app_id or the X11 class) and the launcher's list (`id:<con_id>`) from the tree, the scratchpad skipped; a tree nested deeper than about 44 containers reads as unreadable (`MAX_NESTING` 90).
  - **Order:** under Mango, the launcher lists windows as under Hyprland (the previous one first, the focused one last), from the focus history the model keeps; under Sway, in the focus order Sway keeps per container, previous first, focused last.
  - **Measured in release** (vs `2b137f0`, back to back, 3 runs each, separate target directories): one-time cost +132…+140 KB (binary 5.83 → 5.97 MiB, +140 KiB stripped), startup with every module on 3 outputs: Sway 9145–9233 KB, Mango 9065–9117 KB (the fake Hyprland 9161–9201 KB), 6 threads each, 0 compositor-thread wake-ups at rest under both (the whole daemon: 0, except 2 once under Mango, in run 1, the full memory suite, cause not investigated; the compositor thread 0 throughout).
  - **Pending (Mae's decision, never edited from here):** the trial under Sway and Mango (`.superpowers/sdd/mango/note-essai.md`); after the switch, `shell-ctl reload-bar`. Then the ⏻ power button (below).
- **The ⏻ power button** (spec
  [`docs/specs/2026-10-13-alpheus-power-menu-design.md`](docs/specs/2026-10-13-alpheus-power-menu-design.md),
  plan [`docs/plans/2026-10-13-alpheus-power-menu-plan-1.md`](docs/plans/2026-10-13-alpheus-power-menu-plan-1.md))
  **implemented and merged into `main` on 2026-10-13** (per-task reviews, a final whole-branch review and its fix wave; Mae's trial pending, French note in `.superpowers/sdd/power/note-essai.md`):
  - **The button.** `power`, the last of `ModuleId::ALL`: the default bar ends with the clock then `⏻`, its own block, no hue, no fallback face, the last module removed on a narrow bar; no button when `[quick]`'s lock, logout, reboot and poweroff are all empty.
  - **The menu.** A left click toggles an `xdg_popup` under it (`Input::Grab`): Lock, Log out, Reboot, Power off, one row per command set (logout is empty by default). Nothing selected at open; ↓/Tab the first, ↑/Shift+Tab the last, clamped; Enter, Space or a left press activate; hovering selects; Esc or a click outside closes. Reboot and power off ask (`⏻ Power off? ⏎` in warn) and run on a second activation from 0.4 s to 3 s; any key but Enter and Space, another row or 3 s cancel the question; a held key repeats only to move.
  - **How.** `session.rs` (the entries and the rule, shared with the quick panel, whose tests did not change), `power.rs` (pure), `power_app.rs` (the glue: `[quick]`'s argv through the executor, log `power: <word>`, never the command; `power_timer` armed only while a question is pending, released by `popup_close`).
  - **Measured in release** (vs `242c686`, back to back, 3 runs each, separate target directories): one-time cost −8…+84 KB over six pairings; one run read +288…+336 KB on all three of its pairings (its resources step moved too; a boot shift believed, not proven) (binary 5.97 → 5.97 MiB, +8 KiB stripped), the button at rest 184–192 KB (≈ +128 KB new pages of the same Nerd Font file, ⏻ U+23FB being in it, and ≈ +64 KB the module's code pages: file-backed, clean, one-time), the menu open 432 KB, +280 KB over idle once closed, +12 KB after 20 more cycles, 0 loop wake-ups at rest after a close with the question pending.
  - **Pending (Mae's decision, never edited from here):** `[quick] logout` in her `bar.toml` (empty by default, so Log out shows only once she sets it); the plan's "Decisions written down", decision 13 (no window module: ⏻ alone at the right end); after the switch, `shell-ctl reload-bar`. Next: the privacy indicators.
- **Privacy indicators, delivery 1** (spec
  [`docs/specs/2026-10-16-alpheus-privacy-design.md`](docs/specs/2026-10-16-alpheus-privacy-design.md),
  plan [`docs/plans/2026-10-16-alpheus-privacy-plan-1.md`](docs/plans/2026-10-16-alpheus-privacy-plan-1.md))
  **implemented and merged into `main` on 2026-10-20** (per-task reviews, a final whole-branch review and its fix wave; Mae's trial pending, French note in `.superpowers/sdd/2026-10-16-alpheus-privacy-plan-1/note-essai.md`):
  - **The module.** `privacy`, after `media` in `ModuleId::ALL` and in the default `modules`; nothing drawn at rest. Its own block, one glyph per capture: `󰍬` microphone (orange), `󰄀` camera (green), `󰍹` screen (red), always, whatever `module_colors` says; the microphone dim while the default source is muted; removed on a narrow bar just before `power`. A left click opens an `xdg_popup` (`Input::Grab`) listing the apps per indicator (`(unnamed)` when only the compositor knows; not selectable; Esc closes it, and it closes with `popup closed (nothing captured)` once everything is off), a middle click runs `wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle`. `[privacy] ignore = []` (whole app names, any case).
  - **How.** `pipewire.rs` keeps, only when built for privacy, the capture graph (nodes' class, state and name fields, links; at most 1,024 nodes and 4,096 links) from the `pw-dump -m` child the sound source already runs; `privacy.rs` (pure: the rules, the merge, the off-delay, the segment and popup content) and `privacy_app.rs` (the click path, the popup, the timer). The microphone is a link from an `Audio/Source` to a `Stream/Input/Audio` that is active or running, excluding meters (`stream.monitor`), visualisers (`stream.capture.sink`), link-group members and passive nodes; the camera a `Video/Source` of a v4l2 or libcamera device (or the Camera role) to a video consumer; the screen any other video producer (portal casts, niri's). The compositors add the screen: niri's cast events, Hyprland's `screencast>>`/`screencastv2>>` (a release counts after a 1 s hold, so a screenshot blip never shows), Sway and Mango nothing. An indicator going off stays 0.5 s (one `DeadlineTimer`, armed only while one is pending).
  - **Rulings in the plan's "Decisions written down" (1–9),** among them: the dim microphone glyph also in the popup's heading; screencast lines and niri's cast map are read even without `privacy` in `modules` (one string compare, an empty map unless a cast runs); the compositor events name no app, so a screen lit by them alone reads `(unnamed)`. Task 6's headless test found a real bug (a pending off changed no shown state, so the off-delay's timer never armed): fixed in `launch.rs` (`0051b5f`), now in `App::privacy_feed` and pinned by a source guardrail (final fix wave).
  - **Measured in release** (vs `457cf5d`, back to back, 6 runs each for the start, 3 for the rest, separate target directories): linked in but `privacy` not in `modules`, −80…+116 KB PSS at start over nine pairings (`plan 1` −160…+60; binary 5.97 → 6.05 MiB, +76 KiB stripped): the smaps split shows the binary's code pages +68 KB, its read-only data −56, heap −12…+36, shm pools equal, other anon one 112 KB step either way in both trees — code linked in, unused, nothing allocated. Task 7's first reading, +344…+440 KB (`plan 1` +200…+320, one run +890), came from base runs ≈ 0.4 MB low against the same commit and binary: a harness shift, not reproduced in the final fix wave; module on at start −88…+120 KB over the same bar without it, on the full-size initial dump the fake `pw-dump` serves (the spec's "large graph" figure); a capture shown −60…+26 KB, −50…−28 KB once ended, 20 more cycles +16…+30 KB; 0 loop wake-ups at rest after a capture (0 for the whole daemon).
  - **Not seen:** wf-recorder under Sway and Mango. A camera opened directly (`/dev/video*` outside PipeWire: Chromium, most V4L2 apps) is **delivery 2** (below): an inotify watch on the device nodes, then one debounced `/proc/*/fd` scan to name the holder, never polling.
  - **Deferred minors** (ledger in `.superpowers/sdd/2026-10-16-alpheus-privacy-plan-1/progress.md`): the `+ privacy` ≤ everything + 256 KB bound may need 384 KB on a flaky harness; `at_rest_no_wakeups_after_a_capture` is not selected by the `privacy` test filter (run it with `--exact`).
  - **Pending (Mae's decision, never edited from here):** `privacy` is in the default `modules` (a `modules` of her own needs it added after `media`); after the switch, `shell-ctl reload-bar`.
- **Privacy indicators, delivery 2** (a camera opened outside PipeWire; spec
  [`docs/specs/2026-10-20-alpheus-privacy-camera-design.md`](docs/specs/2026-10-20-alpheus-privacy-camera-design.md),
  plan [`docs/plans/2026-10-20-alpheus-privacy-plan-2.md`](docs/plans/2026-10-20-alpheus-privacy-plan-2.md))
  **implemented and merged into `main` on 2026-10-20** (per-task reviews, a final whole-branch review and its fix wave; Mae's trial pending, French note in `.superpowers/sdd/privacy/note-essai-camera.md`):
  - **What.** The camera glyph (green) also lights for a `/dev/video*` held by a process that never goes through PipeWire; the popup lists its name (`comm`), merged and de-duplicated with PipeWire's. Only with `privacy` in `modules`; `[privacy] dev_dir`/`proc_dir` (absolute, default `/dev` and `/proc`) say where devices and processes are listed.
  - **How.** `camera_dev.rs` (pure: the scan and the record filter) and `camera_app.rs` (glue): one inotify fd on the event loop, a watch on `dev_dir` (create, delete, attrib) and one on each `video*` node (open, close), at most 64; events are triggers, never counts. A trigger moves one `DeadlineTimer` to now + 300 ms, capped at 1 s after the burst's first trigger (`SCAN_MAX_WAIT`, `scan_deadline`; so a process opening and closing a node non-stop cannot postpone the scan forever); when it fires, one scan reads `/proc/<pid>/fd` links of the daemon's own uid (never opening anything under `dev_dir`; at most 4,096 pids and descriptors each; PipeWire, WirePlumber and the daemon itself dropped) and sets the devices' holders; delivery 1's 0.5 s off-delay then applies. One scan at start (a camera held across a restart shows at once). No new thread, no polling, nothing without `privacy`.
  - **Measured in release** (vs `5d8fd55`, back to back, 3 runs each, separate target directories): one-time cost −236…+100 KB (`everything`; one base run read 9.35 MB, a boot shift); module on at start +84…+192 KB (`+ privacy`; retaken once the harness points every test daemon's `[privacy]` at empty directories) over about 9.0–9.2 MB, binary 6.05 → 6.07 MiB (+24 KiB stripped); a holder shown −38…+70 KB over idle, 0…+4 KB once gone, +20 cycles ≤ +64 KB; 0 loop and whole-daemon wake-ups at rest after the holder went. **Scan cost (controller's ruling):** the scan stays on the event loop. `camera_scan_duration` asserts a median ≤ 10 ms on 120 processes × 30 descriptors (measured 4.0–4.1 ms, 3 runs, `nice -n 19`), the shape of the workstation's own user (93 processes, 2,726 descriptors, measured read-only; procfs is also faster than the synthetic on-disk tree); a 500 × 20 tree is printed only: median 14–15 ms, max ≈ 20 ms, the worst case seen (un-niced 8–10 ms).
  - **Not seen.** wf-recorder under Sway and Mango (announces nothing), a camera held by another user's process, a holder that opens and closes inside the 300 ms debounce with nothing left to scan.
  - **Pending.** Mae's trial with a real V4L2 app outside PipeWire (nothing run against a real camera here; does a NixOS-wrapped name such as `.firefox-wrappe` bother her in the popup and in `ignore`?); after the switch, `shell-ctl reload-bar`.
- **Displays through the output protocol** (the Displays tab and screen profiles move from nixos-config's `wm-set` to the daemon's `zwlr_output_manager_v1`; spec
  [`docs/specs/2026-10-30-alpheus-displays-protocol-design.md`](docs/specs/2026-10-30-alpheus-displays-protocol-design.md),
  plan [`docs/plans/2026-10-30-alpheus-displays-plan-1.md`](docs/plans/2026-10-30-alpheus-displays-plan-1.md))
  **implemented and merged into `main` on 2026-11-03** (per-task reviews and fix rounds, a two-slice final review and three fix rounds; Mae's trial pending):
  - **The daemon owns the protocol.** `outputs/wlr.rs` binds the manager (versions 1..=4) and is the one place alpheus writes the screens' layout (the guardrail's single exception, spec § 2). The model updates only on the manager's `done`; one configuration in flight (a `done` meanwhile means "evaluate again after the answer", `cancelled` retried 3 times); never a no-op configuration; a 500 ms debounce on hotplug; a 5 s watchdog (one `DeadlineTimer`, armed only while a configuration is pending) for a compositor that never answers; the last screen is never switched off.
  - **The trial lives in the daemon** (15 s, one `DeadlineTimer` armed only during a trial; the TUI dying cannot leave a bad layout). It ends `Kept`, `Reverted`, `ScreenLost`, `ChangedElsewhere`, `Refused` or `NoAnswer`; while any configuration is in flight the snapshot says `Busy` and the tab stays locked.
  - **Profiles on hotplug.** With `[displays] auto = true` (default), the `displays.toml` profile whose screens are exactly the connected ones is applied after the debounce, never during a trial. A screen is matched by identity (`make model serial`, else its `description`; the saved connector is only a hint between two identical screens).
  - **DisplayLink.** Detected per head from `<drm_dir>/card*-<name>/device/device/driver` ending in `evdi` (fallback `device/driver`; `[displays] drm_dir`, default `/sys/class/drm`, read once per head). The 120 Hz rule: no mode above 120 Hz is offered or accepted, and a head the compositor sets above it is lowered by the daemon (the one change made without a request).
  - **Socket verbs.** `outputs`, `outputs-try`, `outputs-keep`, `outputs-revert` on the launcher's socket (`alpheus-commun::launcher_ipc`); the TUI runs no command for displays. The tab is available under **every** compositor that has `zwlr_output_manager_v1` (Hyprland, Sway, Mango, niri): the "not available under Sway/niri" message is gone. The TUI keeps `wm-set` only for `exit`; its three display verbs are no longer called.
  - **Measured in release** (vs `dc4793a`, back to back, 3 runs each, separate target directories): binary 6.25 → 6.49 MiB (+244 KiB stripped); one-time cost `everything` of `startup_pss_per_source` (no `displays` there) **+4…+168 KB** over the nine pairings. `+ sound`, `+ D-Bus` and `+ resources` read higher on the new build (about +124…+504 KB) with no code change on those paths and `displays` absent from those cases: base-side harness spread (the base's `+ resources` read below its `plan 1` in run 1), not a cost. Module on at start +56…+204 KB with 1 output, −36…+36 KB with 3 (`displays_pss_at_start`, bound base + 512 KB); a trial open −46…+8 KB over idle, 20 more trials +0 KB of the daemon's own memory; hotplug cycles: one cycle −40…+0 KB, +40…+44 KB from cycle 1 to 21, **+8 KB between cycles 21 and 41**, +0…+8 KB from 41 to 61 (`displays_pss_after_hotplug_cycles`, own memory = PSS without the shm pools; checkpoint bound 32 KB). The shm pools are printed, not asserted (SCTK `RawPool` remaps on a resize: a bar-side step of 84…220 KB that also shows with the module off). 0 loop and 0 whole-daemon wake-ups at rest after the start, a trial (16 s) and the hotplug cycles (10 s).
  - **Final fix wave (F1, daemon):** a trial open (or a try in flight) is restored when the daemon stops; `outputs-keep`/`outputs-revert` on a daemon that has seen no try answer `no trial in progress (the daemon restarted)`; at most 3 automatic applies per screen set per minute, then one log line, the skipped profile evaluated again when the window of that set's oldest apply ends (F3); the `no profile` log only with at least one profile. A trial is not persisted across a daemon crash (Mae's call); a try still in flight at shutdown may keep its layout (documented limit).
  - **Final fix wave (F2, TUI):** a fresh read with no trial while the tab counts down closes the Keep box (`Displays: the trial ended`, F3); a keep or revert answered `no trial` shows `Displays: the daemon restarted; its trial ended` (F3, no `settings kept` before the answer); another alpheus's open trial locks the tab until it ends; closing waits up to 2 s for a pending try, keep or revert before its own revert.
  - **Final fix wave (F3):** `displays_pss_after_hotplug_cycles` applies a profile on plug only (each plug is a new screen set, never limited; the unplug's lone profile is already shown, so never sent nor counted); one release run after it: own memory 8109 → 8149 → 8161 → 8117 KB at cycles 1/21/41/61, 0 wake-ups at rest.
  - **Parked:** a 64 KiB `displays.toml` raises the heap ≈ 2.8 MB once (the parse peaks ≈ 44× the file's size and glibc keeps the high-water); the file is re-parsed at each debounce end (a cache on inode, mtime and size would fix it). A real file of a few profiles costs 20–60 KB.
  - **Pending (Mae's decision):** the toast copy, `WxH` for an unknown refresh (shown without `@…`; profiles may still store `@0`), the trial-expiry toast styled as an error; after the merge nixos-config can drop `wm-set`'s three display verbs (and Mango's `monitors.conf` wait, `.superpowers/sdd/mango/note-nixos-ecrans.md`).
- **Simplification, second part** (Mae's go, 2026-11-04; proposal in `.superpowers/sdd/cleanup/proposal.md`, ledger in `.superpowers/sdd/groups/progress.md`) **merged into `main` on 2026-11-04** after per-piece reviews and a final review; behaviour unchanged, the release memory suite 49 passed:
  - **`App` grouped per feature:** 95 → 56 fields (`camera_app::Camera`, `quick_app::QuickApp`, `menu_app::TrayMenu`, `appearance::ThemeState`, `popup::Keyboard`, `bubbles_surface::BubbleSurface`, `osd_surface::OsdSurface`, `link::LinkState`); `roles` and `tray*` stay flat. So `quick_timer` in the dated notes above is now `quick.timer`.
  - **Renames:** `tray/` is now `session_bus/`, `launch.rs` is now `startup.rs` (the dated notes above keep the old names). Backends under `sources/` stay single files.
  - **Wi-Fi and Bluetooth share** `PanelScope`, one `Tap`, `nav`, `moved`, `back_on`, `follow_selection`, tags and rows; a third list still takes about 35 edits (was about 45): their readers and details differ on purpose, so there is no `PanelReader` or `Detail<T>`.
  - **`Mixer…` in the quick panel** (Mae chose a link to wiremix over a built-in mixer): `[quick] mixer`, default launcher terminal + wiremix, `[]` hides it. The default is not checked against `$PATH` (the panel has no such lookup).
- **UI refresh** (spec
  [`docs/specs/2026-11-15-alpheus-ui-refresh-design.md`](docs/specs/2026-11-15-alpheus-ui-refresh-design.md),
  plan [`docs/plans/2026-11-15-alpheus-ui-refresh-plan-1.md`](docs/plans/2026-11-15-alpheus-ui-refresh-plan-1.md))
  **implemented and merged into `main` on 2026-11-15** (8 tasks, per-task reviews, a final whole-branch review and its fix round; Mae's trial pending). The spike Mae approved on 2026-11-15, rebuilt with tests:
  - **Presets.** Six more (`ember`, `glacier`, `ink`, `hicon`, `sorbet`, `phosphor` (dark only)): 15 in all, the extra contrast ratios asserted for the new six.
  - **Control center.** The TUI keeps the theme's full palette and a `Shape` from `style` (`islands` round caps, `powerline` arrows, `underline` no fills, `floating` double frames and shadows; `flat` and `translucent` square; the caps need a Nerd Font). A left **rail** from 84×26 (Mae's Super+A window is 87×26: the plan's amendment replaced the spike's 90×30), one hue per tab, bands, knob gauges, switches and glyph buttons; **Home as a dashboard** from a 64×20 body (block-digit clock, setting cards, four tiles, connections and devices); the keycap row keeps the focused item's keys first (the classic 80×24 row's order changed with it, deliberately). Below the thresholds the classic chrome is drawn as before.
  - **Bar.** A dim `·` between modules, volume as icon + percentage, resources as icon + figure; the stair gauge, the sparkline and the CPU history are gone, `[resources] graph` is read and ignored. Full bar 168 → 142 columns.
  - **Quick panel.** Switches on the toggle rows, labelled session buttons whose hit regions come from the one layout function; the muted microphone reads as the grey off switch (the safe state, to show Mae).
  - **Measured in release** (vs `d9b8965`, back to back, 3 runs each, separate target directories): `everything` of `startup_pss_per_source` +4…+32 KB in two pairings and −160 in the third, every other source within −156…+32 except one `+ resources` pairing at +272 (that base run read 8,271 KB against 8,607–8,611 in its two others: harness spread); the daemon binary the same size (6,812,592 bytes, different bytes), the TUI 1.48 → 1.54 MiB (+60 KiB stripped); the quick panel open 316–338 KB over rest (base 256–274), +208…+210 KB over rest once closed (base +126…+148, the first open's warm-up), 20 cycles −28…−4 KB, 0 loop and 0 whole-daemon wake-ups at rest after; the dashboard's first render 0.4–0.8 ms against the 100 ms budget; the whole memory suite 49 passed, the headless suite 178 passed.
  - **Pending.** Mae's trial; the screenshots in `docs/images` (control-center-home/sound/shapes, the bar and quick refreshed) come from a scratch harness that is not in the tree. Her theme choice stays hers (`theme.toml`).
- **Small features 1** (spec
  [`docs/specs/2026-11-15-alpheus-small-features-design.md`](docs/specs/2026-11-15-alpheus-small-features-design.md),
  plan [`docs/plans/2026-11-15-alpheus-small-features-plan-1.md`](docs/plans/2026-11-15-alpheus-small-features-plan-1.md))
  **implemented and merged into `main` on 2026-11-15** (per-task reviews and fix rounds; Mae's trial pending, French note in `.superpowers/sdd/features/note-essai.md`, with the plan's decisions 1-11 to confirm and the Super+C line for Mango and Sway):
  - **Launcher calculator.** A row in All for a query that looks like arithmetic, alone in the `--calc` mode (`alpheus-launch --calc`: like `--toggle`, it closes an open launcher of any mode first, 250 ms debounce); integers exact (i128, then f64, 12 digits), `-7 % 3 = -1`, units `<expr> <unit> to|in <unit>` (inches are typed `inch`: `12 in to cm` gives no row); Enter copies the answer, Ctrl+Enter `query = answer`, through `copy = ["wl-copy"]` in `launcher.toml`.
  - **Timed do-not-disturb.** `alpheus-launch --dnd on --for 30m|--until 22:30`, the end kept in `$XDG_STATE_HOME/alpheus/dnd` (`until <secs>`, `sharing`), one `DeadlineTimer` armed only while it runs, the clock's minute tick re-checking the wall clock; `[notifications] dnd_while_sharing` (needs `privacy` and `notifications`) follows capture edges, the user's choice winning.
  - **Rules and battery alerts.** `[[notifications.rules]]` (mute, drop, urgency; at most 32, first match wins) in the server; `[battery] warn = [20, 10, 5]` alerts through the server from UPower's state, only while serving and re-offered when the server starts.
  - **Measured in release** (vs `697fea2`, back to back, 3 runs each, separate target directories): one-time cost +24…+88 KB (`everything` of `startup_pss_per_source`; binary 6.50 → 6.59 MiB, +98 KiB stripped); 32 rules, four thresholds and `dnd_while_sharing` +4…+116 KB at start (bound 128 KB); the calculator open with `6*7` typed +778–790 KB over closed, closed again 0…+16 KB (the first open's +390…+422 KB code and font warm-up is the same for a plain toggle, so `launcher_calc_pss_open_and_closed` reads closed after one warm-up cycle: the plan's bound `closed again <= closed + 256` could not hold on a cold first open); 0 loop wake-ups at rest with a timed do-not-disturb (10 s) and after a calculator cycle (5 s), 3 runs; a 1 s timer in `dnd_wait` makes the first read 10 wake-ups.
  - **Pending.** Mae's Super+C bind and any of the new keys in `bar.toml` and `launcher.toml` (never edited from here); after the switch, `shell-ctl reload-bar`. Parked minors: an error row first in All blocks Enter; `--until` from inside a DST-skipped hour; `dnd_while_sharing` removed by hot reload mid-capture leaves do-not-disturb on; a rule on a muted flood is bounded only by the history trim.
- **Small features 2** (spec
  [`docs/specs/2026-11-15-alpheus-small-features-2-design.md`](docs/specs/2026-11-15-alpheus-small-features-2-design.md),
  plan [`docs/plans/2026-11-15-alpheus-small-features-2-plan-1.md`](docs/plans/2026-11-15-alpheus-small-features-2-plan-1.md))
  **implemented on `feat/small-features-2`, not merged; Mae's trial pending**, French note in `.superpowers/sdd/features/note-essai-2.md` (the plan's "Decisions written down" to confirm):
  - **Resources.** `[resources] show` (default `cpu`, `memory`, `temperature`, so the default bar is unchanged) adds `net` (the IPv4 default route's interface, receive and send rates), `disk` ($HOME's filesystem, percent used rounded up like `df`, `statvfs` on the event loop each pass: a hung network `$HOME` would block the bar) and `gpu` (the GPU's hwmon temperature; `gpu_sensor` picks the driver, and reading a discrete GPU can wake it on a hybrid laptop: name the iGPU's driver). Same loop timer, no thread, nothing read for an item left out.
  - **Quick panel.** The do-not-disturb row opens a duration detail (Enter or →; Space still toggles): Off, 30 minutes, 1 hour, 2 hours, Until `[quick] dnd_until`, Until I turn it off; a timed one reads `until HH:MM` on the row.
  - **Measured in release** (vs `1595029`, back to back, 3 runs each, separate target directories): one-time cost −12…+112 KB (`everything` of `startup_pss_per_source`; binary 6.59 → 6.63 MiB, +40 KiB stripped); every item +140…+248 KB over the default three (binary code +64, font pages +64 for the new glyphs, heap +16…+20; the test now bounds it at 384 and ~60 more passes at +64 KB of the daemon's own memory), the quick module's 0–100 KB "module cost" being an anonymous ~110 KB step that flips on both trees (smaps, final review), not a cost, the same tick (14–17 loop wake-ups in 5 s at interval = 1), 0 wake-ups at rest with a timed do-not-disturb.
  - **Pending.** Mae's `show` and `dnd_until` in `bar.toml` (never edited from here); after the switch, `shell-ctl reload-bar`.
- **Refocus (Mae, 2026-10-11): back to the core.** No more replacing system pieces (lock screen, portal FileChooser/Settings, dropping the gtk portal, screensaver): polish what Mae uses daily — the bar, the launcher, notifications, the TUI control center, centralized settings, finishing touches. The lock screen stays paused (`feat/lock` complete and reviewed, unmerged; ≈ 37–68 MB on her 3 outputs with full-size CPU buffers). The file chooser was dropped during its brainstorm (prior notes in `.superpowers/sdd/portal/`).
  - **Bar gaps** (Mae, 2026-10-13): the **⏻ power button** and the **privacy indicators, delivery 1** (above) done; privacy delivery 2 (a camera opened outside PipeWire, above) merged; quick settings delivery 3 (Bluetooth, above) merged; **next**, Mae's choice. The two items noted on the way were nixos-config's, both fixed there: the headset's media buttons (BlueZ's AVRCP keys had no bind; now `playerctl` under every compositor, 8a7e8b9) and keep-awake lost after `nh os switch` (sd-switch restarted hypridle; a `ConditionPathExists` on its flags, d634ac6). The keyboard layout is set aside (Mae, 2026-10-16: "oublie pour l'instant"; each of her keyboards has one layout, so "next layout" does nothing for her, and no compositor reports which keyboard is typing): prior art kept in `.superpowers/sdd/keyboard/`.
- **Dropped 2026-10-11 (refocus): an alpheus file chooser (Mae, 2026-10-09).** `xdg-desktop-portal-gtk` measured 33 MB PSS on Mae's session (≈ 27 MB heap kept after one GTK3 file dialog; investigation in `.superpowers/sdd/portal/`). termfilechooser + yazi was tried and rejected (it creates the file before saving). Mae chose an alpheus-native `FileChooser` portal backend in the launcher's style; then alpheus serving `Settings` (colour-scheme, accent), and dropping the gtk backend.
- **Set aside 2026-10-11 (refocus), was: goal "replace as many costly pieces as possible" (Mae,
  2026-10-05)**: a **lock screen** replacing hyprlock (measure hyprlock's
  memory first; `ext-session-lock-v1`; its own process, so a crash never
  unlocks the session; prior-art research first) and, as a bonus, an
  old-school **screensaver** (visuals only). Guardrail unchanged: alpheus
  never handles sleep or `dpms` — locking and blanking stay triggered by
  hypridle/swayidle, and the DisplayLink power-off risk stays out of scope.
  How Mae's setup does it today (nixos-config `home/hypr/hypridle.conf`):
  hypridle (ext-idle-notify) dims at 3 min, `loginctl lock-session` at
  5 min (→ hyprlock), and at 10 min runs `idle-dpms`, which **skips**
  `dpms off` whenever `/sys/devices/evdi/count` > 0 (DisplayLink docked)
  and otherwise calls `wm-set dpms off`; resume is `wm-set dpms on`. So the
  safe path is: hypridle stays the trigger, alpheus plugs in as commands
  (`lock_cmd` → the alpheus lock, a listener → the screensaver). Replacing
  hypridle itself would mean reusing that evdi check — Mae's decision.
- **Style and themes, for all of alpheus (future, Mae 2026-10-05)**: the flat
  dark look gets tiresome over time — study a richer style (borders, accents,
  depth, transparency, light variants…) and **preset colour themes** for
  users who don't follow their wallpaper (matugen stays the default for Mae).
  Lightness rules apply: a theme costs nothing when unused. Delivery 1
  (colours) and delivery 2 (bar shapes) done, see above.
- **README tables are the yardstick (Mae, 2026-10-05)**: the README keeps a
  memory comparison (alpheus vs other shells, then alpheus per component)
  and a feature comparison (alpheus vs DMS, Noctalia, ashell, waybar stack)
  that guides the roadmap. Update both with every delivery (real release
  measurements; ✅/🚧/💭/❌ statuses).
- **Prior art first**: before designing each part, study how other bars and
  shells do it (i3status-rust, ironbar, ashell, cosmic-applets, waybar,
  Noctalia/DMS…), one research agent per domain; reuse ideas and signal
  lists, not heavy dependencies.
- **English rename** (2026-10-04, Mae's decision): done — identifiers,
  comments, tests, log and error messages, user-visible strings (TUI and
  bar), config file names and keys (`bar.toml`, `displays.toml`), CLI
  (`--tab`), docs and commit messages. Only conversations with Mae stay in
  French. Strings that are a contract with nixos-config scripts (e.g. the
  keep-awake entry of `tools-menu`) stay as they are.
- Code formatted with `cargo fmt` (`rustfmt.toml`), enforced by
  `nix flake check`.
- **Portability** (after the bar, agreed with Mae on 2026-10-04): ship or
  make configurable the dependencies on nixos-config scripts (`wm-set`,
  `shell-ctl`, `tools-menu`, `alpheus-toggle`) with a neutral fallback;
  Dvorak-based keys disabled by default (enabled for Mae through her
  `config.toml`).
- Sub-project 3 (full NixOS integration): to be designed later.

## Rules

- **Language**: repository content (code, comments, docs, commit messages,
  UI) is in English; talk to Mae in French.
- **TDD**: test written first, red, then green; every test must prove it
  bites (temporarily break the code → red → restore).
- **Never a real call in tests** to tools that act on the session
  (`wpctl`, `nmcli`, `bluetoothctl`, `systemctl`, `wm-set`, `shell-ctl`…):
  injectable command executor.
- **Guardrails** (details in the scope doc): no sleep or `dpms` handling;
  no writes to the compositor outside `wm-set`, except
  `zwlr_output_configuration_v1` in the daemon's `outputs/wlr.rs`; nothing resident.
- **Budget**: total resident memory ≤ 120 MB (sub-project 2); TUI < 100 ms
  to first render.
- **Shareable product, lightness first** (Mae, 2026-10-04): alpheus will be
  public. Plan for customization, but never at the expense of performance:
  an unused option costs nothing, no heavy dependency "for config". Don't
  hardcode what's specific to Mae's workstation (nixos-config scripts,
  Dvorak, paths) without a neutral default or a configurable path.
- The workstation runs NixOS, configured in `~/nixos-config` (flake):
  nixos-config takes alpheus as `git+ssh://git@github.com/mornepousse/alpheus`;
  a local branch is tried with `--override-input alpheus
  git+file:///home/mae/Documents/GitHub/alpheus` — whatever isn't committed
  here isn't seen by that build.
