//! Pure parsers and validators for the Diag tab: what `systemctl`, `journalctl`, `coredumpctl` and
//! `systemd-analyze` print, read into small typed values. Nothing here launches a tool or opens a
//! file (the backend does, through the executor); every string that came from outside passes
//! `text::clean_line` before it is kept.
//!
//! Never:
//! - launch a tool or open a file: the backend does that, and these only read what it printed;
//! - keep a field the view does not show: what journald or `coredumpctl` adds is dropped at the
//!   parse, so a secret in the environment or a command line is never carried;
//! - trust a text: everything from outside passes `text::clean_line` before it is kept.
//!
//! See: docs/features/control-center.md ("Diag"), docs/decisions.md (the Diag entries),
//! AGENTS.md ("No private data in fixtures or logs").

pub mod analyze;
pub mod crashes;
pub mod env;
pub mod journal;
pub mod names;
pub mod query;
pub mod report;
pub mod show;
pub mod units;
