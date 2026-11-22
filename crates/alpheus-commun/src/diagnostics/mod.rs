//! Pure parsers and validators for the Diag tab: what `systemctl`, `journalctl`, `coredumpctl` and
//! `systemd-analyze` print, read into small typed values. Nothing here launches a tool or opens a
//! file (the backend does, through the executor); every string that came from outside passes
//! `text::clean_line` before it is kept.

pub mod analyze;
pub mod crashes;
pub mod env;
pub mod journal;
pub mod names;
pub mod query;
pub mod report;
pub mod show;
pub mod units;
