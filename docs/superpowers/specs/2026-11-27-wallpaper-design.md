# Wallpaper picker: design

Date: 2026-11-27. Status: the owner settled the draft's questions by its recommendations ("ok go",
decision 5: names only). Calls marked *(writer's call)* are this page's; one point needs the
owner's go before Task 2 (see "Owner's go needed").

## Intent

Change the wallpaper from the Super+X panel without a script: pick a file of one folder by name,
alpheus restarts the user's setter (swaybg, wbg, swww) with it, then optionally runs matugen so the
bar's colours follow (`colors.toml` is already watched, live). The last choice comes back at login.

## Goals

- A `Wallpaper` tool in the Tools menu (Utilities), shown only when `[wallpaper]` is configured.
- Its level lists the folder's images by name, filtered by typing; Enter applies.
- The setter alpheus started is the only process it ever ends, by the handle it is the parent of.
- The last choice is re-applied when the daemon starts.
- `alpheus-launch --wallpaper next` for an external timer (cron, a systemd timer).

## Non-goals (later slices)

- Per-monitor wallpapers (`{output}`, one setter per connector): slice D.
- Rotation by a timer inside the daemon (`every = "30m"`): slice C's resident half. Only the
  external verb ships.
- Thumbnails, and any image decoder (decision 5). A JPEG decoder is a later dependency decision.

## Anti-goals

- alpheus never draws, scales, holds or decodes a wallpaper: the setter stays a borrowed tool
  (`docs/SCOPE.md`).
- Never `pkill swaybg`, never a process found by name, never a PID read from a file.
- Never a write to a compositor's config; never a disk cache.

## Constraints

- **Lightness.** Off (`dir` empty): no read, no stat, no thread, no timer, no byte. On and closed:
  one pidfd source per running setter, silent until it ends; no wake-up. Open: the listing lives
  on the Tools reader thread that already exists while the box is open, at most 2,000 names, freed
  on close.
- **Session safety.** Tests use `FakeExecutor` and, headless, a fake setter script on a private
  `PATH`; nothing calls swaybg, swww or matugen for real. The setter is a watched child with
  PDEATHSIG, so it dies with the daemon: no orphan for a test to chase.
- **Privacy.** A file name, the folder, an argv never reach a log, a toast or the history: the tool
  is recorded as `tool:builtin:wallpaper.pick` alone. Logs are fixed words.
- **Compositors.** Nothing is compositor-specific: swaybg and wbg need layer-shell (Mango and Sway
  have it), swww its own daemon. The daemon's environment already carries `WAYLAND_DISPLAY`.

## Prior art

waypaper drives the same setters (swaybg, swww, wbg) and has a post-command like our `then`, but
restarts swaybg with `killall`, which we refuse. Quickshell shells (caelestia, noctalia) and
HyprPanel pair a picker with matugen and show thumbnails, at the cost of an image stack. A user can
already write a `[[tools.entry]]` with `choices_cmd = ["sh","-c","ls …"]`; the built-in adds an
in-process listing, a name guard, the restart by handle and the login re-apply.

## Approaches considered

1. **A launcher prefix in Mod+Space.** Rejected: a prefix for something done a few times a month,
   and the `[prefixes]` clash rules grow.
2. **A user entry only (documented recipe).** Zero code, but no restart (a script would `pkill`),
   no re-apply at login, and `ls` output in a shell.
3. **A built-in Tools entry with an in-process listing and a watched setter (picked).** It reuses
   the Tools menu's levels, the reader thread, the executor's watched children (the recorder's
   mechanism) and the event loop's pidfd sources. The setter's lifetime is tied to the daemon, so
   the daemon re-applying at start is the whole login story.

## The design

### What the user sees

`Wallpaper` (glyph `\u{F0E09}`, falling back to the category's glyph when no font draws it) in
Utilities, description "Pick a wallpaper from your folder". No default letter *(writer's call: the
letters are full; `[tools] keys` gives one)*. Enter opens a level listing the folder's images,
sorted, by name only; typing filters; Enter applies and closes the box. An empty or unreadable
folder shows the level empty, as a failed `choices_cmd` does today. A failed setter is logged
(fixed word); the old wallpaper stays when the new setter could not start.

### Configuration (`launcher.toml`, new `[wallpaper]`, all off by default)

| Key | Default | Meaning |
|---|---|---|
| `dir` | `""` (feature absent) | the folder; a leading `~/` is expanded; absolute after expansion |
| `set` | `[]` | the setter's argv; `{file}` exactly once, as a whole element, never the program; required when `dir` is set |
| `restart` | `true` | the setter runs until replaced (swaybg, wbg); `false` for one that exits once applied (`swww img`) |
| `then` | `[]` | argv run after a successful set, `{file}` at most once as a whole element (matugen) |
| `remember` | `true` | keep the last choice's name in `$XDG_STATE_HOME/alpheus/wallpaper` and re-apply it at start |

Listed extensions: `png jpg jpeg webp bmp gif` (case-insensitive). `{file}` is the absolute path
`dir/name`, so it never starts with `-`. matugen runs only when `then` is set (decision on Q4).

*(writer's call)* The draft's `current = <path>` became `remember = true`: the name is written
through `state_file.rs`'s hardened writer (owner-only directory, atomic rename) instead of to an
arbitrary path, and since the setter dies with the daemon, re-applying is on by default whenever
the feature is.

### How it is built

- **`crates/alpheus-daemon/src/wallpaper.rs`** (pure, `wallpaper/tests.rs`): `WallpaperConfig`,
  `validate`, `resolve_dir`, `is_image_name`, `is_plain_name` (no `/`, not `.` or `..`, no NUL,
  not empty), `list(dir, cap) -> Listing` (one `read_dir`, regular files and links to them, images
  only, sorted, at most `MAX_FILES = 2000`), `fill(argv, file) -> Vec<String>`, `next_after(names,
  current) -> Option<&str>` (wrapping).
- **`crates/alpheus-daemon/src/wallpaper/setter.rs`**: `Setter`, holding at most one
  `Box<dyn Watched>`. `start` spawns the new setter (`Executor::spawn_watched`, from the event-loop
  thread), and only once it started drops the old handle (SIGKILL to its own group, reaped; the
  draft's "new first, then old" against a flash). `restart = true`: success is the start;
  `restart = false`: success is exit 0, heard on the pidfd. On success: write the name
  (`remember`), then spawn `then` through `run::launch` (scope, fire and forget). On failure: no
  `then`, the remembered name kept.
- **`crates/alpheus-daemon/src/wallpaper/app.rs`**: the glue on `App`: `wallpaper_apply(config,
  name)`, the pidfd source (`Generic`, as `captures/app.rs` does), `wallpaper_at_start`,
  `wallpaper_verb`.
- **Tools menu**: `BuiltinAction::Wallpaper` and its row in `catalog.rs` (46 built-ins);
  `Context.wallpaper` (dir set, `set` set, the folder is a directory: one `metadata` per open);
  `builtin_shape` gives it `Choices::Command`; `nav.rs`'s `drill_choices` emits
  `Job::Folder { entry, dir, token }`; `read.rs`'s `run_job` lists it on the `alpheus-tools` thread
  and answers `JobEvent::Choices`, so the delivery, the token check and the drawing are today's.
  `run.rs`'s `perform_tool` records the built-in without the name and returns
  `Done::BuiltinChoice`; `app.rs`'s `run_builtin_choice` hands the name to `wallpaper_apply`.
- **At start** (`startup.rs`): `settings` already reads `launcher.toml`; it keeps `[wallpaper]`.
  With `dir` and `remember` set and a remembered plain name present in `dir`, the setter is
  started once the loop exists; `then` is not run (the colour files are already written).
- **Verb**: `Request::Wallpaper(WallpaperVerb::Next)` in `launcher_ipc.rs`, `--wallpaper next` in
  `alpheus-launch`. The daemon re-reads `launcher.toml` (as an open does), lists the folder on the
  event loop (bounded, one `read_dir`), applies the name after the remembered one (the first when
  none). Answered Done, or an error when `[wallpaper]` is off.

## Owner's decisions applied

Names only, no decoder (5). Re-apply at login by the daemon (Q2 b). Rotation: the external verb
only (Q3 b). matugen only through `then` (Q4). Per-monitor later (Q5).

## Owner's go needed

`the_recording_signal_reaches_only_the_recorder_alpheus_started` allows `spawn_watched(` only in
`captures/tools.rs`, `captures/record.rs` and `nonblocking.rs`. The setter needs
`wallpaper/setter.rs` on that list. Recommendation: add it, and in the same commit pin that
`wallpaper/` holds no `.interrupt()`, `kill(`, `pkill`, `killall`, `pgrep`, `pidof` or `/proc/`,
and that the setter ends its child only by dropping the handle. Task 2 does not start without it.

## Cost estimate

- Off: 0 bytes, nothing read (pinned by `small_features_cost_nothing_at_start`).
- On, closed, with swaybg running: a `Box<dyn Watched>`, one pidfd and one calloop source, under
  1 KB anonymous; 0 wake-ups. swaybg itself stays the borrowed 1.4 MB row.
- Open: 2,000 names at most, ≈ 100 KB transient on the reader thread and in the level, back to
  baseline on close (as `tools_choices_2000_lines`).
- Apply: one spawn, plus `then`. Start: one small read, one `metadata`, one spawn.

## Tests (by task in the plan)

Config bounds; listing sorted, filtered, bounded; plain-name guard; argv filled whole with spaces
in names; restart drops only the old handle and only after the new one started; a failed set runs
no `then` and keeps the remembered name; a one-shot setter succeeds only on exit 0; the history
never holds a name; the folder is never read unless the level opens; the guardrail on logs; headless:
the fake setter starts, is replaced, dies with the daemon; no wake-up at rest; memory off/on/open.

## Left to the builder

The exact Nerd Font codepoint if `F0E09` is absent from the bundled font; the wording of the
config errors; whether `Listing` carries a `cut: bool` the level shows (recommended, no text).
