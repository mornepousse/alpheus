//! `release_memory`: the pages of a one-off peak handed back to the system.
//!
//! glibc keeps freed pages in each thread's arena, so a peak — the fonts loaded at start, a
//! source's first state parsed, a popup's pool — would sit in the daemon's figures for ever. This
//! is the one call that returns them, made where a peak is known to be over. The startup budget it
//! serves is 15 MB of PSS, firm, measured by the memory suite.
//!
//! Never:
//! - free anything the daemon still owns: `malloc_trim(3)` returns pages that are already free,
//!   which is what the `unsafe` block says;
//! - assume the allocator: the call is compiled in for the GNU target only, and everything else
//!   simply keeps its pages.
//!
//! See: docs/measurements.md (the budgets and the method), docs/SCOPE.md (why the figures matter).

/// Returns freed memory to the system (glibc otherwise keeps the freed
/// pages for itself, in every thread's arena).
pub fn release_memory() {
    #[cfg(target_env = "gnu")]
    // SAFETY: malloc_trim(3) only returns free pages.
    unsafe {
        libc::malloc_trim(0);
    }
}
