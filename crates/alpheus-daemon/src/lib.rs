//! alpheus-daemon: the resident daemon (`alpheus-demon`) and the client that talks to it
//! (`alpheus-launch`).
//!
//! A pure state (`state::Bar`) changed by messages, sources that send them from their own threads,
//! and a grid of cells drawn by cosmic-text into one layer-shell surface per screen. Each feature
//! is a folder; the files beside this one are the core they share. The crate is a library so that
//! the tests can reach every part of it, and the two binaries are three lines each.
//!
//! See: docs/ARCHITECTURE.md (the codemap and the rules), docs/tour.md (one event from end to end),
//! docs/features/README.md (one page per feature), docs/SCOPE.md (sub-project 2).

pub mod actions;
pub mod appearance;
pub mod background;
pub mod backlight;
pub mod calendar;
pub mod captures;
pub mod clipboard;
pub mod clock;
pub mod compositor;
pub mod config;
pub mod deadline;
pub mod failed;
pub mod grid;
pub mod hooks;
pub mod icons;
pub mod inotify;
pub mod kbd_backlight;
pub mod keys;
pub mod launcher;
pub mod layout;
pub mod link;
pub mod list;
pub mod lock;
pub mod log;
pub mod media;
pub mod memory;
pub mod modules;
pub mod nonblocking;
pub mod notes;
pub mod notify;
pub mod osd;
pub mod outputs;
pub mod painter;
pub mod pipewire;
pub mod placement;
pub mod popup;
pub mod power;
pub mod privacy;
pub mod quick;
pub mod resources;
pub mod retired;
pub mod rfkill;
pub mod segments;
pub mod session;
pub mod session_bus;
pub mod shield;
pub mod sources;
pub mod startup;
pub mod state;
pub mod state_file;
pub mod sys;
pub mod system;
pub mod text;
pub mod timer;
pub mod tray;
pub mod wallpaper;
pub mod watch;
pub mod wayland;
