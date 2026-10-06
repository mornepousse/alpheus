//! alpheus-demon — resident daemon (sub-project 2 of the framing document,
//! spec docs/specs/2026-10-04-alpheus-demon-barre-design.md). A pure state
//! (`state::Bar`) changed by messages, sources that send them, a grid of
//! cells drawn by cosmic-text into a layer-shell surface per screen.

pub mod actions;
pub mod calendar;
pub mod clock;
pub mod compositor;
pub mod config;
pub mod deadline;
pub mod grid;
pub mod icons;
pub mod keys;
pub mod launch;
pub mod launcher;
pub mod layout;
pub mod list;
pub mod lock;
pub mod log;
pub mod memory;
pub mod nonblocking;
pub mod painter;
pub mod pipewire;
pub mod popup;
pub mod segments;
pub mod sources;
pub mod state;
pub mod system;
pub mod tray;
pub mod wayland;
