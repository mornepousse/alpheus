# alpheus: instructions for coding agents

alpheus is a light Wayland desktop shell written in Rust: a layer-shell bar daemon with a launcher,
notifications, an on-screen display and quick settings, and a terminal control center. It is a
"potato machine with modern features" project: every feature that is not enabled must cost nothing.

Read [`docs/SCOPE.md`](docs/SCOPE.md) (why, budget, guardrails), then
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and [`docs/GLOSSARY.md`](docs/GLOSSARY.md). The rules
every contributor follows are in [`CONTRIBUTING.md`](CONTRIBUTING.md); this file adds the ones that
only an agent needs. Where the two say the same thing, CONTRIBUTING.md is the text.

## The code

- `crates/alpheus-common`: the executor (`exec`), configuration, theme, compositor detection,
  backends shared by the other two crates. No UI, no Wayland.
- `crates/alpheus`: the control center, a ratatui program (`alpheus`).
- `crates/alpheus-daemon`: the resident bar (`alpheus-demon`) and `alpheus-launch`: layer-shell
  through smithay-client-toolkit, cosmic-text, calloop; sources on their own threads feed one
  `Mail` channel; state, then grid, then painter.

The map of modules and the walk through one event are in `docs/ARCHITECTURE.md` and `docs/tour.md`.

## Commands

Every cargo command runs in the flake's dev shell: `nix develop -c ...`.

```sh
nix develop -c cargo fmt --all --check            # format only the files you touched
nix develop -c cargo clippy --workspace --all-targets -- -D warnings
nix develop -c cargo test --workspace
nix develop -c cargo test -q -p alpheus-daemon --test headless_session -- --include-ignored
nix develop -c cargo test -q --release -p alpheus-daemon --test memory -- --include-ignored --test-threads=1
nix build .#alpheus-demon .#alpheus .#alpheus-launch
nix flake check
```

- The headless suite starts a disposable sway through `crates/alpheus-daemon/tests/common`; the
  memory suite refuses debug builds. Both are heavy: run them at `nice -n 19`.
- A total-PSS test of the memory suite that fails once is rerun alone, three times, with
  `--exact`; only a failure that repeats is a regression. Leak bounds read anonymous memory.
- Measure a cost back to back against a base commit built in its own target directory
  (`docs/measurements.md` gives the method); trust deltas, not absolute figures.

## Rules for agents

The contributor rules (tests first and mutated to prove they bite, fakes and private buses,
anonymised fixtures, budgets, one change per commit, English) are in `CONTRIBUTING.md`. Beyond them:

- **Never call a session tool for real.** `wpctl`, `nmcli`, `bluetoothctl`, `systemctl`,
  `loginctl`, `hyprctl`, `wm-set`, `shell-ctl`, `wl-copy`, `xdg-open`, `pkexec` and the rest of
  `CONTRIBUTING.md`'s list act on the machine you run on. Use `FakeExecutor`; never a state-changing
  command (`wpctl set-*`, `nmcli con up`, `bluetoothctl connect`, `dpms`).
- **Never touch the user's running daemon or session.** One `alpheus-demon` runs there: do not run
  `alpheus` or `alpheus-demon` on the session, connect to it, read its `/proc`, signal or kill it.
  Headless tests only through the isolated harness (cleared environment, temporary
  `XDG_RUNTIME_DIR`, own process group); afterwards check with `pgrep` that nothing of yours remains.
- **No `pkill`, `killall` or pattern kills.** Kill only a PID you started, by that PID.
- **Never a real system or session bus in a test.** Fake bus, or a private `dbus-daemon` in an
  `#[ignore]`d isolated test.
- **Never commit `flake.lock`** (except an input bump the owner approved), nor the owner's private notes, nor
  the user's local files (`mpv.conf`, `.tripwire-testcount`, editor or tool state). Stage the files
  of the task by name and read `git diff --cached` before every commit.
- **Never edit the user's system configuration** (the NixOS configuration repository, a user's
  `bar.toml`, `launcher.toml` or `theme.toml`, the compositor's config). A change needed there is
  written as a note for the owner, not made.
- **No private data in fixtures or logs.** A log line is a fixed word: it never carries an SSID,
  a password, a command or argument list, an app, device or screen name, an address or object path,
  a VPN profile name, the launcher's query text, note or clipboard text, a MIME list, a panic
  payload, a unit name, a USB device name or id, or a flag file's path. Each feature's guardrail
  test in `crates/alpheus-daemon/tests/guardrails.rs` pins its own files; a new log line follows it.
- **Merge and push only on the owner's explicit go.** Commit on a branch, never on `main`, never
  switch branches under the owner's feet. A tag or a push is the owner's call.
- **Worktrees** live in `.worktrees/<name>` inside the repository, never as sibling folders, and
  are removed after the merge. Agents working in parallel each use their own `mktemp -d` for
  scratch files and check `git diff --cached` before committing.
- **Never loosen a guardrail test to make a change pass.** Open an issue or ask the owner.
- **Prior art first.** Before designing a feature, look at how other bars and shells do it (ironbar,
  ashell, waybar, i3status-rust, cosmic-applets...): reuse ideas and signal lists, not heavy
  dependencies.
- **Consult the owner on product-visible choices** a spec or plan does not settle: stop and ask,
  with a recommendation. Purely technical choices are yours; say which you made.
- **Lightness first.** No new dependency, thread, timer or polling without a written reason and a
  measurement.

## Commits

- One logical change per commit, subject `type(scope): what` in the imperative, a body that says
  why (`CONTRIBUTING.md`).
- End the message with the attribution lines the session gives you, as separate trailers
  (`Co-Authored-By: Claude <model> <noreply@anthropic.com>`, then the session line).
- A move or rename is its own commit.

## Language

Everything in the repository is English: code, comments, docs, commit messages, logs, UI strings.
The conversation with the owner is in French.

## Where to look

- [`docs/STYLE.md`](docs/STYLE.md): how code and comments are written.
- [`docs/decisions.md`](docs/decisions.md): settled decisions and why; code comments point there.
- [`docs/features/`](docs/features/README.md): one page per feature (what, use, configuration,
  construction, the rules its tests enforce, cost).
- [`docs/adding-a-module.md`](docs/adding-a-module.md): the recipe for a bar module.
- [`docs/configuration.md`](docs/configuration.md): every key and default (from `config.rs`).
- [`docs/cli.md`](docs/cli.md), [`docs/compositors.md`](docs/compositors.md),
  [`docs/measurements.md`](docs/measurements.md), [`CHANGELOG.md`](CHANGELOG.md).
- `docs/specs/`: historical design records; the code, `docs/ARCHITECTURE.md` and
  `docs/decisions.md` are authoritative.
