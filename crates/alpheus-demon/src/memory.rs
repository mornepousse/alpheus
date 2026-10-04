//! Memory handed back to the system after a one-off peak (fonts loaded, a
//! source's initial state parsed): the daemon's startup budget is 15 MB.

/// Returns freed memory to the system (glibc otherwise keeps the freed
/// pages for itself, in every thread's arena).
pub fn release_memory() {
    #[cfg(target_env = "gnu")]
    // SAFETY: malloc_trim(3) only returns free pages.
    unsafe {
        libc::malloc_trim(0);
    }
}
