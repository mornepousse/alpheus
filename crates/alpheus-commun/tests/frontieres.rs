//! Frontières du workspace, lues dans Cargo.lock : le socle n'embarque aucun
//! affichage, la TUI n'embarque pas Wayland (et, tâche 3, le démon
//! n'embarque pas ratatui).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Paquet → noms de ses dépendances directes, d'après Cargo.lock.
pub fn graphe() -> BTreeMap<String, BTreeSet<String>> {
    let chemin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let texte = std::fs::read_to_string(&chemin).unwrap_or_else(|e| panic!("{} : {e}", chemin.display()));
    let lock: toml::Table = toml::from_str(&texte).expect("Cargo.lock illisible");
    let mut g: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in lock["package"].as_array().expect("[[package]]") {
        let nom = p["name"].as_str().expect("name").to_string();
        let deps: Vec<String> = p
            .get("dependencies")
            .and_then(|d| d.as_array())
            .map(|d| d.iter().filter_map(|x| x.as_str()).map(|s| s.split(' ').next().unwrap_or(s).to_string()).collect())
            .unwrap_or_default();
        g.entry(nom).or_default().extend(deps);
    }
    g
}

/// Fermeture transitive des dépendances de `racine` (racine exclue).
pub fn fermeture(g: &BTreeMap<String, BTreeSet<String>>, racine: &str) -> BTreeSet<String> {
    assert!(g.contains_key(racine), "{racine} absent de Cargo.lock");
    let mut vus = BTreeSet::new();
    let mut pile = vec![racine.to_string()];
    while let Some(p) = pile.pop() {
        for d in g.get(&p).into_iter().flatten() {
            if vus.insert(d.clone()) {
                pile.push(d.clone());
            }
        }
    }
    vus
}

#[test]
fn le_socle_n_embarque_aucun_affichage() {
    let f = fermeture(&graphe(), "alpheus-commun");
    for interdit in ["ratatui", "crossterm", "wayland-client", "smithay-client-toolkit", "cosmic-text"] {
        assert!(!f.contains(interdit), "alpheus-commun tire {interdit}");
    }
}

#[test]
fn la_tui_n_embarque_pas_wayland() {
    let f = fermeture(&graphe(), "alpheus");
    assert!(f.contains("alpheus-commun") && f.contains("ratatui"), "{f:?}");
    for interdit in ["wayland-client", "smithay-client-toolkit", "cosmic-text"] {
        assert!(!f.contains(interdit), "la TUI tire {interdit}");
    }
}
