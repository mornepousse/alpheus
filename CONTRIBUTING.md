# Contributing

Thank you for looking. alpheus is a personal project that aims to stay small
and light, so a change is judged on correctness first, then on what it costs
in memory, threads and wake-ups. Read [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
for the shape of the code and its guardrails.

## Ground rules

- **Language.** Code, comments, docs, commit messages and UI strings are in
  English.
- **TDD, and prove that tests bite.** Write the test first and see it fail for
  the right reason, then make it pass. Then break the code on purpose (a
  mutation: invert a condition, drop a call, change a constant), confirm the
  test goes red, and restore. A test that stays green under a mutation of the
  thing it covers is not testing it.
- **Never a real session tool in a test.** `wpctl`, `nmcli`, `bluetoothctl`,
  `systemctl`, `loginctl`, `wm-set`, `shell-ctl`, `brightnessctl`… can act on
  the machine running the tests. Everything goes through the injectable
  `Executor` (`alpheus-commun::exec`); tests use `FakeExecutor` and assert the
  exact command line. The same goes for sockets and buses: use the private
  `dbus-daemon`, the fake compositor sockets and the headless sway the existing
  tests set up, never your session's.
- **Fixtures are anonymised.** Captures from a real machine (Wi-Fi names, VPN,
  MAC addresses, hostnames, monitor models and serials, usernames) must be
  scrubbed before they are committed. Replace descriptions and identifiers by
  neutral ones (see the existing fixtures).
- **No writes to the compositor, no sleep or `dpms` handling, nothing resident
  while unused.** The guardrail tests will tell you if you cross a line; do not
  weaken one to make a change pass, discuss it first.
- **Lightness.** No new dependency without a reason. Prefer an existing crate
  in the tree. A new module must cost nothing when it is not in `modules`.

## Building and testing

Everything runs in the flake's dev shell, which provides the toolchain, the
system libraries (libdbus, libxkbcommon), fonts, sway, `wtype` and `grim`:

```sh
nix develop -c cargo test --workspace
nix develop -c cargo fmt --all            # formatting is enforced by `nix flake check`
nix develop -c cargo clippy --workspace
nix flake check                           # fmt check
```

Without Nix, install `pkg-config`, libdbus and libxkbcommon development files
(see [docs/install.md](docs/install.md)); the ignored tests also need sway,
`wtype`, `grim` and a `dbus-daemon`.

Some tests are `#[ignore]`d because they need a headless sway or a private bus.
Run them in the dev shell:

```sh
nix develop -c cargo test -p alpheus-demon --test headless_session -- --ignored --test-threads=1
```

### Budget (memory and wake-up) tests

`crates/alpheus-demon/tests/memory.rs` measures the real daemon's PSS, thread
count and wake-ups. Its budgets are tuned for the **release** binary (a debug
build inflates PSS and the test refuses to run):

```sh
nice -n 19 nix develop -c cargo test --release -p alpheus-demon --test memory -- --ignored --test-threads=1
```

To record what a change costs, build the commit before and after in separate
target directories, run the same test back to back several times (three is
the habit) and compare **deltas**, not absolute figures: the absolute PSS
drifts by roughly half a megabyte between boots because shared libraries are
counted proportionally. Put the result in the README's memory table.

### Machine load

The release builds and the headless tests are heavy. Run them at low priority
(`nice -n 19`), limit parallelism (`-j4`), and avoid compiling several things at
once on a machine that also drives your displays; a long parallel build can make
USB-docked screens reconnect.

## Documentation

- A user-visible feature updates the README (feature table, memory table when
  it costs anything) and the relevant page under `docs/`.
- A design lands as a spec in `docs/specs/` before the code, with a plan in
  `docs/plans/` for anything multi-step; see [`docs/specs/README.md`](docs/specs/README.md).
- Each module starts with a `//!` comment saying what it is and what it
  must never do; keep it true.
- How code is written and commented is in [`docs/STYLE.md`](docs/STYLE.md);
  settled decisions, which comments point to, are in
  [`docs/decisions.md`](docs/decisions.md).

## Commits

One logical change per commit, a subject in the imperative (`fix(quick): …`,
`feat(bar): …`, `docs: …`), a body that says why.

## Reporting problems

See [SECURITY.md](SECURITY.md) for security issues; for anything else, open an
issue with the compositor and its version, the daemon's log lines (standard
error, prefixed `alpheus-demon:`) and the relevant config file.
