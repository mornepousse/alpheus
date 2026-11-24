# Contributing

Thank you for looking. alpheus is a small, light desktop shell, and a change
is judged on correctness first, then on what it costs in memory, threads and
wake-ups. This page holds every rule a contributor follows, each stated once.
For the shape of the code read [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md);
for the words, [`docs/GLOSSARY.md`](docs/GLOSSARY.md).

## What is accepted

- **Lightness first.** A feature that is not enabled costs nothing: no thread,
  timer, wake-up, allocation or file read. A new module is absent from the
  process unless it is in `modules`.
- **No heavy dependency for configuration or convenience.** A new crate needs a
  reason written in the pull request and a measured cost. Prefer a crate
  already in the tree. The tree has no async runtime and must not gain one.
- **Personal setups stay out of the code.** Nothing specific to one machine or
  one distribution's scripts is hard-coded; every command is configurable and
  has a neutral default.
- **Guardrails hold.** alpheus handles no sleep or `dpms`, changes the
  compositor only through configurable commands, with two exceptions, each in
  one file (`zwlr_output_configuration_v1` in `outputs/wlr.rs` and the
  clipboard's own selection in `clipboard/control.rs`), never manages output
  power, and keeps nothing resident while unused. The guardrail tests are
  `crates/alpheus-daemon/tests/guardrails.rs`,
  `crates/alpheus/tests/guardrails.rs` and
  `crates/alpheus-common/tests/boundaries.rs`. A change leaves them untouched
  or makes them stricter; it never loosens one to pass. If a guardrail looks
  wrong, open an issue first.
- **Settled decisions** are in [`docs/decisions.md`](docs/decisions.md). A
  change that reverses one says so and says why.

## The rules

- **English only.** Code, comments, docs, commit messages, logs and UI strings.
- **Tests first, and tests that bite.** Write the test, run it, and see it fail
  for the right reason. Make it pass with the least code. Then mutate the code
  on purpose (invert a condition, drop a call, change a constant), see the test
  go red, and restore. A test that stays green under a mutation of the thing
  it covers is not testing it; say so in the pull request and add the smallest
  test that bites.
- **Never a real session tool in a test.** These tools act on the machine that
  runs the tests, so a test never starts one for real: `wpctl`, `nmcli`,
  `bluetoothctl`, `rfkill`, `systemctl`, `journalctl`, `coredumpctl`,
  `systemd-analyze`, `loginctl`, `systemd-run`, `brightnessctl`,
  `powerprofilesctl`, `pw-dump`, `usbguard`, `pkexec`, `wm-set`, `shell-ctl`,
  `wl-copy` and `xdg-open`, and any command a user configures. Everything goes
  through the injectable `Executor` (`alpheus-common::exec`); tests use
  `FakeExecutor`, which answers "not found" to any command it was not
  programmed for, and assert the exact command line.
- **Fakes and private buses only.** Tests never touch the real session or
  system D-Bus, the real compositor, or the real user's files. Use the fake
  bus, a private `dbus-daemon`, the fake compositor sockets, temporary
  directories, or the headless harness in `crates/alpheus-daemon/tests/common`
  (a disposable sway with a cleared environment and its own runtime
  directory). `wtype`, `grim` and `wl-copy` are used there only against that
  sway.
- **Anonymised fixtures.** A capture from a real machine is scrubbed before it
  is committed: Wi-Fi names, VPN names, MAC and IP addresses, device names and
  serials, host names, user names, monitor models. Use neutral invented values.
  A raw capture is never committed.
- **Budgets.** Total resident memory of everything alpheus runs stays at or
  below 120 MB; the control center's first render stays under 100 ms; the
  daemon makes no wake-up at rest apart from planned timers. Anything that
  becomes resident (a thread, a timer, a connection, a cache) is measured by
  the release memory suite, which refuses debug builds:

  ```sh
  nix develop -c cargo test -q --release -p alpheus-daemon --test memory -- --include-ignored --test-threads=1
  ```

  Build the commit before and after in separate target directories, run the
  same test back to back (three times is the habit) and compare deltas, never
  absolute figures: PSS drifts by about half a megabyte between boots. Leak
  bounds read anonymous memory. Record the result in
  [`docs/measurements.md`](docs/measurements.md), which also gives the method.
- **One logical change per commit**, with a subject that states
  what the change does as a short statement, behind a `type(scope):` prefix
  (`docs: the bar page in the house layout`, `fix(tui): Dvorak navigation keys
  off by default`), and a body that says why. A
  move or rename is its own commit and changes nothing else.
- **Style.** [`docs/STYLE.md`](docs/STYLE.md) says how code and comments are
  written: module headers, names, early returns, no `unwrap` outside a
  documented invariant, a reason on every `unsafe`.

## Commands

Everything runs in the flake's dev shell, which provides the toolchain,
libdbus, libxkbcommon, fonts, sway, `wtype`, `grim` and `wl-clipboard`:

```sh
nix develop -c cargo fmt --all --check
nix develop -c cargo clippy --workspace --all-targets -- -D warnings
nix develop -c cargo test --workspace
nix develop -c cargo test -q -p alpheus-daemon --test headless_session -- --include-ignored
nix flake check
```

Format only the files you touched. Without Nix, install `pkg-config`, the
libdbus and libxkbcommon development files ([`docs/install.md`](docs/install.md)),
and a recent stable Rust.

The headless suite starts a disposable sway, so it needs `sway`, `wtype`,
`grim`, `wl-copy` and a `dbus-daemon` on `PATH`, and a font folder in
`ALPHEUS_POLICES` (the dev shell sets it). It never uses your session. The
memory suite and the headless suite are heavy: run them at low priority
(`nice -n 19`) and avoid long parallel builds on a machine that drives your
displays.

## Documentation

- A user-visible feature updates [`docs/configuration.md`](docs/configuration.md)
  for its keys, its page under [`docs/features/`](docs/features/),
  [`docs/comparison.md`](docs/comparison.md), and
  [`docs/measurements.md`](docs/measurements.md) when it costs anything.
- A new module follows [`docs/adding-a-module.md`](docs/adding-a-module.md).
- A design of a delivery's size lands as a spec in `docs/specs/` before the code
  ([`docs/specs/README.md`](docs/specs/README.md)). The specs already there are the records of
  past deliveries, not the authority on what ships today: that is the code,
  [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and
  [`docs/decisions.md`](docs/decisions.md).
- Each module's `//!` header says what it is and what it never does; keep it
  true.

## AI-assisted contributions

Contributions made with an AI tool are welcome on the same terms as any other.

- Say which tool you used, in the pull request.
- Understand every line, and be able to explain it and its tests.
- Keep the change narrow and tested, with the checks above run locally.
- No AI-generated images or other media.
- Low-effort generated pull requests or issues are closed.

## Reporting problems

For a security issue see [SECURITY.md](SECURITY.md). For anything else, open an
issue with the compositor and its version, the daemon's log lines (standard
error, prefixed `alpheus-demon:`) and the relevant config file.
