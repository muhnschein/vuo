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

#[cfg(test)]
mod tests {
    use super::*;

    /// Resident set size in bytes, or `None` where there is no procfs.
    fn resident_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }

    /// §the freed peak actually goes back, rather than staying resident.
    ///
    /// Shape that matters: many heap-sized blocks freed while something
    /// ALLOCATED LATER lives above. `free` trims only heap top; live block pins
    /// everything below, however free: exactly what sync pass leaves, what
    /// allocator won't undo alone.
    ///
    /// glibc only (call is glibc's). Skipped, not failed, elsewhere and where
    /// procfs absent.
    #[test]
    #[cfg(target_env = "gnu")]
    fn a_freed_peak_is_returned_to_the_operating_system() {
        const BLOCK: usize = 16 * 1024;
        const BLOCKS: usize = 4096;

        let Some(baseline) = resident_bytes() else {
            return;
        };

        // Touched, not just reserved: untouched page not resident; test would
        // pass measuring nothing.
        let mut blocks: Vec<Vec<u8>> = (0..BLOCKS).map(|_| vec![1u8; BLOCK]).collect();
        let peak = resident_bytes().unwrap_or(baseline);
        assert!(
            peak > baseline + (BLOCK * BLOCKS / 2) as u64,
            "the fixture did not actually become resident: {baseline} -> {peak}"
        );

        // Last block stays alive, everything under goes. The pin: without it
        // `free` walks heap top down itself, nothing left for call under test.
        let pin = blocks.pop();
        blocks.clear();
        blocks.shrink_to_fit();

        let freed = resident_bytes().unwrap_or(baseline);
        release_free_memory();
        let trimmed = resident_bytes().unwrap_or(baseline);
        drop(pin);

        let returned = freed.saturating_sub(trimmed);
        assert!(
            returned > (BLOCK * BLOCKS / 2) as u64,
            "freeing left {freed} bytes resident and the trim returned only {returned} of them"
        );
    }
}
