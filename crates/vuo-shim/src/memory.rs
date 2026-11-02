//! Handing sync's peak back to OS.
//!
//! Sync pass = largest thing Vuo allocates: page arrives compressed, inflated,
//! parsed into owned `String`s, written to mirror; scrape same for one body.
//! All freed when pass ends; on glibc freed ≠ returned.
//!
//! glibc returns freed block only at top of heap, and past `M_TRIM_THRESHOLD`.
//! Freed below a live allocation stays mapped: address space, pages, `ps` all
//! keep it. mmap threshold DYNAMIC (raised up to 32 MB each time large mmapped
//! block freed): after first passes big allocations stop being mmapped, come
//! from heap like rest.
//!
//! Result: resident size = worst pass, not working set; on phone that number
//! gets it killed. [`release_free_memory`] fixes it.

/// Return every free page glibc holds to kernel.
///
/// After work that peaked (sync pass, scrape), not on Qt thread: takes each
/// arena's lock in turn; brief, not free; sync worker can afford it.
///
/// No-op without glibc. `malloc_trim` = glibc extension; musl and others lack
/// it: compiles to nothing there, no link failure.
#[cfg(target_env = "gnu")]
pub fn release_free_memory() {
    // Only hand-written (not generated) `unsafe` in crate; see crate docs.
    // `malloc_trim` takes bytes to leave untrimmed at heap top, returns whether
    // it freed anything. Touches only allocator bookkeeping, no program-owned
    // memory, takes each arena lock, only failure mode = doing nothing: no
    // precondition to get wrong, no caller-observable state.
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(target_env = "gnu")]
extern "C" {
    /// `int malloc_trim(size_t pad)` from glibc's `malloc.h`.
    fn malloc_trim(pad: usize) -> std::os::raw::c_int;
}

#[cfg(not(target_env = "gnu"))]
pub fn release_free_memory() {}
