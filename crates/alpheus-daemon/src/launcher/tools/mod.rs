//! The Tools menu (spec docs/specs/2026-11-21-alpheus-tools-menu-design.md): built-in and
//! user entries behind `alpheus-launch --tools`. Pure parts here; the glue is `app.rs`, in this folder.

pub mod app;
pub mod catalog;
pub mod config;
pub mod join;
pub mod nav;
pub mod read;
