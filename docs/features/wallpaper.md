# The wallpaper picker

Pick a wallpaper from one folder, by name, from the Tools menu. alpheus never draws, scales or
decodes the image: it starts your own setter (swaybg, wbg, swww) with the file, optionally runs a
command after it (matugen, so the bar's colours follow), and starts it again with the last choice
when the daemon starts.

This page grows with the feature: the setter and the Tools menu entry are built; the re-apply at
start and the `--wallpaper next` verb follow (`docs/superpowers/plans/2026-11-27-wallpaper.md`).

## Using it

Set `[wallpaper]` (a folder and your setter's command). Super+X shows **Wallpaper** in Utilities
(no letter of its own: `[tools] keys = { "wallpaper.pick" = "y" }` gives it one); type `/wall` to find it.
Enter lists the folder's images by name, sorted (png, jpg, jpeg, webp, bmp, gif; at most 2,000);
type to filter, Enter applies the one selected, after the box closed. The history keeps that you
used the tool, never the file's name.

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
- `wallpaper/app.rs`: the glue on `App`. `wallpaper_apply` runs on the event loop's thread, so the
  setter's PDEATHSIG is the daemon's own: the setter dies with the daemon. Each setter's pidfd is a
  calloop `Generic` source, silent until it ends. Logs are fixed words: `wallpaper: applied`,
  `wallpaper: setter could not start`, `wallpaper: setter failed`, `wallpaper: setter ended`,
  `wallpaper: not saved`, `wallpaper: then could not start`.

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
- `a_restart_drops_only_the_old_handle_after_the_new_one_started`,
  `a_one_shot_setter_succeeds_only_on_exit_zero`,
  `a_failed_set_runs_no_then_and_keeps_the_remembered_name`,
  `then_runs_after_a_successful_set_with_the_file`, `a_name_not_in_the_folder_is_not_started`,
  `the_wallpaper_file_round_trips_and_refuses_a_path`.

## What it costs

- Off: nothing. The `Setter` on `App` is two empty options (the figure is in
  [../measurements.md](../measurements.md)); nothing is read or started.
- A setter running: its handle, one pidfd and one calloop source, no wake-up until it ends. The
  setter itself is the user's program.
- The Tools menu with `[wallpaper]` set: no syscall per open, the anonymous memory within noise
  of main. The Wallpaper level on 2,000 images: about +0.8 to +0.95 MB anonymous while the names
  show, back within +20 to +140 KB once closed, the reader thread gone (figures in
  [../measurements.md](../measurements.md)).
