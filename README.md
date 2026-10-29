# alpheus

> *Alpheus*: the pistol shrimp, tiny and the fastest in the ocean.

A **lightweight, terminal-spirited** Wayland desktop shell, written in
Rust: what DankMaterialShell or Noctalia bring (bar, centralized settings,
notifications, OSD), **without their weight**. Aimed at modest hardware
with modern features.

## Memory

How much RAM a desktop shell keeps resident, measured as PSS
(`/proc/<pid>/smaps_rollup`) on the author's laptop (Hyprland, three
outputs). alpheus figures are release builds. Updated with every delivery.

### Compared with other shells

| Shell | Toolkit | Resident memory | What it covers |
|---|---|---:|---|
| **alpheus** (today, hybrid mode) | Rust, no toolkit | **≈ 15 MB** (to be re-measured) | bar ≈ 14 MB (live) + swaybg 1.4 MB (optional) borrowed (rows below); swayosd (7.6 MB) is no longer used since the OSD replaced it, and mako (6.7 MB) since the notifications did; the live bar figure predates several deliveries and is to be re-measured |
| **alpheus** (target, everything replaced) | Rust, no toolkit | **≤ 120 MB budget** | bar, launcher, notifications, OSD, wallpaper |
| waybar + mako + swayosd + swaybg | GTK / C | ≈ 55 MB (see the note below) | bar, notifications, OSD, wallpaper |
| HyprPanel | GJS | ≈ 151 MB | bar, notifications, OSD (Hyprland only) |
| Wayle | Rust, GTK4 | ≈ 168 MB | bar, panels |
| ashell + swaync | Rust, iced | ≈ 194 MB | bar, notifications |
| Noctalia 4.7 | Quickshell (QML) | ≈ 517 MB | full shell (v5, a native C++ rewrite released in September 2026, not measured yet) |
| DankMaterialShell (trimmed) | Quickshell (QML) | ≈ 512 MB | full shell, sound/visualizer/weather/temperatures off |
| DankMaterialShell | Quickshell (QML) | ≈ 537 MB | full shell |
| Caelestia | Quickshell (QML) | ≈ 730 MB | full shell |

### alpheus, component by component

| Component | Memory | Notes |
|---|---:|---|
| Bar daemon, all modules + launcher, 3 outputs — at start | ≈ 8.0 MB | headless sway test, `tests/memory.rs`, recorded D-Bus services on a private bus; libdbus shared with the session; tray unused: `startup_pss_per_source` ("everything", 3 runs: 8,144–8,246 KB); ≈ 8.4–8.5 MB with the tray and 10 fake items (`startup_pss_with_the_tray_under_15_mb`, 3 runs: 8,614–8,701 KB, without the launcher module) |
| Bar daemon — after 8,300 events and 5 plug cycles | ≈ 9.6–9.7 MB | stable, 0 wake-ups at rest, the theme watch adds no wake-up, 6 threads (D-Bus through dbus-rs/libdbus: one thread; the launcher and the tray add none) |
| Tray, module present, no item | ≈ 0.4 MB | watcher + session connection only, no thread (376–392 KB) |
| Tray, 10 fake items (5 KDE-, 2 Qt-, 2 Chromium-style, 1 Steam-like) | ≈ 0.5 MB | 488–496 KB, ≤ 2 MB budget, `ten_fake_items_cost_at_most_2_mb`; menu and calendar popups freed on close (20 cycles: +32 KB in each of 3 runs) |
| Tray, linked in but unused (no `tray` in `modules`, no session bus) | +0.4–0.7 MB vs. the delivery before this one | one-time cost of a bigger binary (stripped 4.49 → 5.36 MB, +864 KB: libdbus linkage, menus, popups, calendar); measured back to back in release, a real cost, like the launcher's |
| Themes, linked in (9 presets, resolver, inotify watch, portal following) | +144–224 KB vs. the delivery before this one | one-time cost; binary 5.11 → 5.18 MiB (+76 KiB stripped); measured `c85c0df` vs this delivery, release, back to back, 2 runs each; an unused preset is rodata only |
| Themes, 50 reloads (3 outputs repainted each time) | +0–444 KB | `theme_reloads_no_growth`: 0 KB in four of five runs, 444 KB in the first run after a build; 0 loop wake-ups at rest after a reload |
| Bar shapes, linked in (6 styles, corner masks, placement) | within noise (−48…+156 KB) vs. the delivery before this one | one-time cost; binary 5.20 → 5.24 MiB (+44 KiB stripped); measured `6a6df10` vs this delivery, release, back to back, 3 runs each, separate target directories; `flat` pays only the style match |
| Bar shapes, per style at start (all modules, 3 outputs) | flat ≈ 8.0 MB; others −76…+116 KB over flat | `startup_pss_per_style`, 3 runs (flat 8,142–8,224 KB: the same bar as the start row above); corner masks on first use −32…+88 KB (islands − flat), +36…+116 KB (floating − flat, its taller buffer included) (computed: 128 B for islands, 200 B for floating, at scale 1; one per radius and scale used) |
| Bar shapes, 48 style switches (3 outputs; floating reconfigures every bar) | +20 KB | `style_switches_no_growth`, +20 KB in each of 3 runs; 0 loop wake-ups at rest under each of the six styles (`at_rest_no_wakeups_under_each_style`) |
| Media, linked in but unused (no `media` in `modules`) | within noise (−52…+32 KB) vs. the delivery before this one | binary 5.24 → 5.33 MiB (+92 KiB stripped); measured `55ba754` vs this delivery, release, back to back, 3 runs each, separate target directories |
| Media module, no player (on the bar's own session connection, tray off) | 0–4 KB in mode `auto`; 286–395 KB in a fixed mode | `media_module_pss_without_and_with_players`, 3 runs (7.2–7.3 MB with it, 1 output): in mode `auto` that connection is the light/dark portal's, already open; in a fixed mode (`dark`) the module opens it, and the figure includes the connection itself; with the tray on it shares the tray's connection |
| Media, 3 fake players | 85–112 KB | same test, 3 runs; at most 16 players, strings capped at 256 characters, 8 artists |
| Media, 100 track changes | +0 KB | `track_changes_no_growth`, +0 KB in each of 3 runs (the titles reuse glyphs already drawn: the glyph cache's own bound is measured by the 8,300-events row); 0 loop wake-ups at rest with a player playing and the popup closed, tray on and off (`at_rest_no_wakeups_with_a_playing_player`); the popup's 1 s tick runs only while it is open and the track plays |
| Notifications, linked in but unused (no `notifications` in `modules`) | +122–248 KB vs. the delivery before this one | binary 5.34 → 5.47 MiB (+140 KiB stripped); measured `900a519` vs this delivery, release, back to back, 3 runs each, separate target directories |
| Notifications server and bell, serving with nothing (own session connection, tray off) | 0–4 KB | `notifications_pss_idle_history_and_bubbles`, 3 runs (10.9 MB with it, 1 output): in mode `auto` that connection is the light/dark portal's, already open, as for the media row; with the tray on, it shares the tray's connection |
| Notifications, 50-entry history | 192–198 KB | same test; capped: app 64 B, summary 256 B, body 1 KiB, 6 actions; history in memory only |
| Notifications, 4 bubbles shown | 326–774 KB, −84…+76 KB once closed | same test; their surface and pool exist only while a bubble shows; 0 loop wake-ups at rest with a sticky bubble shown and once every bubble went (`at_rest_no_wakeups_with_notifications`) |
| OSD, linked in but unused (no `osd` in `modules`) | within noise (−28…+172 KB) vs. the delivery before this one | binary 5.48 → 5.54 MiB (+56 KiB stripped); measured `1662cb4` vs this delivery, release, back to back, 3 runs each, separate target directories |
| OSD, module on, nothing shown | 0–96 KB | `osd_pss_idle_shown_and_gone`, 3 runs (10.9–11.0 MB with it, 1 output): the last sound state and three instants; no surface, no timer, no sysfs read until `alpheus-launch --osd brightness` |
| OSD, one pill shown | 4–30 KB, −38…−12 KB once gone | same test; its surface and pool exist only while it shows (1.3 s after the last change); a held key keeps one surface; 0 loop wake-ups at rest once it went (`at_rest_no_wakeups_after_the_osd`) |
| Quick settings, linked in but unused (no `quick` in `modules`) | +64…+196 KB vs. the delivery before this one | binary 5.59 → 5.70 MiB (+108 KiB stripped); measured `d483590` vs this delivery, release, back to back, 3 runs each, separate target directories, on the `everything` case of `startup_pss_per_source` (its `modules` list has no `quick`): the code linked in, unused |
| Quick settings, module on, panel closed | 96–108 KB | `quick_pss_idle_open_and_closed`, 3 runs (8.0 MB with it, 1 output): the power profile's state, read once from the system bus (no daemon: one GetNameOwner), and the sound tap; no surface, no timer, no sink list. With `quick` alone in `modules`, the D-Bus source thread may start for that profile (the default modules already run it) |
| Quick settings panel, open | 444–448 KB, +340–344 KB over idle once closed | same test; the closed figure is the first open's one-time warm-up, not a leak: `/proc/<pid>/smaps` shows the binary's code pages (≈ +320 KB) and the font file's (≈ +64 KB) now resident, the heap and the shm pool back to idle's; 20 more open/close cycles −18…+36 KB over closed; its surface and pool exist only while it shows; 0 loop wake-ups at rest once it closed (`at_rest_no_wakeups_after_the_quick_panel`) |
| Quick settings' Wi-Fi, linked in (delivery 2) | +176…+372 KB vs. delivery 1 | binary 5.70 → 5.83 MiB (+136 KiB stripped); measured `dbb90b8` vs this delivery, release, back to back, 3 runs each, separate target directories, on the `everything` case of `startup_pss_per_source` (no `quick` there), nine pairings: code linked in, unused. smaps at startup (one run each): the binary's resident code pages +244…+308 KB and its data +24 KB; heap, fonts and libraries unchanged within noise (the shm pools and anonymous memory move ±110 KB between runs) |
| Quick settings, Wi-Fi detail open (40 access points, 32 listed) | 898–910 KB over the panel's module at rest, +476…+488 KB once closed | `quick_wifi_pss_detail_open_and_closed`, 3 runs (7.9–8.0 MB at rest, 1 output): the D-Bus thread reads the access points and saved profiles only while the detail is open (five match rules, removed with it; a socket pair wakes it, no thread, no timer). Once closed, smaps (3 runs): the binary's code pages +128 KB and the font file's +320 KB touched by the first open, the main stack +20 KB, the main heap +68…+76 KB (most likely the painter's glyph caches, bounded), the shm pools −56 KB; the D-Bus thread's arena back to idle's. 10 more cycles +36…+72 KB over closed without the shm pools (the main heap creeps +4…+16 KB per 10 cycles after the first 10, within the leak bound); 0 loop and 0 D-Bus-thread wake-ups at rest after (`at_rest_no_wakeups_after_the_wifi_detail`) |
| Quick settings' Bluetooth, linked in (delivery 3) | +12…+208 KB vs. delivery 2 | binary 6.07 → 6.24 MiB (+168 KiB stripped); measured `9401cec` vs this delivery, release, back to back, 3 runs each, separate target directories, on the `everything` case of `startup_pss_per_source` (no `quick` there), nine pairings: code linked in, unused |
| Quick settings, Bluetooth detail open (20 devices) | +1.1–1.8 MB over the panel's module at rest, +412…+516 KB once closed | `bluetooth_detail_pss_with_20_devices_and_after`, 8 runs (8.3–8.4 MB at rest, 1 output): the D-Bus thread reads the devices only while the detail is open (no rule added). Open, the daemon's own memory +436…+540 KB and the shm pools +774…+1,240 KB (4 runs), the moving part (most likely the detail growing one row per device found while it is open, its buffers re-made at each height), all gone with the popup; the closed figure is the daemon's own (the first open's code, font and glyph-cache pages, as for the Wi-Fi detail), the pools back to idle's. 10 more cycles +40…+64 KB; 0 loop, 0 D-Bus-thread and 0 whole-daemon wake-ups at rest after (`bluetooth_at_rest_no_wakeups_after_the_detail`, 5 runs) |
| Quick settings, Bluetooth acting connection | +54…+116 KB | `bluetooth_acting_connection_cost`, 5 runs (the detail with it against the rows without it): a second system-bus connection with the pairing agent, open only while the detail is open or a call is in flight; no thread (6 either way); 0 loop wake-ups in 5 s while it idles |
| Quick settings, Bluetooth headset row | +12…+20 KB | `bluetooth_headset_row_cost`, 5 runs (the rows on a Bluetooth headset against a wired sink): the bluez5 device and its profiles kept from `pw-dump` (≤ 8 devices × 32 profiles), one row more |
| Bar daemon, live on the author's laptop (3 outputs) | ≈ 11.3 MB | real session, all modules — predates this merge, see note below |
| `pw-dump` child (sound source) | ≈ 2.5 MB | silent at rest |
| **alpheus resident total, live** | **≈ 14 MB** | bar + sound source — predates this merge, see note below |
| Launcher feature, linked in but never opened (no `launcher` in `modules` even) | +0.3–0.7 MB vs. the delivery before this one | one-time cost of a bigger binary (`nucleo-matcher`, `xkeysym`, `xkbcommon`'s Rust bindings) plus `libxkbcommon.so` itself (≈ 60 KB of it, `/proc/<pid>/smaps`); measured `c256d33` vs the launcher delivery, release, back to back — a real cost, not "noise" |
| Launcher, closed (socket only, nothing else resident) on top of that | +0 MB | no thread, timer or keyboard exists while closed |
| Launcher, open (typed query, keyboard bound) | ≈ +2.0–2.1 MB over closed | headless sway, 300 synthetic apps, `tests/memory.rs`, with dbus-rs |
| Launcher, open latency | ≈ 0.8–1.3 ms median (max 1.7–2.9 ms) | request accepted → first frame committed, 20 opens, with dbus-rs |
| Launcher, 50 open/close cycles | ≈ +0.1–0.2 MB vs. the post-warm-up baseline | 17 of them closed by a typed Escape, the rest by toggle, with dbus-rs |
| Launcher, 100,000-line dmenu | ≈ +6.6–6.8 MB open, back to +0.5 MB 1 s after cancel | worst-case dmenu size; memory released promptly on cancel; with dbus-rs |
| Launcher command sources, linked in but none configured | within noise of the delivery before this one (−4…+92 KB closed; +0.1–0.2 MB open) | one-time cost of a bigger binary (stripped 5.54 → 5.59 MiB); measured back to back in release, 3 runs each, separate target directories |
| Launcher, open with a 300-line command source | ≈ +2.4–2.5 MB over closed (≈ +1.9–2.4 MB with no source) | headless sway, 300 synthetic apps, `tests/memory.rs` `launcher_sources_pss_latency_and_rest`, 3 runs |
| Launcher, a 300-line source's entries | ≈ 15–17 ms after the request (median of 21 opens) | the box shows first: open latency ≈ 1.7–2.2 ms median against 1.6–1.9 ms with no source; 0 wake-ups at rest after; the harness drifts between runs: trust the deltas |
| Control center (TUI) while open | ≈ 2 MB + its terminal | 0 at rest (not resident); the terminal dominates: alacritty ≈ 93 MB |
| Sway and Mango backends, linked in | +132…+140 KB vs. the delivery before this one | binary 5.83 → 5.97 MiB (+140 KiB stripped); measured `2b137f0` vs this delivery, release, back to back, 3 runs each, separate target directories, on the `everything` case of `startup_pss_per_source` (the fake Hyprland on both sides): the code linked in, unused |
| Bar daemon under Sway and under Mango, all modules, 3 outputs — at start | Sway 8.9–9.0 MB, Mango 8.9 MB (the fake Hyprland 8.9–9.0 MB in the same runs; Sway −16…+68 KB, Mango −136…−44 KB against it) | `startup_pss_per_backend`, 3 runs: Sway is the headless sway itself, read through its i3 IPC (two connections on the existing compositor thread), Mango a fake socket serving the real 0.18 captures (two watch connections, one `poll`); 6 threads under each; 0 compositor-thread wake-ups at rest under both (`at_rest_no_wakeups_under_sway_and_mango`; 0 whole-daemon wake-ups in 5 runs of 6; 2 once under Mango, in run 1, the full memory suite, cause not investigated, the compositor thread at 0 throughout) |
| Power button, linked in but unused (no `power` in `modules`) | −8…+84 KB over six pairings; one run read +288…+336 KB on all three of its pairings (its resources step moved too; a boot shift believed, not proven) vs. the delivery before this one | binary 5.97 → 5.97 MiB (+8 KiB stripped); measured `242c686` vs this delivery, release, back to back, 3 runs each, separate target directories, on the `everything` case of `startup_pss_per_source` (no `power` there): the code linked in, unused |
| Power button, menu closed | 184–192 KB | `power_menu_pss_idle_open_and_closed`, 3 runs (7.4 MB with it, 1 output): a one-cell segment; no surface, no timer; of those, ≈ +128 KB are new pages of the same Nerd Font file already mapped (⏻ U+23FB is in it, no other face is loaded; the kernel's fault-around adds to it) and ≈ +64 KB the module's code pages: file-backed, clean, paid once |
| Power menu, open | 432 KB, +280 KB over idle once closed | same test; the closed figure is the first open's code and font pages; 20 more open/close cycles +12 KB over closed; its surface and pool exist only while it shows, its timer only while reboot or power off asks; 0 loop wake-ups at rest once it closed, a pending question included (`at_rest_no_wakeups_after_the_power_menu`) |
| Privacy indicators, linked in but off (no `privacy` in `modules`) | −80…+116 KB at start vs. the delivery before this one (the `everything` case, nine pairings; the `plan 1` case −160…+60 KB) | binary 5.97 → 6.05 MiB (+76 KiB stripped); measured `457cf5d` vs this delivery, release, back to back, separate target directories, 3 outputs, 3 runs each of `startup_pss_per_source` and of two variants dumping `/proc/<pid>/smaps` and `status` at the same 3 s point (8.78–8.87 MB before, 8.79–8.89 MB after). The smaps split (`everything`): the binary's code pages +68 KB and its read-only data −56 KB (RssFile +12 KB), heap −12…+36 KB, shm pools unchanged (660 KB in both trees), other anonymous memory one 112 KB step either way in both trees (startup timing): the code linked in, unused, and nothing allocated. A first measurement read +344…+440 KB; its base runs read ≈ 0.4 MB low against the same commit and binary, a harness shift not reproduced since |
| Privacy indicators, module on at start | −88…+120 KB over the same bar without it (8.79–8.90 MB) | same test, 6 runs; the fake `pw-dump` serves a full-size initial graph (the sound fixture's dump), so this is the "large graph" figure: the capture model keeps only the nodes and links that matter (≤ 1,024 nodes, 4,096 links) |
| Privacy indicators, a capture shown | −60…+26 KB, −50…−28 KB once it ended | `privacy_pss_with_a_capture_and_after`, 3 runs (8.0–8.1 MB idle, 1 output); no surface unless the popup opens; 20 more capture cycles +16…+30 KB; 0 loop wake-ups at rest after a capture and 0 for the whole daemon (`at_rest_no_wakeups_after_a_capture`): the 0.5 s off-delay timer is armed only while an indicator is going off |
| Privacy indicators, a camera opened directly | one-time cost −236…+100 KB (`everything`; one base run read 9.35 MB, a boot shift); module on at start +84…+192 KB (`+ privacy`) vs. `5d8fd55`, 3 runs each, retaken once the harness points every test daemon's `[privacy]` at empty directories (about 9.0–9.2 MB, 3 outputs); a holder shown −38…+70 KB over idle, 0…+4 KB once gone, +20 cycles +32…+64 KB (8.5–8.6 MB, 1 output) | binary 6.05 → 6.07 MiB (+24 KiB stripped); `privacy_camera_pss_and_rest`, 3 runs; 0 loop and 0 whole-daemon wake-ups in 5 s after the holder went (the 300 ms scan timer and the 0.5 s off-delay are armed only after a trigger); the scan stays on the event loop: median 4.0–4.1 ms on 120 processes × 30 descriptors (the daemon scans only its own user: 93 processes and 2,726 descriptors measured on the workstation), 14–15 ms on a synthetic 500 × 20 tree, the worst case seen |
| mako, replaced by the notifications above | 6.7 MB | measured on the author's session (2026-10-08, 19.5 MB RSS); alpheus takes `org.freedesktop.Notifications` as soon as mako lets it go, ≈ 6.7 MB (the module serving costs 0–4 KB here; a live figure against a headless one) won back |
| swayosd, replaced by the OSD above | 7.6 MB | PSS on the author's session (2026-10-09): GTK4, libpulse, zbus, async-std; ≈ 7.6 MB won back once `swayosd-server` is no longer started (the module at rest costs 0–96 KB here; a live figure against a headless one) |
| Borrowed for now: swaybg | 1.4 MB | optional |

> **Memory note:** absolute PSS shifts with machine state, by about ±0.5 MB
> between boots, because shared libraries are counted proportionally; the
> per-component deltas are the stable figures. Ranges are over 2–3 release runs
> (KB/1024, one decimal; 2 runs for the themes rows). The launcher rows come
> from the tray-delivery measurement (three runs); the themes rows from two
> alternated runs against the delivery before; the bar shapes rows likewise
> (three alternated runs, HEAD and base built in separate target directories); the media rows likewise; the notifications rows likewise; the OSD rows likewise; the quick settings rows likewise. The privacy rows: 6 runs for the module on at start, 9 pairings (three harness variants) for the linked-in cost.
>
> **Note:** the "live" row above (≈ 11.3 MB) and the resident total it
> feeds into (≈ 14 MB) predate the move from zbus to dbus-rs, the launcher
> merge and the tray: they were measured before any of them. The headless
> sway rows above are measured with all of them; the live rows will be
> re-measured at the real-world trial of the tray (tray plan 2, task 21).
> The hybrid-mode figures in the shells table add that live total to the
> borrowed tools' rows above: swayosd 7.6 MB (2026-10-09), swaybg 1.4 MB
> (2026-10-05), and mako 6.7 MB (2026-10-08) for the figure before the
> notifications delivery. All are PSS on the author's session.
>
> **The waybar stack's row** (≈ 55 MB) adds PSS figures from different
> days: waybar 39 MB (the nixos-config DMS trial spec, 2026-09-30), mako
> 6.7 MB, swayosd 7.6 MB, swaybg 1.4 MB (the rows above). The ≈ 120 MB first
> recorded for this stack on 2026-09-30 (waybar 39, swayosd 48, swaybg 31,
> hypridle 2 MB, also given as PSS) is not borne out by the later swayosd
> and swaybg measurements, and why they differ is not known; waybar's own
> 39 MB has not been measured again. The stack is to be measured again side
> by side.

The control center (`alpheus`, TUI) is not resident: it opens on demand
and exits.

Hyprland, Sway, Mango and niri. Mouse and keyboard everywhere.

Sleep and screen power-off remain the job of dedicated tools (hypridle,
swayidle…): alpheus never touches them. On some outputs (DisplayLink in
particular), a misplaced power-off can freeze the machine.

> **Status**: ongoing personal project, used daily by its author on NixOS.
> It still depends on a few scripts from her configuration (`wm-set`,
> `shell-ctl`…); making them optional, with a neutral fallback, is the next
> step before use elsewhere.

## Features compared

The roadmap's yardstick: what DankMaterialShell and Noctalia offer, within a
fraction of their memory. ✅ done · 🚧 planned (delivery) · 💭 to decide ·
❌ out of scope by design · ➖ partial or through another tool.

| Feature | **alpheus** | DMS | Noctalia | ashell | waybar stack¹ |
|---|---|---|---|---|---|
| **Resident memory** | **≈ 14 MB** today, ≤ 120 MB target | ≈ 537 MB | — | ≈ 194 MB² | ≈ 55 MB³ |
| **Bar** | | | | | |
| Workspaces, active window | ✅ | ✅ | ✅ | ➖ | ✅ |
| Clock | ✅ | ✅ | ✅ | ✅ | ✅ |
| Calendar popup | ✅ month grid, locale first day, ISO weeks | ✅ | ➖ | ✅ | ✅ |
| Volume, network, Bluetooth, battery | ✅ | ✅ | ✅ | ✅ | ✅ |
| CPU, memory, temperature | ✅ | ✅ | ➖ | ✅ | ✅ |
| System tray (icons and menus) | ✅ Nerd Font glyphs, no images | ✅ | ✅ | ✅ | ✅ |
| Media (MPRIS) | ✅ `▶ Title` in the bar, middle click play/pause, wheel next/previous; a popup with time, progress, ⏮ ⏯ ⏭ and the player choice | ✅ | ✅ | ✅ | ✅ |
| Notification counter, do-not-disturb | ✅ a neutral bell with the unread count, crossed under do-not-disturb (middle click, or `alpheus-launch --dnd on\|off\|toggle`) | ✅ | ✅ | ➖ | ➖ |
| Keyboard layout | 💭 | ✅ | ➖ | ✅ | ✅ |
| Privacy indicators (mic, camera, screen share) | ✅ glyphs in the bar for the microphone, a camera (through PipeWire, or opened directly as `/dev/video*`, found by an inotify watch and one `/proc` scan, no polling) or screen share seen through PipeWire, and a screen cast announced by niri or Hyprland; a click lists the apps; wf-recorder under Sway and Mango is still invisible | ✅ | ➖ | ✅ | ✅ |
| Weather | 💭 | ✅ | ✅ | ✅ | ➖ |
| **Panels** | | | | | |
| Control center (quick settings) | ✅ a panel under the volume, network and Bluetooth modules (`alpheus-launch --quick [sound\|wifi\|bluetooth]`): volume and output, brightness, Wi-Fi (networks, saved, open and WPA-personal with a password field), Bluetooth (power, paired and nearby devices, connect, pair with codes, forget), a Bluetooth headset's Hi-Fi/Mic mode, microphone, do-not-disturb, power profile, keep-awake, night light, session buttons; the TUI (≈ 2 MB + terminal, only while open) for the rest | ✅ | ✅ | ➖ | ❌ |
| Power menu | ✅ a ⏻ button at the bar's end: Lock, Log out, Reboot, Power off with the quick panel's commands (reboot and power off ask first; a double click never confirms); the panel's session buttons and the TUI too | ✅ | ✅ | ✅ | ❌ |
| Display settings | ✅ TUI (Hyprland; Mango through `wm-set` once nixos-config writes its monitors.conf; a message under Sway and niri) | ✅ | ➖ | ➖ | ❌ |
| Network, Bluetooth, per-app audio | ✅ Wi-Fi and Bluetooth in the quick panel (Bluetooth: connect, pair with codes, forget, headset mode; `More…` opens nmtui for enterprise, hidden networks, VPN, hotspot); per-app audio in the TUI (wiremix) | ✅ | ➖ | ✅ | ❌ |
| Notification center, history | ✅ a panel under the bell: the player, the last 50 notifications (in memory), do-not-disturb, Clear all | ✅ | ✅ | ➖ | ➖ |
| USB devices (debugging) | 🚧 TUI tab | ❌ | ❌ | ❌ | ❌ |
| **Launcher** | | | | | |
| Apps, run command, window switcher | ✅ | ✅ | ➖ | ❌ | ➖ |
| Script/dmenu mode | ✅ (fuzzel's exit codes) | ➖ | ➖ | ❌ | ✅ |
| Menu scripts' entries in the search (`[[sources]]`: a `list` command per open, `run` with the chosen line) | ✅ the entry, then its path dimmed; nothing resident, nothing per keystroke | ➖ | ✅ | ❌ | ❌ |
| Calculator, units, currencies, dates | 🚧 launcher 2 | ✅ | ➖ | ❌ | ❌ |
| System actions, Bluetooth/sound, SSH, web search | 🚧 launcher 3 | ➖ | ➖ | ❌ | ❌ |
| Emoji, files, clipboard history | 🚧 launcher 4 | ✅ | ➖ | ❌ | ➖ |
| **Other** | | | | | |
| Notification toasts | ✅ bubbles top centre on the focused output; a click runs the app's action, critical ones stay; replaces mako (≈ 6.7 MB) | ✅ | ✅ | ✅ | ✅ |
| OSD (volume, brightness) | ✅ volume and mute, and the microphone's mute (never its level: call apps move it), from PipeWire's own state (any app, any key, a Bluetooth headset), laptop brightness on `alpheus-launch --osd brightness`; bottom centre on the focused output, never takes a click; replaces swayosd (≈ 7.6 MB) | ✅ | ✅ | ✅ | ✅ |
| Colours from the wallpaper (matugen) | ✅ dark and light files, live | ✅ | ✅ | ❌ | ❌ |
| Colour themes, light/dark following the desktop | ✅ 9 presets, 7 hues per theme, per-module colours, auto light/dark, live reload | ✅ | ✅ | ➖ | ➖ |
| Bar styles (islands, powerline, floating…) | ✅ 6 styles (flat, islands, powerline, floating, underline, translucent), pills, chosen in the Theme tab, live reload | ✅ | ✅ | ➖ | ➖ |
| Appearance settings | ✅ the TUI's Theme tab: theme, light/dark mode, module colours, bar style, with a preview | ✅ | ✅ | ❌ | ❌ |
| Wallpaper management | 💭 | ✅ | ✅ | ❌ | ❌ |
| Lock screen | 💭 | ✅ | ✅ | ❌ | ❌ |
| Settings GUI | ➖ TUI + TOML | ✅ | ✅ | ❌ | ❌ |
| Dock, desktop widgets | 💭 | ➖ | ✅ | ❌ | ❌ |
| Screenshots, recording | 💭 | ➖ | ➖ | ❌ | ❌ |
| Idle and power management | ❌ left to hypridle (DisplayLink safety) | ✅ | ✅ | ✅ | ➖ |
| Shows on (layer-shell compositors: Hyprland, niri, sway, river, labwc…) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Workspaces and windows read from | Hyprland, Sway, Mango, niri | 7 compositors | 6 compositors | Hyprland, niri, sway, Mango | many (one module each) |
| Sway: workspaces, title, the launcher's windows | ✅ the i3 IPC on `$SWAYSOCK`, read only (GET_WORKSPACES, GET_TREE, SUBSCRIBE to workspace and window events), one re-read per burst | ✅ | ✅ | ✅ | ✅ |
| Mango: tags, title, the launcher's windows | ✅ Mango's own socket, read only (two `watch` lines), one `poll`: tags per output (active, urgent, occupied; empty ones hidden), the overview, a focus history for the launcher | ✅ | ❌⁴ | ✅ | ✅ |
| Extensions | script mode, no plugin API | ✅ | ✅ ~100 | ❌ | ➖ |

¹ waybar + mako + swayosd + fuzzel. ² ashell + swaync. ³ waybar + mako +
swayosd + swaybg, from PSS figures of different days (see the memory note
above). Other shells' features from their documentation or source (October 2026);
memory measured on the author's laptop. ⁴ No Mango backend found in Noctalia
(October 2026, unverified).

## What's in the repository

A Cargo workspace of four crates:

| Crate | Role |
|---|---|
| `alpheus` | **Control center** as a TUI (ratatui), opened on demand |
| `alpheus-demon` | **Resident daemon**: the bar, one per screen (layer-shell) |
| `alpheus-launch` | **Scripting bridge** to the daemon's launcher, notifications, OSD and quick panel (`--toggle`, `--dmenu`, `--dnd`, `--quick`…) |
| `alpheus-commun` | Shared foundation: command execution, configuration, theme, access to system tools |

The TUI doesn't embed Wayland, the daemon doesn't embed ratatui: a test
checks this.

### The control center (`alpheus`)

Seven tabs: **Home**, **Sound**, **Network**, **BT** (Bluetooth), **Power**,
**Displays** (enable, place, set resolution and scale, profiles, with
automatic rollback after 15 s if not confirmed), **Theme** (choose a
theme, the light/dark mode, the module colours and the bar style, with a preview; `--tab theme`).

| Key | Action |
|---|---|
| `1`…`7`, `Tab` / `Shift+Tab` | switch tabs |
| `Ctrl+P` | command palette (fuzzy search) |
| `/`, `Ctrl+F` | search a list |
| `F1`, `?` | help |
| `Esc` | go back; `q` quits |

`alpheus --tab <name>` opens directly on a tab (`home`, `sound`, `network`,
`bluetooth`, `power`, `displays`, `appearance`; `theme` is accepted for
`appearance`).

### The bar (`alpheus-demon`)

In the default `flat` style, a line of monospace characters on the theme's bar colour, modules separated by
`│`, the active workspace on the theme's accent (see [Themes](#themes) and [Bar styles](#bar-styles)).

![The alpheus bar, sample data, real font, default theme (catppuccin, dark) (zoomed ×2, split in two halves)](docs/images/bar.png)

![The tray with fictional apps, one icon's menu and the calendar (sample data, real font, default theme (catppuccin, dark), zoomed ×2; the two popups composed in one image)](docs/images/bar-tray.png)

![The media module and its popup, fictional players and track (sample data, real font, default theme (catppuccin, dark), zoomed ×2)](docs/images/bar-media.png)

![Notification bubbles, top centre, one critical (fictional notifications, real font, default theme (catppuccin, dark), zoomed ×2)](docs/images/notifications-bubbles.png)

![The notifications panel under the bell: the player, the history, do-not-disturb (fictional data, real font, default theme (catppuccin, dark), zoomed ×2)](docs/images/notifications-panel.png)

![The OSD: volume, microphone muted, brightness (fictional levels, real font, default theme (catppuccin, dark))](docs/images/osd.png)

![The quick settings panel: volume and output, brightness, microphone, do-not-disturb, power profile, keep-awake, night light, session buttons (fictional data, real font, default theme (catppuccin, dark), zoomed ×2)](docs/images/quick.png)

![The quick settings' Wi-Fi detail: the connected network, saved ones, one connecting (`…`), signal and lock marks, More… (fictional networks, real font, default theme (catppuccin, dark), zoomed ×2)](docs/images/quick-wifi.png)

Shipped: **launcher** (an icon that opens it, first module by default),
**workspaces** (one dot per window, empty workspace dimmed, the active one
on `accent_container`, urgent in `urgent`, click to go there, wheel to move to the next one), **active
window** (app icon and title), **media** (`▶ Title` while a player plays, `⏸ Title` paused, hidden when nothing plays; middle click play/pause, wheel next/previous; a left click opens a popup with the artist, the elapsed time and a progress bar, ⏮ ⏯ ⏭, and the players to choose from),
**privacy** (nothing while nothing is captured; then one glyph per capture in an own block: `󰍬` microphone in orange, `󰄀` camera in green, `󰍹` screen in red, whatever `module_colors` says; the microphone glyph is dim while the default source is muted. Read from PipeWire's graph by the `pw-dump` child the sound module already runs, so a meter (pavucontrol, cava), a filter chain or a loopback never lights it, and an app muting itself in its own call still does; a screen cast announced by niri or Hyprland lights the screen too. A glyph goes 0.5 s after its capture ends, never blinking on a renegotiation. A left click opens a popup listing the apps by name (`(unnamed)` when only the compositor knows); a middle click mutes or unmutes the default microphone. A camera opened directly (`/dev/video*` outside PipeWire) lights it too: the daemon watches the device nodes with inotify (`[privacy] dev_dir`, `proc_dir`: where video devices and processes are listed) and, 300 ms after an open or close, scans its own user's `/proc/<pid>/fd` for a link to a `video*` node (never opening one; PipeWire, WirePlumber and the daemon itself excluded). **Not seen:** wf-recorder under Sway and Mango, which announce nothing, a camera held by another user's process, and a same-user process named `pipewire` or `wireplumber`; under Hyprland, a cast running before the daemon started shows only through PipeWire (niri reports it at connection). **Known:** on boards whose hardware codec nodes are named `video*` (some ARM boards), a codec in use also lights the camera; the nodes are never opened to tell them apart),
**volume** (gauge, wheel ±5 %; with the quick panel, a left click opens it and a right click the TUI's sound tab, else a right click
mutes), **network** (link icon, VPN count), **Bluetooth** (device and its
battery), **battery** (laptop and peripherals; `warn` below 20 %, `error` below 10 %), **resources** (CPU graph,
memory, temperature), **tray** (status icons of running apps as Nerd
Font glyphs — or a letter —, left click activates, right click opens the
app's menu in a popup, keyboard and mouse),
**notifications** (a bell with the unread count, crossed under do-not-disturb, dim while another
server holds the name; a left click opens the panel — the player, the last 50 notifications, Do not
disturb, Clear all — a middle click toggles do-not-disturb; bubbles top centre on the focused output:
a left click runs the app's default action, a right click dismisses, critical ones stay until
clicked), **OSD** (a pill bottom centre on the focused output for the volume and mute, read from PipeWire's own state — any app, any key, a headset's buttons — with the new sink's name when the default sink switches; for the microphone only when its mute flips, never for a change of its level, which call apps (WebRTC) adjust on their own; and for the laptop's brightness on `alpheus-launch --osd brightness`, read once from sysfs, never written; it goes 1.3 s after the last change and never takes a click), **quick settings** (a panel under the volume, network or Bluetooth module, or `alpheus-launch --quick [sound|wifi|bluetooth]`: the volume with its output devices and, for a Bluetooth headset, its Hi-Fi/Mic mode (a switch of its PipeWire profile with `wpctl set-profile`), the backlight, the Wi-Fi (the radio, and a detail of the visible networks: saved ones connect by their profile, open ones by name, a new WPA-personal one with a password typed in the panel and handed to `nmcli` on its standard input; a failed new network's profile is removed; `More…` opens nmtui), the Bluetooth (power, and a detail of the paired devices and of the nearby ones found while it is open: connect or disconnect, pair a new one — a keyboard's code to type or a code to confirm shown in the panel by alpheus's own pairing agent, then trusted and connected —, and forget one after `Forget? ⏎`; the Bluetooth module's left click opens it), the microphone, do-not-disturb, the power profile from power-profiles-daemon, keep-awake and night light as commands of your own, and lock, log out, reboot and power off — reboot and power off ask once more; every change is a command such as `wpctl`, `brightnessctl` or `powerprofilesctl`, never a write of the bar's own; a row whose tool, device or command is missing does not show), **clock** (a click opens the
month's calendar), **power** (⏻ at the bar's end: a menu with Lock, Log out, Reboot and Power off, run with the quick panel's `[quick]` commands; reboot and power off ask once more, and a double click never confirms; an entry with no command does not show, nor the button without any). Clicking a module opens the matching tab
of the control center (with the quick panel: the right click on volume, network and Bluetooth). One bar per screen, screens plugged and unplugged
are tracked live. On a narrow screen, secondary modules disappear;
workspaces and the clock remain.

The lock screen is paused (the wallpaper, swaybg, is optional): the focus is
polishing the bar, the launcher, the notifications and the control center.

At rest the bar never wakes up except for its timers (the clock, and the
resources every 2 s when shown): measured 0 wake-ups at rest, ≈ 8.0 MB
of memory at start (≈ 8.4–8.5 MB with a tray of 10 items, launcher module off) and ≈ 9.6–9.7 MB under load,
on three outputs. The
compositor is only **read** (Hyprland's or niri's event stream, Sway's i3 IPC, Mango's watch sockets); switching
workspace goes through a configurable command. Sound comes from
`pw-dump`, network, battery and Bluetooth from D-Bus (read-only; the quick panel's Bluetooth acts through a second system-bus connection (BlueZ's own calls and its pairing agent), open only while its detail is open or a call is in flight),
resources from `/proc`. The tray talks to the session bus
(StatusNotifierItem, dbusmenu): one connection on the bar's own loop, no
thread. The media module speaks MPRIS on that same connection (or on one of its own when the tray is off,
shared with the light/dark portal): players found at start and on arrival, their changes read from
signals, never polled; while its popup is open the time ticks once a second, and only then.
The notifications module is the `org.freedesktop.Notifications` server on that same connection. It
waits in the D-Bus queue while another server (mako) holds the name, and takes over as soon as it is
released; a mako started after it is refused and exits. Text is capped and cleaned: markup is
stripped, never rendered, and images are skipped unread. A noisy app is rate-limited, and the bubbles
exist only while one shows. One timer is armed for the earliest expiry, and none at rest. Scripts drive it through `alpheus-launch`: `--dnd on|off|toggle` sets
do-not-disturb (kept across restarts, in `$XDG_STATE_HOME/alpheus/dnd`), `--clear-notifications`
empties the history; exit code 2 when the notifications module is not in `bar.toml`, 3 when the bar
is not running. `--osd brightness` shows the backlight's level (exit 0 also when there is none to
show, or the `osd` module is off; 3 when the bar is not running). `--quick [sound|wifi|bluetooth]` opens or closes the quick panel on the focused output (`wifi`, `bluetooth`: on that detail; exit 0 also without the `quick` module; 3 when the bar is not running or older than `--quick` (or than its `wifi` or `bluetooth` word)); a `--quick` arriving less than 250 ms after the one that opened the panel is ignored, so a compositor that re-fires a press bind does not close it at once. A known
limitation: right-to-left titles (Arabic, Hebrew) are drawn in logical order, not reordered. On Hyprland (any version; hyprwm/Hyprland#15729, fixed upstream in 0.56.2), the bar's keyboard interactivity stays None while a menu or the calendar grabs — detected from its `hyprland_*` globals.

### The launcher

A centred box (apps, open windows, `$PATH` commands, a script/dmenu mode
for shell scripts), opened and closed by the daemon itself — nothing of
it stays resident while closed, not even a thread.

![The alpheus launcher, All mode, sample data (fictional apps and windows), default theme (catppuccin, dark), zoomed ×2](docs/images/launcher.png)

`Tab`/`Shift+Tab` switches modes (All, Apps, Windows, Commands); typing
ranks results with [nucleo](https://github.com/helix-editor/nucleo)
(Helix's fuzzy matcher) plus frecency (most-used first on an empty
query). `Enter` runs a command or typed shell text in a held terminal
(stays open to read the output); `Ctrl+Enter` runs it detached. `alpheus-launch`
bridges to it over an owner-only socket (`toggle`, `close`, `--dmenu`
with fuzzel's exit codes, plus exit code 3 — daemon unreachable — the
only case where a script falls back to another menu). Measured in
**release** (headless sway, 300 synthetic apps): opening costs ≈ 1.9–2.1 MB
of PSS over closed, median open latency ≈ 0.8–1.3 ms, 50 open/close cycles
grow PSS by well under the 1 MB budget (measured 148–220 KB across runs),
0 wake-ups in 5 s at rest after use (2 in one of three runs). A 100,000-line dmenu peaks at
+6.6–6.8 MB and drops back to +0.5 MB a second after it is cancelled.

**Command sources.** A menu script's lines can be searched next to the apps.
A `[[sources]]` table in `launcher.toml` names a `list` command and a `run` command. `list` runs once per open, on the thread that already scans the applications, after them: 1 s at most, 2,000 lines, 256 KiB. `run` receives the chosen line as one argument, never through a shell.

A line `Captures · Screenshot zone` shows as **Screenshot zone**, then `Captures` dimmed. It is found by typing either part. It is never listed on an empty query, in another mode or behind a prefix. Its use is remembered for the ranking like an app's.

```toml
[[sources]]
name = "Tools"                     # 1 to 32 characters, unique, no control character (logs, history)
list = ["my-menu", "--list"]       # argv, no shell
run  = ["my-menu", "{line}"]       # "{line}": the chosen line, as one argument
```

A source that fails, hangs or floods is cut off and logged once. The launcher opens as usual.
Whatever the list command started is killed with it when the read ends. If the daemon itself is
killed mid-read, the list command dies too, but processes it left in the background do not; a
systemd unit with `KillMode=control-group` (the default) cleans them up.

While the launcher is open, each kept line costs about three times its text (the line, its entry
and its path): ≈ 6–8 MB at the bounds (8 sources × 256 KiB) plus the ranker, released on close. A
300-line source costs ≈ 0.5 MB (table above).

`alpheus-launch --dmenu` takes fuzzel's dmenu options that scripts use:

| Option | What it does |
|---|---|
| `-p`, `--prompt TEXT` | text shown before the input |
| `--index` | print the chosen line's 0-based position on standard input instead of its text, empty lines counted as fuzzel does (`-1`, exit 0, for typed text that matches no line, as fuzzel) |
| `--password` | draw one `•` per typed character, show no list (standard input is read and ignored), print the text as typed; never logged nor kept in the history, wiped from memory on close; not with `--index` |
| `--visible N` | with an empty query, list only the first N non-empty lines; typing searches every line (a script's home menu: its categories first, every entry by typing); not with `--password` |
| `--placeholder TEXT`, `--width N`, `--lines N` | accepted and ignored (the box keeps its own size) |

A running `alpheus-demon` older than `--index`/`--password`/`--visible` (upgraded but not restarted) refuses
them: `alpheus-launch` then exits 3, like an unreachable daemon, so a script falls back to another
menu with the same options.

### Themes

Colours come from a **theme**: one of nine compiled-in presets, or your
wallpaper through [matugen](https://github.com/InioX/matugen). Each has a dark
and a light variant. The mode follows the desktop (the portal's
`org.freedesktop.appearance` `color-scheme`, read by the bar, which passes it
on to the control center) or is fixed. The bar, its popups, the launcher and the
control center change together. The bar picks up a new
`theme.toml`, `colors.toml` or `colors-light.toml` by itself, without a
restart: one inotify watch, nothing wakes at rest. If `~/.config/alpheus` is
itself a symlink (home-manager), the watch follows it to its target, so
replacing the link is not seen until the bar restarts (the bar logs it once at start).

![The bar in Tokyo Night (dark), Catppuccin (light) and Gruvbox (dark), then the light theme's menu and calendar (sample data, zoomed ×2)](docs/images/themes.png)

```toml
# ~/.config/alpheus/theme.toml — every key optional
theme = "catppuccin"   # a preset below, or "wallpaper"
mode  = "auto"         # "auto" (follow the desktop), "dark", "light"
module_colors = "icons" # "none", "icons" (default), "full", "pills": see Module colours below
style   = "flat"        # "islands", "powerline", "floating", "underline", "translucent": see Bar styles below
opacity = 0.8           # translucent only

[colors]               # override any role, whatever the theme and mode
# accent = "#ff9e64"
```

With no `theme.toml`, or no `theme` key in it, alpheus uses `wallpaper` when
`~/.config/alpheus/colors.toml` exists (what it did before themes existed),
and `catppuccin` otherwise. An unknown name falls back to `catppuccin`, with
one line in the log.

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

A preset with no light variant stays dark in light mode. Every preset is
checked for legibility in both modes by a test using WCAG contrast:
- text reaches 4.5:1 on the bar and the popups;
- secondary text and the state colours reach 3:1 on the bar.

Where an official colour falls short in a light variant, alpheus uses a
darker shade of it. Nord's bar sits on `nord0` rather than `nord1`, so that
its red stays readable.

A theme is 16 colours, named by use:

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
| `accent2_container` | reserved (no surface uses it yet; delivery 2) |
| `ok`, `warn`, `error`, `urgent` | states: low battery is `warn`, critical `error`, an urgent workspace `urgent` |

#### Module colours

A theme also carries **seven hues**: `red`, `orange`, `yellow`, `green`,
`cyan`, `blue` and `purple`. Each preset takes them from its own palette
(darker shades in the light variants where the official one is too pale to
read, 3:1 on the bar at least). `module_colors` in `theme.toml` decides how
much of each module wears its hue:

| `module_colors` | Look |
|---|---|
| `none` | no hue: the bar looks as before the hues existed |
| `icons` (default) | each module's icon takes its hue |
| `full` | the icon and the module's text |
| `pills` | each block filled with its hue (block styles, see Bar styles below) |

The mapping is fixed:

| Module | Hue |
|---|---|
| window title (its icon) | purple |
| media | purple |
| sound | blue |
| network | cyan |
| Bluetooth | purple |
| battery | green |
| CPU (its graph) | orange |
| RAM | yellow |
| temperature | red |

With `graph = 0`, only `full` colours the CPU (its icon is the graph).

Workspaces, the clock, the tray icons, the notification bell
(its count in the accent), the launcher button and the separators keep their colours. **States win over the hue**: a low (`warn`) or
critical (`error`) battery, a hot temperature, a network with limited
connectivity, a muted volume, an offline or
down module, an urgent workspace and a tray item that needs attention keep
their state colours whatever `module_colors` says. A charging battery is
green at any level (its glyph shows it charges, so it never warns); under `none` it
keeps the accent. The Theme tab of the
control center has a "Module colours" row (None, Icons, Full, Pills) with a preview.

A colour file may carry the seven hues as optional keys (`red = "#f38ba8"`,
and so on); a hue that is absent is derived from Catppuccin's (Mocha for dark,
Latte for light) mixed 30 % toward the accent, so it stays in tune with the
theme. `[colors]` in `theme.toml` can override a hue like any role.

#### Bar styles

The bar has six shapes. `flat`, the default, is the bar as it always was.

```toml
# ~/.config/alpheus/theme.toml
style   = "islands"  # "flat" (default), "islands", "powerline", "floating", "underline", "translucent"
opacity = 0.8        # translucent only: 0.3 to 1.0 (clamped, with one line in the log)
```

Only `translucent` uses `opacity`; a value out of range still logs its warning under any other style.

| `style` | Look |
|---|---|
| `flat` | one line on the bar colour, modules separated by `│` |
| `islands` | each group of modules in its own rounded block; the bar between them is transparent |
| `powerline` | full-height blocks joined by arrows, alternating two tones |
| `floating` | one rounded bar, 6 px from the top and sides of the screen, with a light shadow |
| `underline` | blocks one step lighter than the bar, a thin line under it, the active workspace in the accent, underlined |
| `translucent` | the bar at `opacity` over the wallpaper, solid blocks |

![flat](docs/images/style-flat.png)
![islands](docs/images/style-islands.png)
![powerline](docs/images/style-powerline.png)
![floating](docs/images/style-floating.png)
![underline](docs/images/style-underline.png)
![translucent](docs/images/style-translucent.png)

*The six styles on catppuccin (dark), over a fictional wallpaper (sample data, real font, zoomed ×2, each bar split in two halves).*

The groups are the workspaces, the window title, the status modules (sound, network, Bluetooth,
battery), the resources, the tray, the clock and the launcher button. A module that is absent
makes no block. The text inside each block is the same in every style; only the background
changes.

`module_colors = "pills"` fills each block with the hue of its first module. Its text and icons
take whichever of the bar colour or the text colour reads better on that hue, and states still win
(a low battery stays `warn`). Pills apply to `islands`, `powerline`, `floating` and `translucent`.
Under `flat` and `underline`, `pills` looks like `full`.

![pills](docs/images/style-pills.png)

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

#### Wallpaper colours (matugen)

`theme = "wallpaper"` reads `colors.toml` (dark) and `colors-light.toml`
(light). If only the dark file exists, light mode uses it too. A file gives
the 16 roles above. The 7 keys of earlier versions (`bg fg accent muted
surface error ok`) still work, and the other roles are derived from them.

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

`amber` is a custom colour, tinted toward the wallpaper. The seven hues are
custom colours too, harmonised with the wallpaper: add these lines to the
template, in the same way (the keys are optional; without them alpheus
derives the hues from the accent):

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

`alpheus-light.toml` is the same with `output_path = "~/.config/alpheus/colors-light.toml"`.
Then, at each wallpaper change:

```sh
matugen -c ~/.config/matugen/alpheus-dark.toml  image "$WALLPAPER" -m dark  -t scheme-vibrant
matugen -c ~/.config/matugen/alpheus-light.toml image "$WALLPAPER" -m light -t scheme-vibrant
```

`-t scheme-vibrant` gives livelier colours than matugen's default
(`scheme-tonal-spot`). The bar follows the new files on its own. Run from a
script with no terminal (a wallpaper hook), matugen cannot ask which of a
picture's dominant colours to use: add `--source-color-index 0` to pick the
first.

## Build

With Nix:

```sh
nix build .#alpheus          # the TUI
nix build .#alpheus-demon    # the bar
nix develop                  # dev shell: cargo, fonts, sway for tests
```

Without Nix, `cargo build --release` builds the three programs (the daemon needs
`pkg-config`, libdbus and libxkbcommon development files; see
[docs/install.md](docs/install.md)). The bar looks
for its fonts in `$ALPHEUS_POLICES` (set at build time by the flake),
otherwise in the system's.

## Configure

Everything is optional, under `~/.config/alpheus/`. An invalid file falls
back to default values with a warning, never a crash: every file rejects
unknown keys, so a single typo (or an unknown module name) discards that
whole file. The daemon logs to its standard error.

When the files are read: `bar.toml` **once, when the daemon starts** (restart
`alpheus-demon` after editing it); the theme files (`theme.toml`,
`colors.toml`, `colors-light.toml`) are **watched and reloaded live**;
`launcher.toml` is read **at each open** of the launcher; `config.toml` and
`displays.toml` once, when the control center starts (restart it to apply an edit).

- `theme.toml`: the theme (a preset or the wallpaper), light/dark mode,
  per-role overrides, the bar style and its opacity; `colors.toml` / `colors-light.toml`: the wallpaper's
  colours, written by matugen. See [Themes](#themes); the bar follows changes
  without a restart, the control center reads them when it opens.
- `config.toml`: the TUI (keys, tools opened for sound, network,
  Bluetooth).
- `displays.toml`: screen profiles, written by the Displays tab.
- `bar.toml`: the bar.
- `launcher.toml`: the launcher (terminal command, result rows, history
  size, mode prefixes, the focus command, command sources); read fresh at each open,
  nothing kept while it is closed.

```toml
# bar.toml — default values
modules = ["launcher", "workspaces", "window", "media", "privacy", "volume", "network",
           "bluetooth", "battery", "resources", "tray", "notifications", "osd", "quick", "clock", "power"]
height = 22

[font]
family = "JetBrainsMono Nerd Font Mono"
size = 14.0
fallback = ["DejaVu Sans Mono"]  # for symbols the font lacks (✳ ◐…); setting the key replaces this default, [] disables it

[clock]
format = "%H:%M"
date = "%a %e %b"   # empty = no date
calendar = true     # a left click on the clock opens the month
week_numbers = false  # ISO week numbers in the calendar

[tray]
passive = "dim"     # "dim" | "show" | "hide": items that say they are idle
tooltips = true     # the item's name after ½ s of hovering

[tray.icons]        # glyph overrides, keyed like the built-in table (id, icon name, title, process)
# keepassxc = "󰌋"

[media]
title_max = 30            # cells of the title in the bar (8 to 80)
ignore = ["kdeconnect"]   # bus-name substrings never queried; playerctld is always ignored

[privacy]
ignore = []               # app names (whole name, any case) never shown; an indicator left with no name and nothing unnamed goes off
# dev_dir = "/dev"        # where video devices are listed (watched, never opened)
# proc_dir = "/proc"      # where processes are listed (the camera scan)

[notifications]
timeout = 5          # seconds, when the app leaves it to the server
timeout_min = 2      # an app's own timeout is kept within these
timeout_max = 20
max_visible = 4      # bubbles shown at once; more wait
width = 40           # cells (24 to 80)
history = 50         # entries kept in memory (0 to 200)

[osd]
timeout_ms = 1300    # how long the pill stays after the last change (600 to 5000)
width = 32           # cells of its row (24 to 60)
margin = 48          # px above the bottom edge (0 to 400)
backlight = ""       # a device in backlight_dir; "": the first one, in sorted order
backlight_dir = "/sys/class/backlight"   # read only: on `alpheus-launch --osd brightness` and when the quick panel opens

[quick]
step = 5                                   # % per ←/→ or wheel notch on a slider (1 to 25)
keep_awake = { toggle = [], state = "" }   # toggle: a command, run as given; [] hides the row
night_light = { toggle = [], state = "" }  # state: "", "file:<path>" (on while it exists) or "process:<name>" (on while it runs)
lock = ["loginctl", "lock-session"]        # session buttons and the ⏻ menu's entries, run as given; [] hides one
logout = []
reboot = ["systemctl", "reboot"]           # reboot and power off ask once more
poweroff = ["systemctl", "poweroff"]
wifi_more = ["foot", "-e", "nmtui"]        # More… in the Wi-Fi detail; absent: launcher.toml's terminal, then nmtui; [] hides it

[actions]
open_tab = ["alpheus-toggle", "{tab}"]   # a click on volume, network, Bluetooth or battery: {tab} becomes sound, network, bluetooth or home
resources = ["foot", "-e", "btop"]       # a left click on the resources module
# {tab} is "sound" for volume, "network" for network, "bluetooth" for Bluetooth and "home" for battery; no other module runs open_tab.
# With the quick panel (`quick` in modules) a left click on volume, network or Bluetooth opens the panel instead, and open_tab runs on the right click.

[resources]
interval = 2         # seconds between two readings of /proc (1 to 60)
graph = 20           # cells of CPU history (0 to 60; 0: no graph)
sensor = ""          # an hwmon name or a thermal zone type; "": automatic

[workspaces]
command = ["wm-set", "workspace", "{workspace}", "{output}"]

[battery]
peripherals = "always"   # "always" | "low" | "never": peripherals with a real percentage
coarse = "low"           # "always" | "low" | "never": peripherals with only a coarse level
```

An unknown module name in `modules` is an error that makes the whole
`bar.toml` fall back to its defaults (one warning in the log), like any other
invalid key.

The launcher, in `launcher.toml` (defaults shown; `[[sources]]` are described
under [The launcher](#the-launcher)):

```toml
terminal = ["foot", "-e"]                        # Terminal=true apps, plain
terminal_hold = ["foot", "--hold", "-e"]         # a command or typed shell text on Enter: the terminal stays open
rows = 10                                        # result rows (3 to 30)
history = 200                                    # entries kept (0 to 1000; 0: no history file)
focus = ["wm-set", "focus-window", "{window}"]   # run to focus a window; {window} is required (address:0x… on Hyprland, id:N elsewhere)

[prefixes]
commands = ">"       # non-empty, no leading space, neither a prefix of the other
windows = "w "
```

`terminal`, `terminal_hold` and `focus` are separate argv lists (program then
arguments, no shell); switching terminal means setting both of the first two.

The control center, in `config.toml` (defaults shown):

```toml
[keys]
dvorak_nav = true    # d/h/t/n also move left/down/up/right, beside the arrow keys

[keys.bindings]      # optional: action = ["chord", …], replaces that action's keys
# palette = ["ctrl+k"]

[commands]           # opened in alpheus's own window (program, then arguments)
mixer = ["wiremix"]
network = ["nmtui"]
bluetooth = ["bluetui"]
```

Action names for `[keys.bindings]`: `quit`, `back`, `help`, `palette`,
`search`, `next_tab`, `prev_tab`, `tab_1`…`tab_7`, `left`, `right`, `up`,
`down`, `page_up`, `page_down`, `home`, `end`, `toggle`, `activate`,
`decrease`, `increase`. A chord is optional modifiers joined by `+` (`ctrl`,
`alt`, `shift`) then a key: a single character (`?`, `-`, `+`, `k`…), or
`esc`, `enter`, `space`, `tab`, `backspace`, `up`, `down`, `left`, `right`,
`pageup`, `pagedown`, `home`, `end`, `f1`…; case does not matter (`ctrl+p`,
`shift+tab`). A key given to one action is taken away from the others; an
unknown action or key makes the file invalid.

## Principles

- **Lightness first.** An unused option costs nothing; no heavy dependency
  "for config". Budgets are checked by tests (memory and idle wakeups of
  the bar, measured on a headless sway).
- **Never `dpms`** nor sleep handling: turning off a screen on a
  DisplayLink output can freeze the machine.
- **No direct writes to the compositor**: only configurable commands.
- **Tests without a graphical session**: every command goes through an
  injectable executor, no test acts on the session.

## Documentation

- [`docs/install.md`](docs/install.md): build and runtime dependencies, a flake
  input, starting the daemon, key binds for Hyprland, Sway, niri and Mango.
- [`docs/compositors.md`](docs/compositors.md): what each compositor gives the
  bar, and the external commands (`wm-set` and others) alpheus expects.
- [`docs/cli.md`](docs/cli.md): the command lines of `alpheus`,
  `alpheus-demon` and `alpheus-launch`: verbs, options, exit codes.
- [`docs/SCOPE.md`](docs/SCOPE.md): why, budget, guardrails, breakdown.
- [`docs/specs/`](docs/specs/): the specifications (TUI, Displays tab,
  bar, launcher, tray, themes, media, notifications, OSD).
- [`docs/plans/`](docs/plans/): the implementation plans.

## License

[MIT](LICENSE) © 2026 Mae PUGIN
