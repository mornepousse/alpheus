# Architecture

A short map for contributors. The reasons behind each part are in
[`SCOPE.md`](SCOPE.md) (the original scope) and the specs in
[`specs/`](specs/); the per-module `//!` comments in the source are the
detailed reference and are kept current with the code.

Features are documented one page each in [`features/`](features/), the
[timer](features/timer.md) first.

## Crates

One Cargo workspace, four crates:

| Crate | What it is | Does not depend on |
|---|---|---|
| `alpheus-common` | the shared foundation: the injectable command executor (`exec`), configuration, theme model, the TUI's backends (sound, network, Bluetooth, brightness, energy, power, displays, diagnostics), compositor detection and the launcher's wire protocol (which also carries the Displays tab's verbs) | ratatui, Wayland |
| `alpheus` | the TUI control center (ratatui) | Wayland |
| `alpheus-demon` | the resident daemon: bar, launcher, notifications, OSD, tray, quick settings, privacy indicators, Shield mode, displays (the screens' layout through `zwlr_output_manager_v1`, profiles on hotplug) | ratatui |
| `alpheus-launch` | a small client for the daemon's socket (`--toggle`, `--dmenu`, `--quick`, …) | Wayland, fonts |

These boundaries are enforced from `Cargo.lock` by
`crates/alpheus-common/tests/boundaries.rs`: a dependency added across a
line fails the build's tests. The TUI re-exports the shared modules, so
paths like `alpheus::backends::…` still work.

## The daemon

`alpheus-demon` is one process with a **single event loop** (calloop) on the
main thread. It draws with cosmic-text into shared-memory buffers attached to
layer-shell surfaces, one bar per output, through smithay-client-toolkit.
Everything else follows from one rule: *nothing runs unless something
happened*.

- **State is pure.** `state::Bar` holds only what the sources have said;
  messages change it and `layout` derives, per output, a grid of cells that the
  painter draws. Styles and themes are another pass over that grid, not new
  state.
- **Sources feed the loop through channels.** A source is a supervised
  thread with a small stack (`sources::spawn_source`): a read-only
  connection, a backoff when it goes down (1 s doubling up to 30 s; the bar shows
  "–" meanwhile), a panic published as "–". The sources are:
  - the **compositor** (Hyprland, niri, Sway, Mango; one of four, detected
    once from the environment: see [compositors.md](compositors.md)),
  - **sound** (a long-lived `pw-dump -m -N -R` child: volume, default devices,
    the capture graph used by the privacy indicators),
  - the system **D-Bus** (NetworkManager, UPower, BlueZ, power-profiles-daemon
    and, a fifth domain, systemd's `NFailedUnits` for the `failed` module)
    on one thread blocked on the bus while nothing changes. The units domain
    reads one property (`Get`, never `GetAll`: the Manager's carries its
    environment) and calls `Subscribe()` once per owner only with
    `[failed] subscribe = true` (off by default); its reads ride the thread's existing
    wake-ups (a poke from the socket's `failed-refresh`, a resume, another
    domain's signal after 60 s), never a timer of its own.
  Resources (`/proc`, `/sys`) are a timer on the loop, not a thread. With
  every module on, the daemon measures six threads at rest; they are
  accounted for in the README's memory table. Short-lived workers come on
  top only while there is work: the launcher's scan thread, and
  `alpheus-shield`, which runs one Shield command or one blocked-device read
  and ends once it has reported (at most one of each at a time).
- **A guard thread** waits for SIGTERM, SIGHUP and SIGINT without running,
  asks the loop to stop and forces the exit if the loop does not finish within
  a one-second grace period. A single-instance `flock` in `$XDG_RUNTIME_DIR`
  keeps a second copy from starting.
- **Session-bus features run on the loop itself,** through a second,
  non-blocking connection (the tray's StatusNotifierWatcher and dbusmenu,
  MPRIS, the notification server, the portal's colour scheme, and the user
  manager's failed-unit count (`failed/app.rs`, the same typed `Get` and
  `Subscribe` allowlist as the system side); all in `session_bus/`): no new thread.
- **Timers are deadlines, not ticks.** `deadline::DeadlineTimer` arms one
  calloop timer at the earliest deadline while there is one and nothing
  otherwise: tooltips, notification expiry, the OSD's 1.3 s, popups, the
  quick panel's confirmations. At rest there are **no wake-ups**, which the
  tests assert.
- **Popups and panels** (menus, calendar, media, notifications panel, quick
  settings, power menu, privacy list) are `xdg_popup`s on the bar's own layer
  surface and share the launcher's list rendering and keys. The launcher, the
  notification bubbles and the OSD are their own Overlay-layer surfaces,
  created on show and dropped when done; the launcher binds the keyboard only
  while open.
- **Configuration.** `bar.toml` is read once at start; the theme files are
  watched by one inotify fd and reloaded live (the same fd also watches the
  directory of the Shield flag, when `[shield]` is set); `launcher.toml` is read at each
  open. `bar.toml` and `launcher.toml` with an unknown key or a bad value fall
  back wholly to the defaults, with one log line; `theme.toml` ignores only
  the faulty key.

### The control center's diagnostics backend

The Diag tab does not use D-Bus: its reads are `systemctl`, `journalctl`,
`coredumpctl` and `systemd-analyze` run through the executor's bounded
`capture` on the TUI's worker thread, parsed in `alpheus-common::diagnostics`
(`serde_json` joined that crate for it), never in a render, and only while the
tab shows. A restart or reset-failed of a *system* unit is the one thing the
TUI runs outside the executor, in its own window (`external_tools`), so
systemd's polkit agent can ask for the password on that terminal; alpheus
never runs `sudo` or `pkexec`. Compositor detection (`SWAYSOCK`,
`HYPRLAND_INSTANCE_SIGNATURE`, `MANGO_INSTANCE_SIGNATURE`, `NIRI_SOCKET`) lives
in `alpheus-common::compositor`, used by the daemon's sources and by the
tab's health check (which connects to the compositor's socket and closes it:
nothing is written). Every string from these tools passes one cleaner
(`alpheus-common::text`) before reaching a cell, and the bug report is
redacted (`diagnostics::report`) before it is shown or copied.

### The launcher bridge

`alpheus-launch` talks to the daemon over `$XDG_RUNTIME_DIR/alpheus-launcher.sock`
(mode 0600, peers of another uid refused). The TUI is its second client: the
Displays tab reads and tries the screens' layout there (`outputs`,
`outputs-try`, `outputs-keep`, `outputs-revert`), and the daemon holds the 15 s
trial; the Diag tab sends `failed-refresh` there so the bar's count follows. The wire protocol lives in
`alpheus-common::launcher_ipc`. The daemon's side is `launcher/socket.rs`, the
only place that binds it. Application and `$PATH` scans run on a short-lived
thread at each open; command sources (`[[sources]]`) run through the executor's
bounded `capture`.

The Tools menu (`launcher/tools/`: pure catalog, navigation and config; `launcher/tools/app.rs`:
the glue) is a launcher mode with no scan. Built-ins replay the daemon's own verbs
inside the process; the entry states and dynamic choices are read one at a time on a
short-lived `alpheus-tools` thread through `capture`; `launcher/tools/join.rs` is the only file
that names the clipboard, Shield and Diagnostics for it.

## Policy: what the daemon may touch

These are guardrails from the scope document, checked mechanically.

1. **No sleep or `dpms` handling.** Locking and blanking stay with an idle
   daemon; `dpms` is a forbidden word in the daemon's sources, and the D-Bus
   sources may not name `Sleep`, `Suspend`, `Hibernate`, `PowerOff` or `Reboot`.
2. **The compositor's sockets are only read.** Each compositor module has a
   fixed list of requests it may write (four for Hyprland, one subscription for
   niri, three messages for Sway, two `watch` lines for Mango); sockets are
   opened only under `src/sources/`. Anything that must change the compositor
   goes through a configured external command (see [compositors.md](compositors.md)),
   with **one exception**: `zwlr_output_configuration_v1` in the daemon's
   `outputs/wlr.rs`, which changes the screens' layout (the Displays tab's
   trial, profiles on hotplug); never output power management, never `dpms`.
3. **Nothing resident without a reason.** A closed launcher owns no thread,
   timer or keyboard.
4. **The system D-Bus is read-only.** The one connection on the D-Bus thread
   has only read methods, calls a service through the **unique name** of the
   process that owns it (never the well-known name, which would let D-Bus
   activation start it), and a guardrail test fails if a write word (`Set`,
   `Connect`, `ActivateConnection`, `GetSecrets`, `Pair`, …) appears there. The
   systemd1 calls are `Get` of `NFailedUnits` (the Manager interface only,
   never `GetAll`) and, with `[failed] subscribe = true`, `Subscribe()`, which makes systemd send its signals; no
   method of `systemd1` that acts (`StartUnit`, `RestartUnit`, `ResetFailed`,
   `KillUnit`, …) may appear under `sources/dbus/`.
   **The one named exemption** is the Bluetooth acting connection
   (`sources/dbus/bt_act.rs`), a second system-bus connection opened only
   while the quick panel's Bluetooth detail is open or a call is in flight,
   with a typed allowlist of methods (adapter power, device trust, discovery,
   pair, connect, disconnect, remove, and registering alpheus's pairing agent),
   every call going to BlueZ's unique name with auto-start off. Wi-Fi changes
   go through `nmcli`, not D-Bus. The session-bus pieces (tray, notifications,
   MPRIS, the portal) have their own allowlists, also pinned by tests.
5. **No command by hand.** Every external program is started through the
   `Executor` (next section); in the daemon nothing calls `Command` directly,
   and a guardrail checks it.

## The executor

`alpheus-common::exec::Executor` is the **only** way alpheus launches a system
tool (`run`, `spawn`, `stream`, `capture`, `run_silent`, `spawn_with_input`). Production code
uses `SystemExecutor` (argument vectors, never a shell, a timeout, the whole
process group killed and reaped on timeout); tests use `FakeExecutor`, which
records the exact command lines and answers from a script. In the daemon the
only executor is wrapped in `NonBlocking`, which refuses `run` outright
(it would freeze every bar) and lets clicks `spawn`, the sound source `stream`
and the launcher's scan thread `capture`; Shield's worker `run_silent`s its commands (output to `/dev/null`, so a privileged helper is never cut by a closed pipe) and `capture`s its device list. This is what makes the rule "no test
ever calls a real `wpctl`, `nmcli`, `systemctl`, `wm-set`…" practical.

## Tests and guardrails

- **Unit tests** sit beside the code (pure models: `quick/mod.rs`, `privacy/mod.rs`,
  `media/mod.rs`, `layout`, the compositor parsers).
- **Guardrail tests** (`crates/alpheus-daemon/tests/guardrails.rs`,
  `crates/alpheus/tests/guardrails.rs`, `crates/alpheus-common/tests/boundaries.rs`)
  read the *source files* and fail on a forbidden pattern: D-Bus confined to
  `sources/` and read-only, the Bluetooth connection acting only through its
  allowlist, compositor requests read-only, the launcher socket bound in one
  file, no thread or timer while the launcher is closed, and so on.
- **Integration tests** use a private `dbus-daemon` (never the session or
  system bus), fake Hyprland/Mango sockets serving recorded captures, a
  headless sway, and fake `pw-dump`/MPRIS peers. Those that need sway or
  `wtype`/`grim` are `#[ignore]`d and run in the dev shell.
- **Budget tests** (`tests/memory.rs`) measure PSS and wake-ups of the real
  daemon and **require a release build**.
- **Fixtures** are anonymised captures (`crates/*/tests/fixtures/`).

## Lightness rules

The reason the project exists, applied as rules:

- Resident memory is small and measured; a feature's cost is measured against
  the commit before it (back to back, separate target directories, deltas rather
  than absolutes) and recorded in the README's tables. The overall budget is
  ≤ 120 MB; the daemon sits around 8 to 11 MB.
- **An unused option costs nothing**: a module not in `modules` creates no
  thread, connection or timer; surfaces exist only while shown.
- **Zero wake-ups at rest.** Event-driven sources, deadline timers, no polling
  of D-Bus or `/proc` beyond the resources module's own interval.
- No heavy dependency "for configuration"; TOML parsing and a hand-rolled
  protocol client are preferred over an async runtime. The D-Bus stack is
  dbus-rs over the system libdbus, shared with the rest of the session.
- The TUI renders its first frame in under 100 ms.
- Don't hard-code what is specific to one workstation (scripts, layouts,
  paths) without a neutral default or a configurable path.
