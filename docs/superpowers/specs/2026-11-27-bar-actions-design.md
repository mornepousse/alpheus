# Bar click actions and module tooltips: design

Date: 2026-11-27. Status: the owner approved the cluster's draft ("ok go", OWNER-DECISIONS.md of
the 2026-11-27 session): every open question is settled by the draft's recommendation, decision 7
applies. Calls marked *(writer's call)* were not settled by the draft or the decisions.

## Intent

A bar people can shape without forking it: each module's clicks and wheel turns can run the user's
own command or one of a few built-in verbs, and resting the pointer on a module spells out what its
glyphs mean, the way the tray's tooltips already do. Defaults stay exactly today's.

## Goals

- `[bindings.<module>]` in `bar.toml`: `left`, `middle`, `right`, `wheel_up`, `wheel_down`, each an
  argument list or a verb. A button left out keeps today's behaviour.
- A one-line tooltip after ½ s of rest on clock, battery, volume, network, Bluetooth, resources,
  window, `failed` and the bell, through the tray's existing hover timer and popup.
- One `tooltips` key for the whole bar (tray included).

## Non-goals

- **Autohide**: a later, separate cluster (its own spec, after the owner's compositor trial; decision 6
  settles its overlay behaviour there, not here).
- No shell (argv only), no hot reload, no binding on the tray (its gestures stay D-Bus calls), the
  workspaces (their clicks are navigation), or the modules that draw nothing (`osd`, `quick`,
  `displays`).
- No tooltip for launcher, workspaces, media, privacy, Shield, timer, power in this delivery: no text
  the bar does not already make plain.
- A shown tooltip is not refreshed: its text is taken when it shows.

## Anti-goals

- A tooltip that tells more than the bar draws: no SSID (decision 7: the bar never draws one, so the
  network tooltip never shows one), no sink or source label, no peripheral name, no window class, no
  app name. Window title and Bluetooth device are drawn (cut), so their tooltip may show them whole.
- A log line carrying a binding's command, a tooltip's text, or a title.
- A verb that writes to the compositor, or any DPMS/idle action.

## Constraints

- **Lightness**: no new dependency, thread, timer or poll. Bindings are a map, empty by default.
  Tooltips reuse the one `DeadlineTimer` armed only while the pointer rests on a zone; the popup
  surface lives only while shown. 15 MB at rest for everything.
- **Session safety**: bindings run through the `Executor` (`FakeExecutor` in tests, an argv recorder
  on a private `PATH` in the headless suite). The allowlist test keeps pinning the defaults.
- **Compositors**: nothing compositor-specific: `wl_pointer` motion and an `xdg_popup` without grab,
  the path the tray's tooltips already use on Sway and Mango. Sway is proved in the headless suite;
  Mango has no harness, so the owner checks once on his session (listed in the report).

## Approaches considered

1. **Per-module tables `[bindings.volume] right = [...]`** (picked, draft Q1a): same shape as
   `[actions]`, validated with `deny_unknown_fields`, five keys at most by construction.
2. i3status-rust `[[click]]` arrays: one more indirection (`button = "right"`), duplicates to detect.
3. waybar `on-click` keys inside each module's own table: most modules have no table today; it would
   spread the feature across twenty structs.

Tooltips: generalise `Hover`'s key (picked) rather than a second timer and popup path for modules.

## The design

### What the user sees

Unchanged bar. With `[bindings.volume] right = ["pavucontrol"]`, a right click on the volume runs
`pavucontrol`; its left click still opens the quick panel. Resting ½ s on the clock shows
`Friday 27 November 2026`; on the volume `Volume 45 %` or `Volume muted (45 %)`; on the network
`Wi-Fi · 1 VPN`, `Ethernet · limited connectivity` or `Offline`; on the battery
`Battery 60 %, discharging · Mouse 40 %` (peripherals by kind, only those the bar draws); on
Bluetooth the first device's whole name, its battery and `· 2 more`; on resources each figure of
`[resources] show` spelled out (`CPU 12 % · Memory 61 % · Temperature 60 °C`); on the window its whole
title; on `failed` `2 failed units (system 1, user 1)`; on the bell `3 unread · do not disturb`. A
feed that is down reads `<Module> unavailable`; one still waiting shows nothing. Exact wording is
the builder's within these examples.

### Configuration

```toml
tooltips = true                       # top level: modules and tray; false: no hover timer at all
[bindings.volume]                     # one table per module; a button left out keeps today's
right = ["pavucontrol"]               # argv, run as written; {tab} and {output} replaced
wheel_up = "none"                     # a verb: none, popup, quick, open_tab, launcher, dnd, mute
[bindings.clock]
middle = ["foot", "-e", "calcurse"]
```

- `tooltips` (default `true`). `[tray] tooltips` stays accepted as its old spelling: either one
  `false` turns every tooltip off *(writer's call: the draft's "one key, alias the old one" read as
  "false anywhere wins", so no existing file changes meaning)*.
- Bindable modules: every module that draws a segment except `workspaces` and `tray`. Another name
  is an error naming the key. A table for a module not in `modules` is accepted and costs nothing.
- Argv: 1 to 16 items, a non-blank program, no NUL; `{tab}` (only on a module with a control-center
  tab) and `{output}` (the bar's connector) each a whole argument, at most once, never the program
  (`check_placeholders`). `[]` is an error ("use \"none\"").
- Verbs: `none` (nothing, the default silenced), `popup` (the module's own popup; it must have one,
  and `quick` must be in `modules` when that popup is the quick panel), `quick` (the quick panel;
  needs `quick`), `open_tab` (`[actions] open_tab` with the module's tab; it must have one),
  `launcher` (toggles the launcher, same press rule as its button), `dnd` (needs `notifications`),
  `mute` (the sink mute command already on the allowlist).
- An invalid table makes the whole file fall back to defaults with one log line naming the key,
  never the command (the file's existing rule).

### How it is built

- `config.rs`: `Verb`, `Binding { Verb(Verb), Command(Vec<String>) }`, `ButtonBindings` (five
  `Option<Binding>`), `BarConfig.bindings: BTreeMap<ModuleId, ButtonBindings>` (`ModuleId` gains
  `Ord`), `BarConfig.tooltips: bool`, `check_bindings`.
- `grid.rs`: `Target::Passive(ModuleId)`: the cells of a module that has no click zone of its own
  today (the window; the clock with `calendar = false`) but has a binding or a tooltip. It takes no
  click of its own: `actions::command` and `ModulePopup::of` ignore it, so adding it changes no
  default. `layout.rs::segment` sets it when the segment has no target and
  `wants_zone(bar, module)` holds.
- `actions.rs`: `Bound { Nothing, Run(Vec<String>), Popup(ModulePopup), Launcher, Dnd }` and
  `bound(bar, output, target, button) -> Option<Bound>`, read before any default. `wayland.rs::click`
  asks it first; a `Popup` opens under the zone's anchor (`zone_anchor`, split from
  `module_popup_at`), `Launcher` goes through `decide_click`.
- `tooltips.rs` (new, pure): one text function per module, a `tooltip` column in `modules.rs`'s
  `MODULES` table (`Option<fn(&Bar) -> Option<String>>`).
- `session_bus/hover.rs`: `HoverKey { Tray(ItemKey), Module(ModuleId) }` and its borrowed
  `HoverRef`; `over` finds tray items and `Module`/`Passive` zones whose spec has a tooltip.
  `wayland.rs::hover_fire` builds the text from `self.bar` at fire time; `menu.tooltip_item` becomes
  the tooltip's owner (`Option<HoverKey>`): a module tooltip is never "gone" with a tray item;
  `hover_target_moved` finds the module's zone; a zone that moves or goes closes the tooltip (the
  tray's rule).

## Owner's decisions applied

Draft Q1 (a) tables per module; Q2 (b) argv plus a short verb list; Q5 / decision 7: the SSID is
never shown, because the bar never draws it (`NetState` holds none), and the rule is extended to
every tooltip *(writer's call: the sink label, peripheral names and window class are kept out on the
same ground)*; Q6 (a) one key. Q3, Q4 and decision 6 belong to autohide.

## Cost estimate

Disabled (no bindings, `tooltips = false`): an empty map (24 B), no zone added, no timer. Default
(`tooltips = true`, no binding): two `Passive` zones per grid (≈ 40 B each), the existing timer
armed only while resting on a zone and fired once per rest (one wake-up), a text and a popup surface
only while shown, freed on close as the tray's are; binary a few KiB. A binding spawns one child per
click, like `[actions]`. Expected: within noise of `main` at start, 0 wake-ups at rest.

## Tests

Config bounds and errors (`bindings_reject_…`, errors never quote the command); `bound` per verb and
button, empty tables change no click (byte-for-byte against `main`), a `Passive` zone takes no click;
`every_user_binding_runs_as_written` through `FakeExecutor`; tooltip texts per module with a guardrail
that `tooltips.rs` logs nothing, cannot panic and names no private field (`ssid`, `sink_name`, …);
hover over modules (arm once, cancel off the zone, no text arms nothing, `tooltips = false` arms
nothing, owner never gone with a tray item); headless on Sway: a binding runs its argv, a module
tooltip shows after ½ s and goes on leave, one fire then 0 wake-ups; memory back to back against
`main`.

## Left to the builder

Exact tooltip wording within the examples, error message wording, the order of the tooltip functions,
and whether `Bound` lives in `actions.rs` or its own file.
