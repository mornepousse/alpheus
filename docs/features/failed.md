# The failed-units count

A small `󰀦 N` in the bar when systemd has units in the failed state. This page says what it does,
how to use it, how to configure it, how it is built, which rules the tests enforce on it, and what
it costs. It describes the code as it is today. The control center's Diag tab (which lists the
units and can restart or reset them) is a separate feature; this page is the bar's half.

## What it does

The `failed` module draws `󰀦 N` in the theme's error colour, in a block of its own, whatever
`module_colors` says. `N` is the system manager's count of failed units plus the user manager's
count, each when it is known.

- **Nothing is drawn** when `N` is 0, or while neither count has ever been known (a count never
  known is left out of the sum, and the bar draws nothing rather than a placeholder: see "The bar has no ignore list for failed units" in
  [decisions.md](../decisions.md)).
- **A count that becomes unknown keeps its last value.** The system bus going away, or the session
  link closing, neither hides the module nor changes the notice or the hook.
- **systemd's system manager failing to answer** (a timeout, an odd reply) is not the session's
  outage: the count is unknown for that read, one rate-limited log line says so, and the session
  and the other D-Bus modules go on. Only the bus itself going away ends the session.
- **The session link is never reconnected.** If it closes, the user count is not read again until
  the daemon restarts; the bar keeps the last one.
- The module sits after `shield` and before `power` in `ModuleId::ALL`, so in the default `modules`;
  on a narrow bar it is removed after `timer` and before `shield`.

The module reads one number from each manager and never a unit's name: for the names, open the Diag
tab.

## Using it

- **Left click** on the glyph opens the control center's Diag tab (through `[actions] open_tab`,
  with `{tab}` set to `diagnostics`). There is no other click.
- **`alpheus-launch --failed-refresh`** reads both counts again now. The control center sends the
  same request when the Diag tab is shown and after each unit action. The daemon answers `Done`
  even when the module is off (nothing is read then). A daemon older than the verb refuses it as an
  unknown request.
- **A notification**, if you turn `[failed] notify` on: one notice when the total rises, saying
  `N units failed` (or `1 unit failed`) with the body `Open Diag in the control center`. It is sent
  once per rise; a total of 0 arms it again.
- **A hook**: `failed_units` in `[hooks]` runs a command when the total changes (see
  [hooks.md](hooks.md)).

## Configuration

In `bar.toml`, read at start. The module must be in `modules`; the default list has it. A `modules`
list you wrote yourself needs `"failed"` added after `"shield"`.

| Key (under `[failed]`) | Default | Meaning |
| --- | --- | --- |
| `user_units` | `true` | also count the user manager's failed units (this uses the session bus); `false` leaves the session bus alone |
| `subscribe` | `true` | `true`: call `Subscribe()` on systemd so that a change arrives at once (the icon is instant; systemd and dbus-daemon then send a signal per unit change to every subscriber); `false`: the count is read on triggers only (see below) |
| `notify` | `false` | send one notification when the total rises (needs `notifications` in `modules`) |

With the default `subscribe = true` the count is read at start, on resume and on `failed-refresh`,
and systemd pushes every change; the read added to another D-Bus domain's wake-up is skipped. With
`false` that read comes back: once more whenever another domain wakes the daemon and the count was
last read a minute ago or more. A `Subscribe` that systemd refuses falls back to those triggers; on
the user manager's side the count is then re-read with the system count's reads (the next system
unit change, a resume or `failed-refresh`), so a failed user unit can show late, never lost.

```toml
[failed]
subscribe = false   # read on triggers only; systemd is not told to signal
notify = true
```

## How it is built

The counts come from two places, joined in one total.

- **The system manager's count** is the `Units` domain on the D-Bus thread's read-only system bus
  connection (`sources/dbus/units.rs`: one `Get` of `NFailedUnits` on `org.freedesktop.systemd1`'s
  owner, and `Subscribe` once per owner when `subscribe` is on). It reaches the loop as
  `Msg::Units`. After each successful read the thread also sends `Msg::UnitsRead`, which carries no
  count: it only tells the loop that the system count was read.
- **The user manager's count** is read on the loop, as one more client of the session bus link (the
  tray's connection, else the link the portal, media or notifications already opened, else one it
  opens). `failed/app.rs` holds `UserUnits`, which asks for the owner's name, reads the property,
  subscribes if wanted, and follows `NameOwnerChanged` and `PropertiesChanged`.
- `failed/mod.rs` is the pure part: `total` (the sum), `Notice` (when the notice goes), `incoming`
  (the notification), `notice_wanted`.
- The bar draws the module in `segments.rs` (`failed`) and gives it its row in `modules.rs`.

**The path of one change.** The D-Bus thread reads the system count and mails `Msg::Units`, then
`Msg::UnitsRead`. `on_bar_message` (`startup.rs`) gives the message to the hooks, then calls
`failed_units_read`, which reads the user count again; the answer comes back to `failed_user`, which
tells the hooks, applies `Msg::UserUnits` to the bar and calls `failed_notify`.

The tests are next to the files: `failed/tests.rs` (the model), `failed/app/tests.rs` (the user
manager's count against a fake outbox), `tests/failed_private_bus.rs` (a private bus) and the
`failed_*` tests of `tests/headless_session.rs`.

## Rules it keeps

- `the_user_manager_is_asked_for_one_property_and_subscribed_to` (`tests/guardrails.rs`): the
  only systemd call on the session bus is `Subscribe`; the count is one `Get` of one property,
  never a `GetAll`; `failed/app.rs` names no dbus type and logs fixed words only.
- `no_acting_systemd1_method_in_the_daemon`: nothing under `sources/dbus/` names a method that
  starts, stops, resets or changes anything of systemd's.
- `the_tray_spawns_no_thread`: `failed/app.rs` starts no thread, process or command.
- No timer and no polling: the system count rides the D-Bus thread's own wait, the user count the
  session link's own descriptor (`at_rest_no_wakeups_with_failed_units`).
- The reasons are in [decisions.md](../decisions.md): "A source that is down is not a state", "The
  failed-units count is pushed by a subscription, and read on triggers when it is off", "The failed-units
  count is asked for one property, never the whole interface", "The failed-units notice waits for
  both managers", "The bar has no ignore list for failed units".

## What it costs

Measured in release, back to back with the version before it (the full table is in [measurements](../measurements.md)).

- **Linked in, `failed` not in `modules`:** within the harness's spread at start (−392…+844 KB
  PSS over three pairings; the default bar's anonymous memory +12…+72 KB); the stripped daemon grew
  by 44 KiB.
- **Module on, both managers at 0** (`failed_module_pss_at_start`): +28…+36 KB of anonymous memory
  with `subscribe = false`, +12…+16 KB with `true` (the default); a later run read +32 KB with either.
- **A count shown, then 20 more changes** (`failed_module_shown_pss`): with `subscribe = true`
  (the default), `Pss_Anon` +20…+24 KB shown and +32 KB after 20 changes; with `false`, 0…+28 KB
  (bound 64 KB).
- **Wake-ups:** none at rest, with `subscribe` true or false, and none after a read added to
  another domain's wake-up (`at_rest_no_wakeups_with_failed_units`, `piggyback_costs_no_wakeup`).
