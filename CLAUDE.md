# alpheus — Claude Code instructions

Mae's "homemade" desktop shell, light and TUI-spirited, written in Rust.
Start by reading [`docs/SCOPE.md`](docs/SCOPE.md) (why, budget, guardrails,
breakdown), then the spec of the sub-project currently in progress in
`docs/specs/`.

## Where things stand

- Sub-project 1 (TUI control center, ratatui): **v1 spec written**
  ([`docs/specs/2026-10-02-alpheus-v1-design.md`](docs/specs/2026-10-02-alpheus-v1-design.md)),
  implementation plan ([`docs/plans/2026-10-02-alpheus-v1-plan.md`](docs/plans/2026-10-02-alpheus-v1-plan.md))
  **executed** (16 tasks + fixes from the final review, merged into `main`
  on 2026-10-03) and wired into nixos-config (`Mod+A`). Real-world trial
  validated by Mae on 2026-10-03 (Hyprland and niri).
- Sub-project 1 bis, **Displays** tab (Hyprland first): **spec written**
  ([`docs/specs/2026-10-03-alpheus-ecrans-design.md`](docs/specs/2026-10-03-alpheus-ecrans-design.md)),
  under review by Mae; plan to be written next. Keybindings: deferred.
- Fixtures and snapshots are **anonymized** (Wi-Fi, VPN, MAC, machine): never
  recommit a raw capture from the workstation, the repository is meant to
  go public.
- Sub-project 2 (resident daemon), first delivery **the bar**: **spec
  written** ([`docs/specs/2026-10-04-alpheus-demon-barre-design.md`](docs/specs/2026-10-04-alpheus-demon-barre-design.md)),
  under review by Mae. Vision: as much as DMS/Noctalia, within budget.
  Measured stack: smithay-client-toolkit + cosmic-text (≈ 4.5 MB).
- **English rename** (2026-10-04, Mae's decision): the whole repository
  moves to English — identifiers, comments, tests, log and error messages,
  user-visible strings (TUI and bar), config file names and keys, CLI
  flags, docs and commit messages. Only conversations with Mae stay in
  French. Code rename tracked on branch `chore/english`, docs translation
  on branch `docs/english`; see `.superpowers/sdd/english/` for the brief
  and glossary.
- **Portability** (after the bar, agreed with Mae on 2026-10-04): ship or
  make configurable the dependencies on nixos-config scripts (`wm-set`,
  `shell-ctl`, `tools-menu`, `alpheus-toggle`) with a neutral fallback;
  Dvorak-based keys disabled by default (enabled for Mae through her
  `config.toml`).
- Sub-project 3 (full NixOS integration): to be designed later.

## Rules

- **Language**: repository content (code, comments, docs, commit messages,
  UI) is in English; talk to Mae in French.
- **TDD**: test written first, red, then green; every test must prove it
  bites (temporarily break the code → red → restore).
- **Never a real call in tests** to tools that act on the session
  (`wpctl`, `nmcli`, `bluetoothctl`, `systemctl`, `wm-set`, `shell-ctl`…):
  injectable command executor.
- **Guardrails** (details in the scope doc): no sleep or `dpms` handling;
  no writes to the compositor outside `wm-set`; nothing resident.
- **Budget**: total resident memory ≤ 120 MB (sub-project 2); TUI < 100 ms
  to first render.
- **Shareable product, lightness first** (Mae, 2026-10-04): alpheus will be
  public. Plan for customization, but never at the expense of performance:
  an unused option costs nothing, no heavy dependency "for config". Don't
  hardcode what's specific to Mae's workstation (nixos-config scripts,
  Dvorak, paths) without a neutral default or a configurable path.
- The workstation runs NixOS, configured in `~/nixos-config` (flake):
  alpheus will be wired in there as a `git+file://` input — whatever isn't
  committed here isn't seen by the build.
