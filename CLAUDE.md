# alpheus — instructions Claude Code

Shell de bureau « maison » de Mae, léger et en esprit TUI, en Rust.
Commencer par lire [`docs/CADRAGE.md`](docs/CADRAGE.md) (pourquoi, budget,
garde-fous, découpage), puis la spec du sous-projet en cours dans
`docs/specs/`.

## Où on en est

- Sous-projet 1 (centre de contrôle TUI, ratatui) : **spec v1 écrite**
  ([`docs/specs/2026-10-02-alpheus-v1-design.md`](docs/specs/2026-10-02-alpheus-v1-design.md)),
  plan d'implémentation ([`docs/plans/2026-10-02-alpheus-v1-plan.md`](docs/plans/2026-10-02-alpheus-v1-plan.md))
  **exécuté** (16 tâches + corrections de la relecture finale, fusionné
  dans `main` le 2026-10-03) et branché dans nixos-config (`Mod+A`).
  Reste : essai réel par Mae après `nh os switch`.
- Prochaine étape demandée : onglets **Dispositions clavier** et **Écrans**
  (hors v1) — spec à écrire, avec Mae ; écrans via `wm-set` uniquement,
  jamais de `dpms`.
- Fixtures et instantanés **anonymisés** (Wi-Fi, VPN, MAC, machine) : ne
  jamais recommiter de relevé brut du poste, le dépôt est destiné à être
  public.
- Sous-projets 2 (démon résident) et 3 (intégration NixOS complète) : à
  concevoir plus tard.

## Règles

- **Langue** : Mae communique en français ; commentaires en français.
- **TDD** : test écrit d'abord, rouge, puis vert ; chaque test doit prouver
  qu'il mord (casser temporairement le code → rouge → rétablir).
- **Jamais d'appel réel en test** aux outils qui agissent sur la session
  (`wpctl`, `nmcli`, `bluetoothctl`, `systemctl`, `wm-set`, `shell-ctl`…) :
  exécuteur de commandes injectable.
- **Garde-fous** (détail dans le cadrage) : aucune gestion de veille ni de
  `dpms` ; aucune écriture au compositeur hors `wm-set` ; rien de résident.
- **Budget** : résident total ≤ 120 Mo (sous-projet 2) ; TUI < 100 ms au
  premier rendu.
- Le poste est NixOS, configuré dans `~/nixos-config` (flake) : alpheus y
  sera branché comme input `git+file://` — ce qui n'est pas commité ici
  n'est pas vu par le build.
