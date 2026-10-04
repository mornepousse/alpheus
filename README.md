# alpheus

> *Alpheus* : la crevette-pistolet, minuscule et la plus rapide de l'océan.

Un shell de bureau Wayland **léger, en esprit terminal**, écrit en Rust : ce
qu'apportent DankMaterialShell ou Noctalia (barre, réglages centralisés,
notifications, OSD), **sans leur poids**. Il vise une machine modeste avec
des fonctionnalités modernes.

| | Mémoire résidente |
|---|---|
| DankMaterialShell (Quickshell) | ≈ 530 Mo |
| **Barre alpheus, 3 écrans** | **≈ 13 Mo** (mesuré, stable) |
| Budget total d'alpheus (barre, notifications, OSD, fond) | ≤ 120 Mo |

Hyprland et niri. Souris et clavier partout. Aucune gestion de la veille ni
de l'extinction des écrans, par principe.

> **État** : projet personnel en cours, utilisé quotidiennement par son
> autrice sur NixOS. Il dépend encore de quelques scripts de sa
> configuration (`wm-set`, `shell-ctl`…) ; les rendre facultatifs, avec un
> repli neutre, est la prochaine étape avant un usage ailleurs.

## Ce que contient le dépôt

Un workspace Cargo de trois crates :

| Crate | Rôle |
|---|---|
| `alpheus` | **Centre de contrôle** en TUI (ratatui), ouvert à la demande |
| `alpheus-demon` | **Démon résident** : la barre, une par écran (layer-shell) |
| `alpheus-commun` | Socle partagé : exécution des commandes, configuration, thème, accès aux outils système |

La TUI n'embarque pas Wayland, le démon n'embarque pas ratatui : un test le
vérifie.

### Le centre de contrôle (`alpheus`)

Six onglets : **Accueil**, **Son**, **Réseau**, **Bluetooth**,
**Alimentation**, **Écrans** (activer, placer, régler la résolution et
l'échelle, profils, avec retour automatique au bout de 15 s si on ne
confirme pas).

| Touche | Action |
|---|---|
| `1`…`6`, `Tab` / `Maj+Tab` | changer d'onglet |
| `Ctrl+P` | palette de commandes (recherche floue) |
| `/`, `Ctrl+F` | chercher dans une liste |
| `F1`, `?` | aide |
| `Échap` | revenir ; `q` quitte |

`alpheus --onglet <nom>` ouvre directement un onglet (`accueil`, `son`,
`reseau`, `bluetooth`, `alimentation`, `ecrans`).

### La barre (`alpheus-demon`)

Une ligne de caractères monospace sur fond uni, modules séparés par `│`,
élément actif en couleur d'accent inversée.

Livré : **bureaux** (un point par fenêtre, bureau vide estompé, urgent en
rouge, clic pour y aller, molette pour passer au suivant), **fenêtre active**
(icône de l'appli et titre), **heure**. Une barre par écran, écrans branchés
et débranchés suivis à chaud. Sur un écran étroit, les modules secondaires
s'effacent ; les bureaux et l'heure restent.

À venir, dans l'ordre : son, réseau, Bluetooth, batterie, ressources ; puis
plateau système (icônes et menus des applis) ; puis lecture en cours et
notifications. Ensuite l'OSD, puis les bulles de notification.

La barre ne se réveille qu'à chaque minute et sur événement. Le compositeur
n'est que **lu** (flux d'événements Hyprland ou niri) ; changer de bureau
passe par une commande configurable.

## Construire

Avec Nix :

```sh
nix build .#alpheus          # la TUI
nix build .#alpheus-demon    # la barre
nix develop                  # shell de dev : cargo, polices, sway pour les tests
```

Sans Nix, `cargo build --release` suffit pour la TUI. La barre cherche ses
polices dans `$ALPHEUS_POLICES` (fixé à la compilation par le flake), sinon
dans celles du système.

## Configurer

Tout est facultatif, dans `~/.config/alpheus/`. Un fichier invalide donne
les valeurs par défaut et un avertissement, jamais un plantage.

- `colors.toml` : la palette, prévue pour être produite par un modèle
  [matugen](https://github.com/InioX/matugen).
- `config.toml` : la TUI (touches, outils ouverts pour le son, le réseau, le
  Bluetooth).
- `ecrans.toml` : les profils d'écrans, écrits par l'onglet Écrans.
- `barre.toml` : la barre.

```toml
# barre.toml — valeurs par défaut
modules = ["bureaux", "fenetre", "lecture", "volume", "reseau", "bluetooth",
           "batterie", "ressources", "plateau", "notifications", "heure"]
hauteur = 22

[police]
famille = "JetBrainsMono Nerd Font Mono"
taille = 14.0
repli = []          # familles de repli (emoji, CJK) : chacune coûte sa mémoire

[heure]
format = "%H:%M"
date = "%a %e %b"   # vide = pas de date

[bureaux]
commande = ["wm-set", "workspace", "{bureau}", "{sortie}"]
```

Les modules pas encore livrés sont acceptés et ignorés : un `barre.toml`
écrit aujourd'hui restera valable.

## Principes

- **La légèreté d'abord.** Une option inutilisée ne coûte rien ; pas de
  dépendance lourde « pour la config ». Les budgets sont vérifiés par des
  tests (mémoire et réveils au repos de la barre, mesurés sur un sway sans
  tête).
- **Jamais de `dpms`** ni de gestion de la veille : une extinction d'écran
  sur une sortie DisplayLink peut geler la machine.
- **Aucune écriture directe au compositeur** : seulement des commandes
  configurables.
- **Tests sans session graphique** : toutes les commandes passent par un
  exécuteur injectable, aucun test n'agit sur la session.

## Documentation

- [`docs/CADRAGE.md`](docs/CADRAGE.md) : pourquoi, budget, garde-fous,
  découpage.
- [`docs/specs/`](docs/specs/) : les spécifications (TUI, onglet Écrans,
  barre).
- [`docs/plans/`](docs/plans/) : les plans d'implémentation.

## Licence

[MIT](LICENSE) © 2026 Mae PUGIN
