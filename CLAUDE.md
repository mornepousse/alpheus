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
  and plan executed, merged into `main` on 2026-10-03. Real-world trial by
  Mae still pending. Keyboard layouts tab: deferred.
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
  - **Pending, Mae's decision (nixos-config, never edited from here):**
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
  - **Pending (Mae's decision, nixos-config, never edited from here):**
    - *shell-ctl.* The alpheus pile: volume and mic keys on `wpctl`, brightness keys on `brightnessctl` then `alpheus-launch --osd brightness`; `alpheus:start` becomes `( restore-wallpaper & ); exit 0` (no `swayosd-server`). Until then both swayosd and the alpheus OSD pop, since `osd` is in the default `modules` and Mae sets none.
    - *Tests and docs there.* `COMPORTEMENTS.md` AL1/AL2 and `tests/shell-ctl-test.sh`'s AL1/AL2 cases; stale comments in `shell-ctl.sh` (header, `pile=classic`), `modules/desktop/desktop-shell.nix` (header, `description`), `hosts/p16/default.nix` (the pile comment), `keybinds.conf` and `keybinds.lua` ("route vers swayosd").
    - *Switching.* The running `swayosd-server` needs a `pkill`, or a re-login.
    - *Memory figures.* The README's 49.5 MB for swayosd is replaced by 7.6 MB PSS; the hybrid-mode row is recomputed (≈ 23 MB now, ≈ 15 MB after the switch); the waybar stack's ≈ 120 MB (DMS trial spec, 2026-09-30: swayosd 48, swaybg 31 MB as PSS) does not match later PSS measurements, restated as ≈ 55 MB from sourced figures until measured again side by side.
- Next, in order: the wallpaper (swaybg) is optional; then the lock screen and the file chooser (below).
- **Later: an alpheus file chooser (Mae, 2026-10-09).** `xdg-desktop-portal-gtk` measured 33 MB PSS on Mae's session (≈ 27 MB heap kept after one GTK3 file dialog; investigation in `.superpowers/sdd/portal/`). termfilechooser + yazi was tried and rejected (it creates the file before saving). Mae chose an alpheus-native `FileChooser` portal backend in the launcher's style; then alpheus serving `Settings` (colour-scheme, accent), and dropping the gtk backend.
- **Later, goal "replace as many costly pieces as possible" (Mae,
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
  no writes to the compositor outside `wm-set`; nothing resident.
- **Budget**: total resident memory ≤ 120 MB (sub-project 2); TUI < 100 ms
  to first render.
- **Shareable product, lightness first** (Mae, 2026-10-04): alpheus will be
  public. Plan for customization, but never at the expense of performance:
  an unused option costs nothing, no heavy dependency "for config". Don't
  hardcode what's specific to Mae's workstation (nixos-config scripts,
  Dvorak, paths) without a neutral default or a configurable path.
- The workstation runs NixOS, configured in `~/nixos-config` (flake):
  alpheus will be wired in there as a `git+file://` input — whatever isn't
  committed here isn't seen by the build.
