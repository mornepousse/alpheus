//! alpheus-demon — démon résident (sous-projet 2 du cadrage, spec
//! docs/specs/2026-10-04-alpheus-demon-barre-design.md). Un état pur
//! (`etat::Barre`) modifié par des messages, des sources qui les envoient,
//! une grille de cellules dessinée par cosmic-text dans une surface
//! layer-shell par écran.

pub mod config;
pub mod heure;
