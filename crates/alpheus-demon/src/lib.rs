//! alpheus-demon — resident daemon (sub-project 2 of the framing document,
//! spec docs/specs/2026-10-04-alpheus-demon-barre-design.md). A pure state
//! (`state::Bar`) changed by messages, sources that send them, a grid of
//! cells drawn by cosmic-text into a layer-shell surface per screen.

pub mod actions;
pub mod appearance;
pub mod background;
pub mod backlight;
pub mod bubbles;
pub mod bubbles_surface;
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
pub mod link;
pub mod list;
pub mod lock;
pub mod log;
pub mod media;
pub mod media_popup;
pub mod memory;
pub mod nonblocking;
pub mod notify;
pub mod notify_app;
pub mod osd;
pub mod osd_surface;
pub mod painter;
pub mod panel;
pub mod pipewire;
pub mod placement;
pub mod popup;
pub mod retired;
pub mod segments;
pub mod sources;
pub mod state;
pub mod state_file;
pub mod system;
pub mod text;
pub mod tray;
pub mod watch;
pub mod wayland;
