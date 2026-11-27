# The wallpaper picker

Pick a wallpaper from one folder, by name, from the Tools menu. alpheus never draws, scales or
decodes the image: it starts your own setter (swaybg, wbg, swww) with the file, optionally runs a
command after it (matugen, so the bar's colours follow), and starts it again with the last choice
when the daemon starts.

## Using it

Set `[wallpaper]` (a folder and your setter's command). Super+X shows **Wallpaper** in Utilities
(no letter of its own: `[tools] keys = { "wallpaper.pick" = "y" }` gives it one); type `/wall` to find it.
Enter lists the folder's images by name, sorted (png, jpg, jpeg, webp, bmp, gif; at most 2,000);
type to filter, Enter applies the one selected, after the box closed. The history keeps that you
used the tool, never the file's name.

**At login.** With `remember` (the default), the daemon applies the last choice again when it
starts, once its event loop runs: the setter is its child, so the wallpaper comes and goes with
the bar. `then` is not run then (the colours it wrote are already there). A remembered name that
is no longer an image of the folder is ignored.

**Rotation.** `alpheus-launch --wallpaper next` applies the image after the last one applied
(remembered, or in memory with `remember = false`; the first when none), then `then`. The daemon
steps from the name it last applied, held in its memory; before any, from the remembered one (with
`remember = false`, the first). A save that failed thus does not make `next` land on the same
image again. Call it from a cron line or
a systemd user timer
([../cli.md](../cli.md#alpheus-launch-scripting-bridge-to-the-daemon) has both). The daemon has
no timer of its own.

**One setter.** If your compositor's autostart starts swaybg (or another setter) itself, remove
that line once alpheus manages the wallpaper: otherwise two setters run, and the one alpheus does
not own stays on screen under or over the one it does. alpheus never ends a setter it did not
start.

## What it does

- The setter alpheus started is the only process the picker ever ends, by the handle of the child
  it is the parent of. A new setter starts first; only once it runs does the old one go, so the
  screen never flashes empty, and a setter that cannot start leaves the wallpaper as it was.
- A setter that runs until replaced (swaybg, wbg) is applied at its start; a one-shot setter
  (`swww img`, `restart = false`) only once it exits 0.
- After a set that worked, the name is kept (`remember`) and `then` runs; after a failed one,
  neither.
- A one-shot setter's success is heard on its pidfd through the event loop: without a loop or a
  pidfd, its exit 0 is lost (no name kept, no `then`) until the next start reaps it.
- With `restart = true`, success is the start: a setter that exits at once (an unreadable image)
  still counts as applied, and its end is then logged as `wallpaper: setter ended`.

## Configuration

`[wallpaper]` in `launcher.toml`, off by default: see
[../configuration.md](../configuration.md#wallpaper) (`dir`, `set`, `restart`, `then`, `remember`).

## How it is built

- `wallpaper.rs`, pure: the configuration and its checks, the folder's listing, the name guard
  (`is_plain_name`, `is_image_name`), `fill` (the element `{file}` replaced whole, so a name with
  spaces is one argument), `next_after`.
- `wallpaper/setter.rs`: `Setter`, holding at most one running setter and at most one one-shot not
  yet ended, each a `Box<dyn Watched>` (`Executor::spawn_watched`). `start` refuses a name that is
  not a plain image name of a file in the folder, starts the new setter, and only on success drops
  the old handle: the drop kills that child's own process group and reaps it. `child_ended` turns a
  pidfd's readiness into `Applied`, `Failed` or `Gone`. `apply_outcome` saves the name through
  `state_file.rs` (`$XDG_STATE_HOME/alpheus/wallpaper`, owner-only, atomic) and starts `then` in its
  systemd scope (`run::launch`).
- The Tools menu (`launcher/tools/`): `BuiltinAction::Wallpaper`, shown when `Context.wallpaper`
  (`[wallpaper]` set: no syscall on the event loop, a missing folder is the reader's to find). Its
  choices are `Choices::Command`: never in the flat search, never recorded. Enter on it hands the
  reader a `Job::Folder` (`ToolsState::with_folder` only keeps the folder), listed by `wallpaper::list`
  on the `alpheus-tools` thread and delivered as a choices read. The chosen name is recorded as
  `tool:builtin:wallpaper.pick` alone and handed to `wallpaper_apply` with the open's `[wallpaper]`
  once the box closed. A folder that cannot be read (missing, or a file in its place) logs
  `tools: folder read failed` and its level shows empty; one over 2,000 images logs
  `tools: folder cut, 2000 kept`.
- At start (`startup.rs`): `settings` keeps `launcher.toml`'s `[wallpaper]`; once the loop exists,
  `wallpaper_at_start` asks `start_choice` (picker on and `remember` set, else nothing is read; the
  remembered name read through `state_file::read_wallpaper`, then one `metadata` of it in the
  folder) and applies it without `then`.
- `--wallpaper next`: `Request::Wallpaper(WallpaperVerb::Next)`, the wire words `wallpaper next`,
  answered by `wallpaper_verb` before the launcher's guard (`launcher/socket_glue.rs`). It reads
  `launcher.toml` again, and `next_choice` lists the folder once on the loop (`wallpaper::list`,
  at most 2,000 names) and takes `next_after` the name `Setter::last_applied` holds (set by a set
  that worked, in memory only), or before any set the remembered name (none with
  `remember = false`): memory first, so a stale name left by a failed save is not read. Its refusals
  are fixed words:
  `wallpaper is off`, `the folder cannot be read`, `no image in the folder` (logged as
  `wallpaper: next refused`).
- `wallpaper/app.rs`: the glue on `App`. `wallpaper_apply` runs on the event loop's thread, so the
  setter's PDEATHSIG is the daemon's own: the setter dies with the daemon. Each setter's pidfd is a
  calloop `Generic` source, silent until it ends. Logs are fixed words: `wallpaper: applied`,
  `wallpaper: setter could not start`, `wallpaper: setter failed`, `wallpaper: setter ended`,
  `wallpaper: not saved`, `wallpaper: then could not start`, `wallpaper: next refused`.
- A folder on a hung mount (an NFS or sshfs server gone) blocks the bar for as long as the kernel
  waits: the start re-apply and every apply `stat` the file, and `--wallpaper next` lists the folder
  (one `read_dir`), all on the event loop. Accepted for v1 (the spec says so); the Tools menu's
  listing runs on the reader thread and is not affected.

## The rules its tests enforce

- `the_wallpaper_ends_only_the_setter_it_started` (guardrail): nothing in `wallpaper/` interrupts,
  releases or signals a process, finds one by name (`pkill`, `killall`, `pgrep`, `pidof`) or reads
  `/proc/`; `spawn_watched(` appears once, in `wallpaper/setter.rs`, before the handle is replaced.
  The watched-child list of `the_recording_signal_reaches_only_the_recorder_alpheus_started` holds
  `wallpaper/setter.rs` (the owner's go, 2026-11-27).
- `the_wallpaper_logs_fixed_words_only` (guardrail): every `log(` takes one string literal.
- `the_wallpaper_entry_is_absent_without_a_dir`, `the_folder_is_read_only_when_its_level_opens`,
  `a_folder_job_lists_names_like_a_choices_cmd`, `a_folder_cut_at_the_cap_says_so_in_a_fixed_note`,
  `the_wallpaper_folder_is_a_set_directory` (resolved, never stat'ed on the loop),
  `a_wallpaper_name_is_never_in_the_flat_search`,
  `a_wallpaper_choice_is_recorded_without_its_name`,
  `the_wallpaper_builtin_spawns_nothing_through_the_tools_glue`.
- Headless, with a fake setter on the private `PATH`: `tools_wallpaper_picks_and_starts_the_setter`
  (the file as one argument, the setter dying with the daemon) and
  `tools_wallpaper_replaces_the_setter` (one setter left after two picks).
- `the_remembered_wallpaper_is_applied_at_start_without_then`,
  `a_remembered_name_outside_the_folder_is_not_applied`, `nothing_is_read_at_start_when_off`,
  `next_applies_the_name_after_the_remembered_one`,
  `next_without_remember_steps_from_the_name_last_applied`,
  `next_with_a_failed_save_steps_from_the_name_last_applied`, `the_wallpaper_verb_round_trips`,
  `alpheus_launch_parses_wallpaper_next` (an older daemon: exit 3).
- Headless: `wallpaper_reapplied_at_start` (the remembered file, `then` not run),
  `wallpaper_next_steps_through_the_folder` (`a.png` then `b.png`, `then` after each),
  `wallpaper_next_steps_without_remember` (the same with `remember = false`, no state file),
  `wallpaper_setter_dies_with_the_daemon`,
  `tools_panel_at_rest_no_wakeups_after_a_cycle_with_a_wallpaper` (the entry shown, no loop
  wake-up after moves, no setter started).
- Memory (release): `small_features_cost_nothing_at_start` (an idle `[wallpaper]` on an empty
  folder, anonymous memory within 128 KB of none), `at_rest_no_wakeups_with_a_wallpaper_setter`
  (a setter running, 0 loop wake-ups in 5 s), `wallpaper_setter_replaced_50_times_no_growth` (one
  setter left, no thread left, anonymous memory within 128 KB), `tools_wallpaper_2000_files`,
  `startup_pss_per_source` (its `+ wallpaper` case).
- `a_restart_drops_only_the_old_handle_after_the_new_one_started`,
  `a_one_shot_setter_succeeds_only_on_exit_zero`,
  `a_failed_set_runs_no_then_and_keeps_the_remembered_name`,
  `then_runs_after_a_successful_set_with_the_file`, `a_name_not_in_the_folder_is_not_started`,
  `the_wallpaper_file_round_trips_and_refuses_a_path`.

## What it costs

Release builds, back to back against main (529a0bc6) where main has a counterpart, 3 runs each;
every figure is in [../measurements.md](../measurements.md).

- Off: nothing. The `Setter` on `App` is three empty options, 128 bytes, no allocation; nothing is
  read or started.
- On, nothing remembered (`[wallpaper]` on an empty folder): the daemon's anonymous memory 0 / 0 /
  +4 KB over the same start without it (`small_features_cost_nothing_at_start`, bound 128 KB).
- A setter running: its handle, one pidfd and one calloop source. 0 loop and 0 whole-daemon
  wake-ups in 5 s, no thread added (`at_rest_no_wakeups_with_a_wallpaper_setter`). The setter
  itself is the user's program and its memory is its own.
- At start with a remembered name: the same setter running, the daemon's PSS −121…+39 KB and
  anonymous memory −92…+68 KB against the same start without it, within noise
  (`startup_pss_per_source`'s `+ wallpaper` case).
- `--wallpaper next` keeps nothing measurable between calls: after 50 of them (each a setter
  started, the old one ended and reaped, the name saved) the anonymous memory is +0…+16 KB
  over the reading after the first two (bound 128 KB), one setter is left and no thread
  (`wallpaper_setter_replaced_50_times_no_growth`). With `remember = false` it holds one name.
- The Tools menu with `[wallpaper]` set: no syscall per open, the anonymous memory within noise
  of main. The Wallpaper level on 2,000 images: about +0.8 to +0.95 MB anonymous while the names
  show, back within +20 to +140 KB once closed, the reader thread gone.
