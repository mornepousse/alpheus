//! alpheus-commun — socle partagé par la TUI (`alpheus`) et le démon
//! résident (`alpheus-demon`) : exécuteur injectable, configuration, thème,
//! backends, écrans. Aucune dépendance d'affichage (ni ratatui, ni
//! Wayland) : vérifié par tests/frontieres.rs.

pub mod arret;
pub mod backends;
pub mod chemins;
pub mod config;
pub mod displays;
pub mod exec;
pub mod theme;
