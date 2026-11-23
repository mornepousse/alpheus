# Architecture

A map for someone who wants to change alpheus and has to find where. It says what each folder
is for and what it must never do; it does not repeat the code. Names to search for are in
`code font`; lines are not linked because they move. The reasons behind the rules are in
[`decisions.md`](decisions.md) and [`SCOPE.md`](SCOPE.md); each feature has a page in
[`features/`](features/README.md). Words in the [glossary](GLOSSARY.md) are linked at their first
use. The `//!` header of each source file is the detailed reference and says which spec and
decisions it follows. Further reading: [`tour.md`](tour.md) follows one battery change from the
D-Bus thread to the screen, and [`adding-a-module.md`](adding-a-module.md) is the recipe for a new
module, walked through the timer.

## Bird's-eye view

alpheus is a Wayland desktop shell for people who want it light: a status bar, an application
launcher, a notification server, an on-screen display, a quick settings panel and a terminal
control center. It is split in two programs that share a foundation:

- the **daemon** (`alpheus-demon`) runs all session long. It is one process with one event loop;
  it draws the bar and everything that pops up from it, and it does nothing unless something
  happened (a message from a [source](GLOSSARY.md#source), a Wayland event, a deadline).
- the **control center** (`alpheus`) is a terminal program (ratatui) that you open to change a
  setting. It runs the system's tools through a worker thread and exits when closed.
- `alpheus-launch` is a tiny client that sends the daemon one [verb](GLOSSARY.md#verb) on a socket
  (open the launcher, show the OSD, switch do-not-disturb...).

Two ideas run through all the code. The first is a split inside every feature: a **pure model**
(plain data and functions of their arguments, tested without Wayland, a clock or a process) and
the [glue](GLOSSARY.md#glue) that connects it to the outside. In a feature folder the model is
`mod.rs` and the glue is `app.rs` (a `surface.rs` holds the Wayland side where there is one); a
model never reads a clock, a file or starts anything. The second is that lightness is a checked
rule, not a wish: a module that is not in `modules` creates no thread, connection or timer, and
at rest there are no [wake-ups](GLOSSARY.md#wake-up) (see [Threads](#threads) and
[`measurements.md`](measurements.md)).

## Crates

One Cargo workspace, four crates:

| Crate | What it is | Does not depend on |
|---|---|---|
| `alpheus-common` | the shared foundation: the injectable command [executor](GLOSSARY.md#executor) (`exec`), configuration, theme model, the TUI's backends (sound, network, Bluetooth, brightness, energy, power, displays, diagnostics), compositor detection and the launcher's wire protocol (which also carries the Displays tab's verbs) | ratatui, Wayland |
| `alpheus` | the TUI control center (ratatui) | Wayland |
| `alpheus-daemon` (binary `alpheus-demon`) | the resident daemon: bar, launcher, notifications, OSD, tray, quick settings, privacy indicators, Shield mode, displays (the screens' layout through `zwlr_output_manager_v1`, profiles on hotplug) | ratatui |
| `alpheus-launch` | a small client for the daemon's socket (`--toggle`, `--dmenu`, `--quick`, ...) | Wayland, fonts |

These boundaries are enforced from `Cargo.lock` by `crates/alpheus-common/tests/boundaries.rs`: a
dependency added across a line fails the build's tests. The TUI re-exports the shared modules, so
paths like `alpheus::backends::...` still work.

**Architecture invariant:** the foundation carries no display dependency
(`the_foundation_carries_no_display_dependency`), the TUI no Wayland
(`the_tui_does_not_carry_wayland`), the daemon no ratatui (`the_daemon_does_not_carry_ratatui`),
`alpheus-launch` only `alpheus-common` (`alpheus_launch_stays_tiny`), and the D-Bus library stays
in the daemon (`dbus_stays_in_the_daemon`, `neither_zbus_nor_blocking_is_in_the_daemon`).

## Entry points

**The daemon.** `crates/alpheus-daemon/src/main.rs` is one line: `main` calls
`alpheus_daemon::startup::run`. `startup::run` installs the panic hook first, limits glibc's malloc
arenas, blocks the terminating signals before any thread exists (`shutdown::block_signals`), takes
the single-instance lock (`lock::acquire`), binds the launcher socket, reads the configuration
(`load_settings`) and wraps the one executor in `NonBlocking`. Then `build_app` loads the fonts,
connects to Wayland, builds `wayland::App` and the calloop `EventLoop`, with the Wayland queue as
one of its sources (`WaylandSource`). `insert_core_sources` adds the [mail](GLOSSARY.md#mail)
channel, the clock and, when shown, the resources timer; `start_modules` and `start_sources` start
what the configured modules [need](GLOSSARY.md#needs); `start_guard` starts the signal thread;
`main_loop` calls `event_loop.dispatch` until `App::stop` is set; `shut_down` ends. The loop lives
in `startup.rs`, not in `wayland.rs`: `wayland.rs` holds the `App` struct and the Wayland handlers.

**The control center.** `crates/alpheus/src/main.rs` parses the command line with
`app::run::parse_options` and calls `app::run::run`, which builds the `App`, starts the worker
(`worker::spawn_with`) and a guard thread, and loops in `event_loop`: send the requests the `App`
queued, draw (`ui::render`), wait up to 100 ms for a key or mouse event, hand over the worker's
answers, call `App::on_tick`.

**`alpheus-launch`.** `crates/alpheus-launch/src/main.rs` calls `alpheus_launch::run` with the
arguments, the socket path (`launcher_ipc::socket_path`) and the standard streams, and exits with
its return code (the codes are in [`cli.md`](cli.md)).

## The daemon's codemap

`crates/alpheus-daemon/src`, by what a file is for. Most folders are *features*: `mod.rs` is the
model, `app.rs` the glue, and the feature's page in [`features/`](features/README.md) says what it
does for the user.

### The core

- **`startup.rs`, `startup/`.** Assembles the daemon (above) and owns the loop. `Mail` is what the
  other threads send the loop; `App::on_bar_message` is where a bar message (`Msg`) is applied, and
  where the hooks, the battery alerts and the privacy feed are fed from.
  **Architecture invariant:** the panic hook is installed before any thread starts
  (`the_panic_hook_is_installed_before_anything_else`), and the `started` hook runs only once the
  launcher socket is watched (`hooks_start_after_the_launcher_socket_watch`).
- **`state.rs`, `system.rs`, `config.rs`, `modules.rs`.** `state::Bar` is the pure state: what the
  sources said, nothing else. `system.rs` holds the normalised readings (sound, network,
  Bluetooth, power, resources). `config.rs` reads `bar.toml` once at start and defines `ModuleId`
  (the bar's modules in their default order). `modules.rs` has one row per module (`MODULES`) and
  the `Needs` flags: the sources a module asks for, so that an absent module costs nothing.
- **`layout.rs`, `segments.rs`, `grid.rs`.** From state to a screen: each module produces a
  [segment](GLOSSARY.md#segment), `layout` places them left to right in `bar.toml` order and
  writes a `Grid` of cells and click zones, with one `Group` per [block](GLOSSARY.md#block) under
  the block styles. The grid is the tested object; the painter only copies it.
- **`painter.rs`, `painter/`, `background.rs`, `placement.rs`.** Draws a grid into a shared-memory
  buffer with cosmic-text (`Fonts::paint_bar`), then the per-style background and line pass, the
  corner masks and the opaque region; `placement.rs` says where a bar's
  [layer surface](GLOSSARY.md#layer-surface) sits per style.
  **Architecture invariant:** the daemon paints with the theme's 16 roles only
  (`the_daemon_paints_with_roles_never_the_legacy_palette`).
- **`wayland.rs`, `wayland/`, `popup.rs`, `list.rs`, `keys.rs`, `retired.rs`.** `App` and its
  Wayland handlers (one `Top` layer surface per output, the pointer, the keyboard); `popup.rs` has
  `OpenPopup` and the [popup body](GLOSSARY.md#popup-body) `PopupBody` (the popup that hangs from
  the bar); `list.rs` the rows of cells the launcher, the menus and the calendar share; `keys.rs`
  the keys the daemon's surfaces understand; `retired.rs` the Wayland objects that must not be
  dropped inside the loop's dispatch.
  **Architecture invariant:** a released keyboard is retired, then dropped after the dispatch
  (`a_released_keyboard_is_retired_then_dropped_after_the_dispatch`), and a popup finds its
  parent bar by surface identity, never by name
  (`popup_close_and_output_gone_find_their_parent_by_identity_never_by_name`).
- **`deadline.rs`.** The [deadline timer](GLOSSARY.md#deadline-timer) `DeadlineTimer`: one calloop
  timer at the earliest deadline while there is one, none otherwise. Every feature that needs a time
  (tooltips, notification expiry, the OSD, confirmations) owns one; none ticks.
- **`actions.rs`, `nonblocking.rs`.** Click and wheel to command (the workspace command is
  configurable); `NonBlocking` wraps the daemon's only executor and refuses the blocking `run`.
  **Architecture invariant:** every command a click on a full bar can start is on an allowlist
  (`every_click_on_a_full_bar_stays_on_the_allowlist`) and the only system executor is built inside
  `NonBlocking` (`the_only_system_executor_is_wrapped_in_non_blocking`).
- **Small shared files.** `clock.rs` (local time, no date crate), `icons.rs` (a Nerd Font glyph
  for an app), `inotify.rs` and `watch.rs` (the one inotify wrapper and the theme's hot reload),
  `state_file.rs` (the hardened writer of small state files), `link.rs` (the bar's own session-bus
  connection when the tray is off), `memory.rs` (handing memory back after a peak), `sys.rs` (the
  one `poll` wrapper), `lock.rs`, `log.rs`, `text.rs`, `backlight.rs`, `session.rs`.
  **Architecture invariant:** inotify lives in `inotify.rs`, `watch.rs` and `privacy/camera.rs`
  alone, and only watches (`the_theme_and_camera_watches_only_read`).

### Sources: what feeds the loop

- **`sources/`.** The supervised threads that read the outside world and send bar messages:
  `spawn_source` gives a source its own small-stack thread, a backoff (1 s doubling to 30 s) and a
  "–" on a panic. Here live the compositor sessions (`hypr.rs`, `niri.rs`, `mango.rs`, `sway.rs`),
  the sound source (`sound.rs`, which reads a `pw-dump -m -N -R` child), the resources reader
  (`resources.rs`, no thread: it runs on the loop) and the system D-Bus thread (`dbus/`, one file
  per [domain](GLOSSARY.md#domain) plus the taps the panels use, see
  [scope and tap](GLOSSARY.md#scope-and-tap)). Compositor detection is `alpheus-common::compositor`;
  see [`compositors.md`](compositors.md).
  **Architecture invariant:** the compositor's requests are fixed lists and read-only
  (`compositor_requests_are_read_only`, `mango_writes_only_its_request_lines`,
  `sway_writes_only_its_three_messages`), its sockets are opened only under `sources/`
  (`compositor_sockets_opened_only_in_sources`), D-Bus is confined to `sources/` and its system
  connection can only read (`d_bus_is_confined_to_sources_and_read_only`), with the Bluetooth
  acting connection `dbus/bt_act.rs` as the one named exemption
  (`the_bluetooth_connection_acts_only_through_act_method`), systemd is asked for a count and
  nothing else (`no_acting_systemd1_method_in_the_daemon`), and the only command the daemon keeps
  running is the sound source's `pw-dump` (`the_only_stream_is_pw_dump_monitor`). The reasons for
  the exemptions are in [`decisions.md`](decisions.md): "UPower is the one service the daemon may
  start", "A source that is down is not a state".
- **`compositor/`.** The normalised compositor state (workspaces, windows, urgency, the active
  window) and the four parsers (`hypr.rs`, `niri.rs`, `mango.rs`, `sway.rs`) that fill it; the
  rest of the daemon sees only this state.
- **`pipewire/`.** The model of what `pw-dump -m` says: sinks, sources, defaults and, only when
  built for privacy, the capture graph (`graph.rs`); `tap.rs` is the `SoundTap` the quick panel
  shares with the sound thread.
- **`session_bus/` and `tray/`.** The tray and every other session-bus feature on a second,
  non-blocking dbus-rs connection driven by the loop: `router.rs` (`Tray`) owns the connection; the
  status-notifier watcher and host, the dbusmenu menus and the MPRIS and notification codecs live
  here; `tray/bridge.rs` and `tray/menu_app.rs` connect it to the bar. No thread.
  **Architecture invariant:** the tray's D-Bus is confined and allowlisted
  (`tray_d_bus_is_confined_and_allowlisted`), the portal is read through exactly two members
  (`the_portal_is_read_through_exactly_two_members`), and the files of the tray, the popups and
  the calendar start no thread and no command (`the_tray_spawns_no_thread`, through its
  `NO_THREAD_NOR_COMMAND` list).
- **`appearance.rs`.** The desktop's light or dark preference, read once from the portal and
  followed; it rides the tray's connection, or `link.rs` when the tray is off.

### Features

Each has a page in [`features/`](features/README.md) unless noted.

- **`quick/`** ([quick settings](features/quick-settings.md)). The panel under the bar's right
  end: rows (volume, brightness, microphone, do-not-disturb, power profile, toggles, Wi-Fi,
  Bluetooth, VPN, Shield, session buttons) and a [detail](GLOSSARY.md#detail) for each list
  (`wifi.rs`, `bt.rs`, `vpn.rs`, `dnd.rs`, `shield.rs`). `mod.rs` holds `Quick`; `app.rs` the
  wiring; `state.rs` the state reader; `bt_app.rs` Bluetooth's glue. The D-Bus thread reads only
  while the panel is open, through a `PanelScope` and a `Tap`. It must never run a command that is
  not configured or on the allowlist, never put a password on an argument line, never log a name.
  **Architecture invariant:** `every_quick_command_stays_on_the_allowlist`,
  `the_quick_model_and_its_state_reader_only_read`,
  `the_wifi_password_goes_to_stdin_only_and_is_wiped_after`.
- **`launcher/`** (keys in [`configuration.md`](configuration.md), verbs in [`cli.md`](cli.md)).
  The application launcher: a centred Overlay box inside the daemon (apps, windows, `$PATH`
  commands, script sources, prefixes, calculator, emoji, the clipboard history), created at open and
  freed at close. `host.rs` decides the socket's requests; `surface.rs` is the Wayland side;
  `socket.rs` and `socket_glue.rs` the socket; `scan.rs` the catalog built on a short-lived thread
  at each open; `tools/` (the [Tools menu](features/tools-menu.md)) is a mode of the box, and
  `tools/join.rs` is the one file that names the clipboard, Shield and diagnostics for it. Nothing
  of it is resident while closed.
  **Architecture invariant:** `nothing_resident_while_the_launcher_is_closed`,
  `the_launcher_socket_is_bound_only_in_socket_rs`,
  `every_launcher_activation_stays_on_the_allowlist`,
  `every_tool_run_is_a_configured_argv_with_the_choice_whole`,
  `the_parallel_branches_are_named_only_in_tools_join`.
- **`notify/`** ([notifications](features/notifications.md)). The model of alpheus's own
  `org.freedesktop.Notifications` server (cleaning, caps, the history, do-not-disturb, rules), the
  bubbles (an Overlay surface, `bubbles_surface.rs`), the panel under the bell and the battery
  alerts. The server itself is `session_bus/notifications.rs`. It must never panic on a client's
  text and never log it.
  **Architecture invariant:** `every_dnd_change_goes_through_dnd_apply`,
  `the_notification_server_has_no_panicking_path`, `the_notification_rules_log_nothing`.
- **`osd/`** ([the on-screen display](features/osd.md)). `Watch` decides when a sound change shows
  the [pill](GLOSSARY.md#pill); `surface.rs` is its Overlay surface, with an empty input region,
  dropped after 1.3 s. It reads sysfs and shows; it never writes.
  **Architecture invariant:** `the_osd_only_reads`,
  `the_osd_and_the_bubbles_are_created_on_the_overlay_layer`.
- **`media/`** ([media](features/media.md)). The MPRIS players and the popup, fed from
  `session_bus/mpris`.
  **Architecture invariant:** `the_media_files_have_no_panicking_path`.
- **`privacy/`** ([privacy](features/privacy.md)). Who holds the microphone, the camera or the
  screen, from the PipeWire capture graph, the compositor's screencast events and (`camera.rs`,
  `camera_app.rs`) a watch on `/dev/video*` plus one debounced `/proc/*/fd` scan. It never opens
  anything under the device directory and never logs a process.
  **Architecture invariant:** `the_camera_watch_opens_nothing_under_dev`,
  `the_camera_logs_name_no_process`, `the_privacy_module_has_no_panicking_path`.
- **`timer/`** ([the focus timer](features/timer.md), the reference feature: `mod.rs` model,
  `app.rs` glue, `popup.rs`). One `DeadlineTimer`, armed only while a timer runs; the model reads no
  clock.
  **Architecture invariant:** `the_timer_is_wired_through_one_path`,
  `the_timer_has_no_panicking_path`.
- **`resources/`** ([resources](features/resources.md)). The popup's rows and graphs; the ring of
  the last 60 samples is in `system::Resources`, fed by the loop's existing resources timer.
  **Architecture invariant:** `the_resources_popup_has_no_panicking_path`,
  `a_resources_tick_repaints_only_an_open_popup_after_applying_the_reading`.
- **`power/`** ([the power menu](features/power.md)). The ⏻ button's menu; it runs only a
  `[quick]` session command that is set, and reboot and power off ask first.
  **Architecture invariant:** `every_power_menu_activation_is_a_configured_session`,
  `the_power_menu_lets_go_of_its_timer_and_runs_only_its_entry`.
- **`failed/`** ([failed units](features/failed.md)). The `󰀦 N` count: the system manager's is a
  domain of the D-Bus thread (`sources/dbus/units.rs`), the user manager's a client of the session
  link (`failed/app.rs`).
  **Architecture invariant:** `the_user_manager_is_asked_for_one_property_and_subscribed_to`,
  `no_acting_systemd1_method_in_the_daemon`.
- **`shield/`** ([Shield mode](features/shield.md)). The state read from a flag file, its switch
  and the blocked USB devices, all through commands the user configures; one job at a time on a
  short-lived thread (`run.rs`), the flag watched in the theme watch's inotify instance. It does
  no root work.
  **Architecture invariant:** `a_shield_action_is_never_a_capture_and_shutdown_stops_the_commands`,
  `the_shield_logs_name_nothing`, `the_shield_watch_is_the_theme_watchs_instance`.
- **`hooks/`** ([event hooks](features/hooks.md)). A command of the user's per event, with the data
  in the environment. Each event's first value is a [seed](GLOSSARY.md#seed), and for the first 3 s
  (the [quiet window](GLOSSARY.md#quiet-window), `QUIET`) a change only moves its seed (decision:
  "A hook fires on a change, never for the state at start").
  **Architecture invariant:** `spawn_with_env_only_runs_hooks`,
  `hook_logs_carry_fixed_words_only`, `the_hooks_are_fed_where_state_changes`.
- **`outputs/`** ([displays](features/displays.md)). The screens' layout through
  `zwlr_output_manager_v1`: `wlr.rs` speaks the protocol, `model.rs` and `control.rs` are the pure
  model and controller (the trial, profiles on hotplug), `app.rs` the glue. The one place alpheus
  changes the compositor's state.
  **Architecture invariant:** `outputs_protocol_only_in_wlr_rs`,
  `output_power_management_never_appears`, `the_displays_are_restored_before_the_app_is_dropped`.
- **`calendar/`** ([calendar](features/calendar.md)). A month grid under the clock, dates worked out
  in the file (no date crate). It is on the `NO_THREAD_NOR_COMMAND` list of
  `the_tray_spawns_no_thread`.
- **`notes/`** (the pad of `alpheus-launch --notes`; no feature page yet). A plain-text pad in the
  launcher's box; its file is written only through `state_file`, and its save timer and paste pipe
  live only while it is open.
  **Architecture invariant:** `notes_files_are_written_only_through_state_file`,
  `the_notes_timer_and_pipe_live_only_while_open`.
- **`clipboard/`** (the history, off unless enabled; no feature page yet). `control.rs` is the one
  file that names the data-control protocols and the only `set_selection`; text only, bounded,
  memory only.
  **Architecture invariant:** `data_control_only_in_control_rs`,
  `the_clipboard_selection_is_set_only_by_the_own_source`,
  `the_clipboard_sensitive_hint_type_is_never_received`.

## The control center (`crates/alpheus/src`)

- **`app/`** is the state and logic: the `App` that every key, mouse event, worker answer and tick
  changes. `tab.rs` has one `TabSpec` per tab; each tab has `app/<tab>.rs` (items and keys) and
  `ui/<tab>.rs` (rows and shortcut hints). `input.rs` and `keymap.rs` route keys, `mouse.rs` the
  mouse. `worker.rs` is the one place that runs the backends while the program is up, on its own
  thread. `run.rs` is the real program; closing reverts an open display trial through the daemon.
- **`ui/`** draws an `App` (`ui::render`) and returns the map of clickable regions; `rail.rs` is the
  wide layout, `shape.rs` applies the theme's style to controls, `tabs.rs` dispatches to each tab's
  drawing.
- **`theme.rs`, `theme_file.rs`, `external_tools.rs`, `fuzzy.rs`.** The palette, the one file the
  Theme tab writes (`theme.toml`), the hand-over of the terminal to a tool such as wiremix, and the
  fuzzy search of the palette and lists.

The control center never talks to Wayland and never writes to the compositor; every command goes
through the executor. The Diag tab reads `systemctl`, `journalctl` and `coredumpctl` through the
executor's bounded `capture` on the worker, never in a render, and a system unit's restart runs in
the tab's own window so that the polkit agent can ask for a password (alpheus never runs `sudo` or
`pkexec`); see [the control center](features/control-center.md).
**Architecture invariant:** `every_launched_command_is_on_the_allowlist`,
`systemctl_never_starts_stops_or_changes_the_environment` and
`the_source_never_talks_to_the_compositor_or_the_displays_directly`, in
`crates/alpheus/tests/guardrails.rs`.

## The foundation and the client

- **`alpheus-common`.** `exec.rs` and `exec/` (the `Executor` trait, `SystemExecutor`,
  `FakeExecutor`); `config.rs` (`config.toml`); `theme/` (the 16 roles, hues, presets, the settings
  of `theme.toml`, the mode file); `backends/` (one trait per domain for the TUI); `displays/` (the
  layout model and the `displays.toml` profiles); `diagnostics/` (parsers, redaction, the bug
  report); `compositor.rs` (which compositor, from its environment variables; nothing here opens a
  socket); `launcher_ipc.rs` (the wire protocol: std only, every size bounded as soon as it is
  known); `shutdown.rs`, `paths.rs`, `text.rs` (outside text made safe for a cell), `local_time.rs`,
  `temp_path.rs`. It never depends on a display library (see [Crates](#crates)).
- **`alpheus-launch`.** One `run` function: parse a verb, send one request on
  `$XDG_RUNTIME_DIR/alpheus-launcher.sock`, print the answer. The socket is owner-only (mode 0600,
  other uids refused); the daemon's side is `launcher/socket.rs`. The TUI is its second client: the
  Displays tab's verbs and the Diag tab's `failed-refresh` go through it.

## Threads

The daemon is one event loop plus a few threads that block without a timeout:

| Thread | What it does | Started in | Lives |
|---|---|---|---|
| main (the loop) | calloop: Wayland, mail, deadline timers, the socket, inotify | `startup::main_loop` | always |
| `guard` | waits for SIGTERM, SIGHUP and SIGINT without running; asks the loop to stop; forces the exit after a one-second grace | `startup::start_guard` | always |
| `compositor` | one compositor session, read-only | `sources::start` (`spawn_source`) | always |
| `sound` | reads the `pw-dump -m -N -R` child's output (the child is a process, not a thread) | `sources/sound.rs` (`spawn_source`) | when the sound source is needed |
| `D-Bus` | the system bus, one connection, blocked on it | `sources/dbus/mod.rs` | when a domain is needed |
| a font thread | cosmic-text parses fonts with rayon; `RAYON_NUM_THREADS` is 1 | `startup::build_app` | always |
| the launcher's scan | the application and `$PATH` scan, then the command sources through `capture` | `launcher/scan.rs` (`SCAN_THREAD`) | one per open, ends after delivering |
| `alpheus-tools` | one Tools entry's state or choices, through `capture` | `launcher/tools/read.rs` | only while the Tools box is open |
| `alpheus-shield` | one Shield command or one blocked-device read | `shield/run.rs` | until it has reported |
| pipe readers | `exec.rs` drains a command's output on a helper thread so a full pipe cannot block it | `alpheus-common/src/exec.rs` (`drain`) | until the command ends |

At rest, with every module on, the daemon has **six threads**: the loop, `guard`, `compositor`, the
font thread, `sound` and `D-Bus`; everything else exists only while it has work. The count is
recorded in [`measurements.md`](measurements.md). The tests in
`crates/alpheus-daemon/tests/memory.rs` do not assert the number six itself: `at_rest_no_wakeups`
bounds the count at 12 and asserts that the `sound` and `D-Bus` threads and the loop do not wake,
and `startup_pss_per_backend` asserts that each compositor starts as many threads as the fake
Hyprland. Other tests assert that a feature leaves none behind (the launcher's cycle, and
`shield_threads_back_to_baseline`).

**Architecture invariant:** no thread, timer, file descriptor or keyboard of the launcher exists
while it is closed (`nothing_resident_while_the_launcher_is_closed`).

## Policy: what the daemon may touch

The guardrails of [`SCOPE.md`](SCOPE.md), checked mechanically by
`crates/alpheus-daemon/tests/guardrails.rs`, which reads the source files (comments stripped by
`without_comments` where a comment could fool a check) and fails on a forbidden pattern.

1. **No sleep or `dpms` handling.** Locking and blanking stay with an idle daemon.
   **Architecture invariant:** `dpms` is a forbidden word in the daemon's sources
   (`the_daemon_only_reads_from_the_compositor`) and in every crate's (the TUI's
   `the_source_never_talks_to_the_compositor_or_the_displays_directly`), and no output power
   management appears (`output_power_management_never_appears`).
2. **The compositor is only read,** with one exception: `zwlr_output_configuration_v1` in
   `outputs/wlr.rs` (and, besides the workspace command, the clipboard's own selection). Anything
   else that must change goes through a configured external command, see
   [`compositors.md`](compositors.md).
3. **Nothing resident without a reason.** A closed feature owns no thread, timer or keyboard.
4. **The system D-Bus is read-only** and called through the owner's unique name, never the
   well-known one (which would let D-Bus activation start the service); the exemptions are named
   and pinned (see Sources above).
5. **No command by hand.** Every external program is started through the executor.

## The executor

`alpheus-common::exec::Executor` is the **only** way alpheus launches a system tool (`run`, `spawn`,
`stream`, `capture`, `run_silent`, `spawn_with_input`, `spawn_with_env`). Production code uses
`SystemExecutor` (argument vectors, never a shell, a timeout, the whole process group killed and
reaped on timeout); tests use `FakeExecutor`, which records the exact command lines and answers
from a script. In the daemon the executor is wrapped in `NonBlocking`, which refuses `run` (it would
freeze every bar), lets clicks `spawn`, the sound source `stream` and the launcher's scan thread
`capture`; Shield's worker `run_silent`s its commands. This is what makes the rule "no test ever
calls a real `wpctl`, `nmcli`, `systemctl`..." practical.
**Architecture invariant:** only the source runners and Shield's runner call `capture`, and neither
builds a shell line (`only_the_runners_capture_and_never_through_a_shell`); `spawn_with_env` only
runs hooks (`spawn_with_env_only_runs_hooks`).

## Tests

- **Unit tests** sit beside the code (the pure models, `layout`, the compositor parsers).
- **Guardrail tests** read the *source files*: `crates/alpheus-daemon/tests/guardrails.rs`,
  `crates/alpheus/tests/guardrails.rs`, `crates/alpheus-common/tests/boundaries.rs`.
- **Integration tests** use a private `dbus-daemon` (never the session or system bus), fake Hyprland
  and Mango sockets serving recorded captures, a headless sway and fake `pw-dump` and MPRIS peers;
  those that need sway are `#[ignore]`d.
- **Budget tests** (`crates/alpheus-daemon/tests/memory.rs`) measure PSS and wake-ups of the real
  daemon and require a release build; how to run them is in [`measurements.md`](measurements.md).
- **Fixtures** are anonymised captures (`crates/*/tests/fixtures/`).
- Every test is expected to *bite*: see [prove-it-bites](GLOSSARY.md#prove-it-bites) and
  [`CONTRIBUTING.md`](../CONTRIBUTING.md). The [PSS](GLOSSARY.md#pss) and wake-up figures come from
  these [budget tests](GLOSSARY.md#budget-test).

## Lightness rules

The reason the project exists, applied as rules: resident memory is small and measured (a feature's
cost is measured against the commit before it, back to back, and recorded in
[`measurements.md`](measurements.md); the overall budget is 120 MB, the daemon sits at about 8 to
11 MB); an unused option costs nothing; at rest nothing wakes (event-driven sources, deadline
timers, no polling of D-Bus or `/proc` beyond the resources module's own interval); no heavy
dependency for configuration (TOML parsing and hand-rolled protocol clients over an async runtime;
the D-Bus stack is dbus-rs over the system libdbus); the TUI renders its first frame in under
100 ms.
