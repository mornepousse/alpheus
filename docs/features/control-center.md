# The control center

`alpheus` is the terminal control center: a ratatui program with eight tabs (Home, Sound, Network,
Bluetooth, Power, Displays, Theme, Diag). This page is written one tab at a time, in the order the
tabs are brought to the house standard (`docs/STYLE.md`); today it holds the Sound tab, the first
one done. The tabs that are not described here yet are described in the README's control center
section.

How the program is laid out, for every tab. The state lives in `crates/alpheus/src/app/` (one
`App` that every key, mouse event and worker answer changes, with no terminal and no thread of its
own) and the drawing in `crates/alpheus/src/ui/` (`render` draws the whole screen and returns the
map of clickable regions). A tab that has code of its own has a file of the same name in each
folder: `app/sound.rs` and `ui/sound.rs`. What every tab shares stays in `app/mod.rs`,
`app/tab.rs` (one row of plain data per tab), `ui/tabs.rs` (the item rows), `ui/rail.rs` (the
rail, the tab hues) and `ui/mod.rs` (the frame and the status line's hints).

## Sound

### What it does

The Sound tab lists the audio **outputs** (sinks), then the **inputs** (sources), each with a volume
slider, and a **Per-app mixer** button at the end. A dot marks the default output and the default
input. A node's name comes from `wpctl status`. While the audio data is loading the lists show
`…`; if the read failed they show its error; a list with nothing in it shows `none`.

```
 Outputs
 ▶   Dock D6000 Dell                 ██████████████████░░░░░░  75 %
   ● CasqueBT                        ███████████████░░░░░░░░░  62 %

 Inputs
   ● Stereo Microphone               ████████████████████████ 100 %

   [ Per-app mixer ]
```

(The classic layout, in a window smaller than 84 columns by 26 rows. From that size the tab is
drawn with the rail: a blue hue for its headings and gauges, the focused row as a band, and a
knob on each gauge. The items and their order are the same.)

### Using it

| Key (default) | What it does |
| --- | --- |
| ↑ ↓ | move to the previous or next item |
| Enter | on an output or an input: make it the default; on the button: open the mixer |
| `-` `+` | lower or raise the volume of the focused node by 5 points |
| `/` or Ctrl+F | search: only the nodes whose name matches the text are listed (a fuzzy match) |
| Esc | clear the search |
| F5 | read the audio data again |

With the mouse: a click on a row selects it and makes it the default (the same as Enter); a click
or a drag on a gauge sets the volume; the wheel over a gauge changes it by 5 points, and the wheel
elsewhere moves the focus; a click on the button opens the mixer.

Two rules about the volume. It is never sent above 100 %: `wpctl` is called with `-l 1.0`. And a
node already above 100 % (another program can set it) is not lowered by `+`; going down from there
works. After a volume change the tab is not read again: the value shown is already the new one,
and a read in the middle of a drag would make the slider jump (a comment in `App::op_done`).

The **Per-app mixer** button opens the program set under `[commands] mixer` in `config.toml`
(default `wiremix`) in alpheus's own window. The other keys (the number keys for the tabs, Ctrl+P
for the palette) are the same on every tab, and `[keys.bindings]` renames any of them.

### Configuration

The tab has one setting of its own, in `config.toml`, read once when the control center starts:

| Key | Default | Meaning |
| --- | --- | --- |
| `[commands] mixer` | `["wiremix"]` | the program and arguments the Per-app mixer button runs |

The keys come from `[keys]` and `[keys.bindings]` (the names are `activate`, `increase`,
`decrease`, `search`, `refresh`; see the README's list of action names).

### How it is built

Two files, one per layer, and the tab's row in `app/tab.rs`.

- `app/sound.rs`: `items` (the focusable items, in drawing order), `App::visible_nodes` (the
  outputs and inputs that pass the search) and `App::set_default` (make a node the default). The
  audio data itself (`App::data.audio`) and the slider helpers (`slider_value`, `set_slider`,
  `nudge_slider`; a node's slider is a `SliderId::Node`) are in `app/mod.rs`, beside the volume
  and brightness sliders of Home.
- `ui/sound.rs`: `rows` (the heading, node and button rows) and `hints` (the keys named in the
  status line). A row is drawn by the shared item drawing in `ui/tabs.rs`.
- `app/tab.rs`: the tab's row says it is searchable, that it is read every 2 seconds while it is
  shown, from the `Domain::Audio` domain, and that its label column is 34 cells wide.

**Refresh.** Only the visible tab is read (`docs/specs/2026-10-02-alpheus-v1-design.md`, § 3:
nothing runs in the background and nothing runs when the window is closed). `App::on_tick` asks
the worker for `Domain::Audio` once the tab's period has passed; a failing read is tried again at
most every 15 seconds.

**The path of one change**, choosing another output as the default:

1. Enter, or a click, reaches `App::activate`, which sees the focused item is a
   `SliderId::Node` and calls `set_default` (`app/sound.rs`).
2. `set_default` marks the node as default in its own list at once, then calls `send_op` with
   `Op::SetDefault(id)`. `send_op` notes that `Domain::Audio` and `Domain::Volume` have a change in
   flight and leaves a `Request::Do` in the box the real loop empties.
3. The worker thread runs `wpctl set-default <id>` (`WpctlSound::set_default`), through the
   injectable command executor, and answers with `Update::Done`.
4. Until then a read of those domains that arrives is dropped (`App::on_update`). On the answer
   the tab is read again (`op_done`), so what is shown is what `wpctl` now says.

### Rules it keeps

- `activating_an_output_makes_it_the_default` (`app/tests.rs`): the output becomes the default, the
  default input does not move, and exactly one `SetDefault` is queued.
- `search_filter` (`app/tests.rs`): the search narrows the outputs and the items, and the focus is
  kept inside the list.
- `items_per_tab` (`app/tests.rs`): the order of the items, with the mixer button last.
- `volume_above_100_is_never_lowered_by_plus` (`app/tests.rs`).
- `sound_tab` (`ui/tests.rs`): the rows, the clickable regions and the hint on the status line.
- `sound_tab` (`ui/sound/tests.rs`, its snapshot in `ui/sound/snapshots/`): the whole 80 by 24
  screen.
- `every_launched_command_is_on_the_allowlist` (`tests/guardrails.rs`): `wpctl` is on the list of
  programs the control center may run, and no test runs the real one (they use the fake executor).

### What it costs

The control center is not measured tab by tab. What the specs fix for it as a whole: the first
render in under 100 milliseconds, and nothing read while a tab is not shown. This tab's reads are
one `wpctl status` every 2 seconds while it is shown, and nothing otherwise. (Reason for the 2
second period: not recorded.)
